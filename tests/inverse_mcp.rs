//! Exercise the released stdio interface against local HTTP/Unix visor peers.
use base64::Engine;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};

const SESSION: &str = "00000000-0000-0000-0000-000000000001";

// A rejected valid request should fail this test, not leave accept blocked.
macro_rules! accept_bounded {
    ($listener:expr) => {{
        $listener.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match $listener.accept() {
                Ok(pair) => break pair,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "adapter did not connect"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("accept: {error}"),
            }
        }
    }};
}

fn run(endpoint: &str, unix: bool, session: &str, requests: &[Value]) -> Vec<Value> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hypermesh-desktop-mcp"));
    command
        .env("HYPERMESH_SESSION_ID", session)
        .env_remove("HYPERMESH_VISOR_TOKEN")
        .env_remove("HYPERMESH_VISOR_URL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if unix {
        command.env("HYPERMESH_RUNTIME_DIR", endpoint);
    } else {
        command
            .env("HYPERMESH_VISOR_URL", endpoint)
            .env("HYPERMESH_VISOR_TOKEN", "test-bearer");
    }
    let mut child = command.spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    for request in requests {
        writeln!(input, "{request}").unwrap();
    }
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}

fn call(tool: &str, args: Value) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}})
}

fn png() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(include_str!("fixtures/view.png.b64").trim())
        .unwrap()
}

fn exchange(
    mut stream: impl Read + Write,
    status: u16,
    mime: &str,
    body: &[u8],
    chunked: bool,
) -> String {
    let mut request = Vec::new();
    let mut byte = [0];
    while !request.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        request.push(byte[0]);
    }
    let header = String::from_utf8(request).unwrap();
    let length = header
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload).unwrap();
    write!(
        stream,
        "HTTP/1.1 {status} Response\r\nContent-Type: {mime}\r\n"
    )
    .unwrap();
    if chunked {
        write!(stream, "Transfer-Encoding: chunked\r\n\r\n").unwrap();
        for chunk in body.chunks(7) {
            write!(stream, "{:x}\r\n", chunk.len()).unwrap();
            stream.write_all(chunk).unwrap();
            stream.write_all(b"\r\n").unwrap();
        }
        stream.write_all(b"0\r\n\r\n").unwrap();
    } else {
        write!(stream, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        stream.write_all(body).unwrap();
    }
    stream.flush().unwrap();
    // Keep the connection open: a framed response must not wait for EOF.
    let mut rest = Vec::new();
    stream.read_to_end(&mut rest).unwrap();
    format!("{header}{}", String::from_utf8_lossy(&payload))
}

fn roundtrip(
    unix: bool,
    status: u16,
    mime: &'static str,
    body: Vec<u8>,
    chunked: bool,
    request: Value,
) -> (Value, String) {
    let directory = std::env::temp_dir().join(format!("hm-inverse-{}", uuid::Uuid::new_v4()));
    let (result, wire) = if unix {
        std::fs::create_dir(&directory).unwrap();
        let listener = UnixListener::bind(directory.join("control.sock")).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = accept_bounded!(listener);
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            exchange(stream, status, mime, &body, chunked)
        });
        let responses = run(directory.to_str().unwrap(), true, SESSION, &[request]);
        (
            responses.into_iter().next().unwrap(),
            server.join().unwrap(),
        )
    } else {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (stream, _) = accept_bounded!(listener);
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            exchange(stream, status, mime, &body, chunked)
        });
        let responses = run(&endpoint, false, SESSION, &[request]);
        (
            responses.into_iter().next().unwrap(),
            server.join().unwrap(),
        )
    };
    if unix {
        std::fs::remove_dir_all(directory).unwrap();
    }
    (result, wire)
}

#[test]
fn screenshots_preserve_png_bytes_over_both_transports_and_framings() {
    for unix in [false, true] {
        for chunked in [false, true] {
            let expected = png();
            let (response, wire) = roundtrip(
                unix,
                200,
                "image/png",
                expected.clone(),
                chunked,
                call("view", json!({})),
            );
            assert_eq!(response["id"], 1);
            assert_eq!(response["result"]["isError"], false);
            let image = &response["result"]["content"][0];
            assert_eq!(image["type"], "image");
            assert_eq!(image["mimeType"], "image/png");
            assert_eq!(
                base64::engine::general_purpose::STANDARD
                    .decode(image["data"].as_str().unwrap())
                    .unwrap(),
                expected
            );
            assert!(wire.starts_with(&format!("POST /session/{SESSION}/view HTTP/1.1")));
            if !unix {
                assert!(wire
                    .to_lowercase()
                    .contains("authorization: bearer test-bearer"));
            }
        }
    }
}

#[test]
fn invalid_arguments_never_reach_the_transport() {
    let mut requests = vec![];
    for args in [
        json!({}),
        json!({"x":null,"y":1}),
        json!({"x":"1","y":1}),
        json!({"x":true,"y":1}),
        json!({"x":1.5,"y":1}),
        json!({"x":-1,"y":1}),
        json!({"x":32768,"y":1}),
        json!({"x":1,"y":18446744073709551615u64}),
        json!([]),
        json!(null),
    ] {
        requests.push(call("mouse_move", args));
    }
    for button in [json!(null), json!(1), json!("wheel"), json!("Left")] {
        requests.push(call("mouse_click", json!({"x":1,"y":1,"button":button})));
    }
    for text in [json!(null), json!(1), json!(""), json!("a".repeat(4097))] {
        requests.push(call("type_text", json!({"text":text})));
    }
    requests.extend([
        call("type_text", json!({})),
        call("view", json!({"extra":true})),
        call("resume", json!({})),
        call("read_file", json!({})),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":null}),
    ]);
    // This unbound endpoint turns any accidental connection into a tool error,
    // whereas every malformed request must return an invalid-params error.
    let responses = run("http://127.0.0.1:0", false, SESSION, &requests);
    assert_eq!(responses.len(), requests.len());
    for response in responses {
        assert_eq!(response["error"]["code"], -32602, "{response}");
    }
    for session in ["", "../companion", "bad\r\nheader"] {
        let responses = run(
            "http://127.0.0.1:0",
            false,
            session,
            &[call("view", json!({}))],
        );
        assert_eq!(responses[0]["error"]["code"], -32602);
    }
}

#[test]
fn valid_arguments_and_tool_failures_preserve_semantics() {
    for unix in [false, true] {
        for (tool, args) in [
            ("mouse_move", json!({"x":32767,"y":0})),
            ("mouse_click", json!({"x":1,"y":2,"button":"right"})),
            ("mouse_click", json!({"x":1,"y":2,"button":"middle"})),
            ("type_text", json!({"text":"hello\nworld"})),
        ] {
            let (response, _) = roundtrip(
                unix,
                204,
                "application/json",
                vec![],
                false,
                call(tool, args),
            );
            assert_eq!(response["result"]["isError"], false);
        }
        for status in [401, 403, 404, 501] {
            let (response, _) = roundtrip(
                unix,
                status,
                "application/json",
                br#"{"error":"denied"}"#.to_vec(),
                false,
                call("view", json!({})),
            );
            assert_eq!(response["result"]["isError"], true);
            assert!(response["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains(&status.to_string()));
        }
        let (response, wire) = roundtrip(
            unix,
            204,
            "application/json",
            vec![],
            false,
            call("mouse_click", json!({"x":0,"y":32767})),
        );
        assert_eq!(response["result"]["isError"], false);
        assert!(wire.contains("\"button\":\"left\""));
        let (response, _) = roundtrip(
            unix,
            409,
            "application/json",
            br#"{"error":"automation paused after 32 keystrokes"}"#.to_vec(),
            true,
            call("type_text", json!({"text":"hello"})),
        );
        assert_eq!(response["result"]["isError"], true);
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("409"));
        assert!(text.contains("after 32 keystrokes"));
        for (mime, data) in [("text/html", png()), ("image/png", b"not a PNG".to_vec())] {
            let (response, _) = roundtrip(unix, 200, mime, data, false, call("view", json!({})));
            assert_eq!(response["result"]["isError"], true);
        }
    }
}

#[test]
fn tool_notifications_cannot_execute_and_schema_matches_validation() {
    let messages = [
        json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"view"}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    ];
    let responses = run("http://127.0.0.1:0", false, SESSION, &messages);
    assert_eq!(responses.len(), 1);
    let tools = responses[0]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 4);
    assert_eq!(tools[1]["inputSchema"]["properties"]["x"]["maximum"], 32767);
    assert_eq!(
        tools[2]["inputSchema"]["properties"]["button"]["enum"],
        json!(["left", "right", "middle"])
    );
    assert_eq!(
        tools[3]["inputSchema"]["properties"]["text"]["maxLength"],
        4096
    );
    assert!(tools
        .iter()
        .all(|tool| tool["inputSchema"]["additionalProperties"] == false));
}
