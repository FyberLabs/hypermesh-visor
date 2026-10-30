//! HTTP / SSE MCP attach (JSON-RPC over POST).
//! Streamable HTTP subset: initialize → tools/list → tools/call.

use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

use serde_json::{json, Value};

use super::config::ServerSpec;
use super::stdio::ToolInfo;
use super::McpError;

pub struct HttpClient {
    url: String,
    headers: BTreeMap<String, String>,
    next_id: u64,
}

impl HttpClient {
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let response = post_json(&self.url, &self.headers, &body)?;
        if response.get("id") != Some(&json!(id)) || response.get("jsonrpc") != Some(&json!("2.0"))
        {
            return Err(McpError::message("MCP HTTP response id/version mismatch"));
        }
        Ok(response)
    }
}

pub fn handshake_http(
    name: &str,
    spec: &ServerSpec,
) -> Result<(Vec<ToolInfo>, HttpClient), McpError> {
    let url = spec
        .url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| McpError::message(format!("mcp server \"{name}\": http requires url")))?
        .to_string();
    let mut headers = spec.headers.clone();
    if !headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("content-type"))
    {
        headers.insert("Content-Type".into(), "application/json".into());
    }
    if !headers.keys().any(|k| k.eq_ignore_ascii_case("accept")) {
        headers.insert(
            "Accept".into(),
            "application/json, text/event-stream".into(),
        );
    }
    let mut client = HttpClient {
        url,
        headers,
        next_id: 1,
    };
    let init = client.call(
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "hypermesh-visor", "version": "0.1.0"}
        }),
    )?;
    if init.get("error").is_some() {
        return Err(McpError::message(format!(
            "mcp server \"{name}\": initialize failed"
        )));
    }
    let _ = post_json(
        &client.url,
        &client.headers,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    );
    let tools = super::discovery::list_tools(|params| client.call("tools/list", params))?;
    Ok((tools, client))
}

fn post_json(
    url: &str,
    headers: &BTreeMap<String, String>,
    body: &Value,
) -> Result<Value, McpError> {
    let raw = serde_json::to_string(body)
        .map_err(|e| McpError::message(format!("encode mcp http body: {e}")))?;
    let mut req = ureq::post(url);
    for (k, v) in headers {
        req = req.set(k, v);
    }
    let resp = req
        .timeout(Duration::from_secs(15))
        .send_string(&raw)
        .map_err(|_| McpError::message("MCP HTTP request failed"))?;
    let status = resp.status();
    let mut text = String::new();
    resp.into_reader()
        .take(8 * 1024 * 1024 + 1)
        .read_to_string(&mut text)
        .map_err(|e| McpError::message(format!("mcp http read: {e}")))?;
    if !(200..300).contains(&status) {
        return Err(McpError::message(format!(
            "mcp http status {status}: {}",
            truncate(&text, 200)
        )));
    }
    if text.len() > 8 * 1024 * 1024 {
        return Err(McpError::message("MCP HTTP response too large"));
    }
    let expected = body.get("id");
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    if let Ok(value) = serde_json::from_str::<Value>(text.trim()) {
        return Ok(value);
    }
    // Select the matching response, not a trailing list-changed notification.
    let normalized = text.replace("\r\n", "\n");
    for event in normalized.split("\n\n") {
        let payload = event
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(|s| s.strip_prefix(' ').unwrap_or(s))
            .collect::<Vec<_>>()
            .join("\n");
        if payload.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&payload)
            .map_err(|_| McpError::message("MCP invalid SSE JSON"))?;
        if expected.is_some() && value.get("id") == expected {
            return Ok(value);
        }
    }
    Err(McpError::message("MCP HTTP response missing matching id"))
}

fn truncate(s: &str, n: usize) -> String {
    let mut t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        t.push('…');
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn http_refresh_paginates_and_reconnects_without_replaying_lost_call() {
        use std::sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let done = Arc::clone(&stop);
        let called = Arc::clone(&calls);
        let worker = thread::spawn(move || {
            let mut generation = 0;
            while !done.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let raw = read_http_request(&mut stream);
                let req: Value =
                    serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap();
                let method = req["method"].as_str().unwrap();
                if method == "tools/call" {
                    called.fetch_add(1, Ordering::SeqCst);
                    continue;
                }
                let result = match method {
                    "initialize" => {
                        json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}}})
                    }
                    "tools/list" => {
                        if req["params"].get("cursor").is_none() {
                            generation += 1;
                            json!({"tools":[{"name":format!("{generation}-a")}],"nextCursor":" +/= "})
                        } else {
                            assert_eq!(req["params"]["cursor"], " +/= ");
                            json!({"tools":[{"name":format!("{generation}-b")}]})
                        }
                    }
                    _ => json!({}),
                };
                let body = if let Some(id) = req.get("id") {
                    // A response followed by a notification must still correlate.
                    format!("data: {}\n\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}}\n\n", json!({"jsonrpc":"2.0","id":id,"result":result}))
                } else {
                    String::new()
                };
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
            }
        });
        let spec = ServerSpec {
            r#type: Some("http".into()),
            command: None,
            args: vec![],
            env: BTreeMap::new(),
            url: Some(format!("http://{addr}/mcp")),
            headers: BTreeMap::new(),
            cwd: None,
            enabled: None,
        };
        let mut server = super::super::stdio::attach_one_server("http", &spec).unwrap();
        assert_eq!(server.tools[1].name, "1-b");
        server.refresh(false).unwrap();
        assert_eq!(server.tools[0].name, "2-a");
        assert!(server.call_tool("2-a", json!({})).is_err());
        assert!(server.tools.is_empty());
        assert!(!server.is_healthy());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.refresh(true).unwrap();
        assert_eq!(server.tools[1].name, "3-b");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        stop.store(true, Ordering::SeqCst);
        worker.join().unwrap();
    }

    #[test]
    fn attaches_http_fixture_and_lists_tools() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for stream in listener.incoming().take(3) {
                let mut stream = stream.unwrap();
                let req = read_http_request(&mut stream);
                let body = if req.contains("\"method\":\"tools/list\"")
                    || req.contains("\"method\": \"tools/list\"")
                {
                    r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"ping","description":"p"}]}}"#
                } else if req.contains("\"method\":\"initialize\"")
                    || req.contains("\"method\": \"initialize\"")
                {
                    r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"h","version":"0"}}}"#
                } else {
                    r#"{}"#
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        let spec = ServerSpec {
            r#type: Some("http".into()),
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            url: Some(format!("http://{addr}/mcp")),
            headers: BTreeMap::new(),
            cwd: None,
            enabled: None,
        };
        let (tools, _client) = handshake_http("remote", &spec).unwrap();
        assert_eq!(tools[0].name, "ping");
    }

    fn read_http_request(stream: &mut impl Read) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(pos) = find_headers_end(&buf) {
                let headers = String::from_utf8_lossy(&buf[..pos]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let lower = line.to_ascii_lowercase();
                        lower
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                let body_start = pos + 4;
                while buf.len() < body_start + content_length {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                break;
            }
            if buf.len() > 64 * 1024 {
                break;
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    fn find_headers_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n")
    }
}
