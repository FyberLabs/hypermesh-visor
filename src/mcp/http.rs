//! Streamable HTTP client. Legacy `sse` config is an alias, not HTTP+SSE discovery.
use super::{config::ServerSpec, stdio::ToolInfo, subscription::Changes, McpError};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::sync::{atomic::Ordering, Arc};
use std::time::Duration;

pub struct HttpClient {
    url: String,
    headers: BTreeMap<String, String>,
    next_id: u64,
    pub changes: Arc<Changes>,
    pub supports_tools: bool,
    pub listed_generation: u64,
}
impl Drop for HttpClient {
    fn drop(&mut self) {
        self.changes.stopped.store(true, Ordering::Release);
    }
}
impl HttpClient {
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        let response = post_json(
            &self.url,
            &mut self.headers,
            &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            &self.changes,
            method == "initialize",
        )?;
        if response.get("id") != Some(&json!(id)) || response["jsonrpc"] != "2.0" {
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
    // Connection-owned headers cannot be supplied or shadowed by configuration.
    headers.retain(|k, _| {
        ![
            "content-type",
            "accept",
            "mcp-session-id",
            "mcp-protocol-version",
            "last-event-id",
        ]
        .iter()
        .any(|h| k.eq_ignore_ascii_case(h))
    });
    headers.insert("Content-Type".into(), "application/json".into());
    headers.insert(
        "Accept".into(),
        "application/json, text/event-stream".into(),
    );
    let mut client = HttpClient {
        url,
        headers,
        next_id: 1,
        changes: Arc::new(Changes::default()),
        supports_tools: false,
        listed_generation: 0,
    };
    let init = client.call("initialize", json!({"protocolVersion":super::protocol::VERSION,"capabilities":{},"clientInfo":{"name":"hypermesh-visor","version":env!("CARGO_PKG_VERSION")}}))?;
    let negotiated = super::protocol::negotiate(&init)?;
    client.supports_tools = negotiated.tools;
    client
        .changes
        .enabled
        .store(negotiated.list_changed, Ordering::Release);
    client
        .headers
        .insert("MCP-Protocol-Version".into(), negotiated.version.clone());
    post_json(
        &client.url,
        &mut client.headers,
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        &client.changes,
        false,
    )?;
    // Subscribe before listing so changes during the initial catalog cannot be lost.
    if negotiated.list_changed && negotiated.version != "2024-11-05" {
        subscribe(
            client.url.clone(),
            client.headers.clone(),
            Arc::clone(&client.changes),
        )?;
    }
    let generation = client.changes.generation();
    let tools = if negotiated.tools {
        super::discovery::list_tools(|params| client.call("tools/list", params))?
    } else {
        Vec::new()
    };
    client.listed_generation = generation;
    Ok((tools, client))
}

fn agent() -> ureq::Agent {
    // Never forward configured credentials or a session ID across redirects.
    ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(Duration::from_secs(15))
        .build()
}
fn request(
    agent: &ureq::Agent,
    method: &str,
    url: &str,
    headers: &BTreeMap<String, String>,
) -> ureq::Request {
    let mut req = agent.request(method, url);
    for (k, v) in headers {
        req = req.set(k, v);
    }
    req
}
fn post_json(
    url: &str,
    headers: &mut BTreeMap<String, String>,
    body: &Value,
    changes: &Changes,
    initialize: bool,
) -> Result<Value, McpError> {
    let raw = serde_json::to_string(body).map_err(|_| McpError::message("MCP encode failed"))?;
    let resp = request(&agent(), "POST", url, headers)
        .send_string(&raw)
        .map_err(|_| McpError::message("MCP HTTP request failed; reconnect explicitly"))?;
    if !(200..300).contains(&resp.status()) {
        return Err(McpError::message("MCP HTTP unexpected status"));
    }
    if initialize {
        if let Some(id) = resp.header("Mcp-Session-Id") {
            if id.is_empty() || id.len() > 1024 || !id.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
                return Err(McpError::message("MCP invalid session header"));
            }
            headers.insert("Mcp-Session-Id".into(), id.into());
        }
    }
    // Notifications have no JSON-RPC response. HTTP errors above still propagate.
    if body.get("method").is_none() {
        return Ok(json!({}));
    }
    let Some(expected) = body.get("id") else {
        return Ok(json!({}));
    };
    let sse = resp.content_type() == "text/event-stream";
    if !sse && resp.content_type() != "application/json" {
        return Err(McpError::message("MCP unsupported response content type"));
    }
    let mut reader = BufReader::new(resp.into_reader());
    if sse {
        let mut events = Events::default();
        while let Some(value) = events.next(&mut reader)? {
            changes.notify(&value);
            if let Some(reply) = super::protocol::server_reply(&value) {
                post_json(url, headers, &reply, changes, false)?;
            }
            if value.get("id") == Some(expected) && value.get("method").is_none() {
                return Ok(value);
            }
        }
        return Err(McpError::message("MCP HTTP response missing matching id"));
    }
    let mut data = Vec::new();
    reader
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut data)
        .map_err(|_| McpError::message("MCP HTTP read failed"))?;
    if data.len() > 8 * 1024 * 1024 {
        return Err(McpError::message("MCP HTTP response too large"));
    }
    serde_json::from_slice(&data).map_err(|_| McpError::message("MCP invalid JSON response"))
}

// Bounded incremental SSE parsing; returning a response does not wait for EOF.
struct Events {
    id: Option<String>,
    retry_ms: u64,
}
impl Default for Events {
    fn default() -> Self {
        Self {
            id: None,
            retry_ms: 1000,
        }
    }
}
impl Events {
    fn next(&mut self, reader: &mut impl BufRead) -> Result<Option<Value>, McpError> {
        let mut payload = String::new();
        let mut bytes = 0;
        loop {
            let mut line = Vec::new();
            let n = reader
                .take((8 * 1024 * 1024 + 1 - bytes) as u64)
                .read_until(b'\n', &mut line)
                .map_err(|_| McpError::message("MCP SSE read failed"))?;
            bytes += n;
            if bytes > 8 * 1024 * 1024 {
                return Err(McpError::message("MCP SSE event too large"));
            }
            if n == 0 {
                return Ok(None);
            }
            let line = std::str::from_utf8(&line)
                .map_err(|_| McpError::message("MCP invalid SSE UTF-8"))?
                .trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if payload.is_empty() {
                    bytes = 0;
                    continue;
                }
                return serde_json::from_str(&payload)
                    .map(Some)
                    .map_err(|_| McpError::message("MCP invalid SSE JSON"));
            }
            if let Some(data) = line.strip_prefix("data:") {
                if !payload.is_empty() {
                    payload.push('\n');
                }
                payload.push_str(data.strip_prefix(' ').unwrap_or(data));
            } else if let Some(retry) = line.strip_prefix("retry:") {
                let retry = retry.strip_prefix(' ').unwrap_or(retry);
                if !retry.is_empty() && retry.bytes().all(|b| b.is_ascii_digit()) {
                    self.retry_ms = retry
                        .parse()
                        .map_err(|_| McpError::message("MCP SSE retry exceeds limit"))?;
                }
            } else if let Some(id) = line.strip_prefix("id:") {
                let id = id.strip_prefix(' ').unwrap_or(id);
                if id.len() > 1024 || !id.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
                    return Err(McpError::message("MCP invalid SSE event id"));
                }
                self.id = Some(id.into());
            }
        }
    }
}

fn subscribe(
    url: String,
    mut headers: BTreeMap<String, String>,
    changes: Arc<Changes>,
) -> Result<(), McpError> {
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut ready = Some(ready_tx);
        let mut events = Events::default();
        while !changes.stopped.load(Ordering::Acquire) {
            if let Some(id) = &events.id {
                headers.insert("Last-Event-ID".into(), id.clone());
            }
            let resp = match request(&agent(), "GET", &url, &headers).call() {
                Ok(resp) if resp.status() == 200 && resp.content_type() == "text/event-stream" => {
                    resp
                }
                Err(ureq::Error::Status(405, _)) => {
                    if let Some(ready) = ready.take() {
                        let _ = ready.send(());
                    }
                    return; // Optional GET unsupported; manual refresh remains available.
                }
                _ => {
                    changes.failed.store(true, Ordering::Release);
                    return;
                }
            };
            // A new stream may have a delivery gap. Reconcile by listing, never replay calls.
            changes.invalidate();
            if let Some(ready) = ready.take() {
                let _ = ready.send(());
            }
            let mut reader = BufReader::new(resp.into_reader());
            while !changes.stopped.load(Ordering::Acquire) {
                match events.next(&mut reader) {
                    Ok(Some(value)) => {
                        changes.notify(&value);
                        if let Some(reply) = super::protocol::server_reply(&value) {
                            if post_json(&url, &mut headers, &reply, &changes, false).is_err() {
                                changes.failed.store(true, Ordering::Release);
                                return;
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) if e.to_string() == "MCP SSE read failed" => break,
                    Err(_) => {
                        changes.failed.store(true, Ordering::Release);
                        return;
                    }
                }
            }
            let retry_started = std::time::Instant::now();
            while retry_started.elapsed() < Duration::from_millis(events.retry_ms.max(1000)) {
                if changes.stopped.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    });
    ready_rx
        .recv_timeout(Duration::from_secs(16))
        .map_err(|_| McpError::message("MCP subscription could not initialize"))
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn subscription_reopens_get_with_last_event_id_without_post_replay() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            assert!(read_http_request(&mut stream).starts_with("GET "));
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\nid: event-7\nretry: 1100\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}}\n\n").unwrap();
            drop(stream);
            let start = std::time::Instant::now();
            let (mut stream, _) = listener.accept().unwrap();
            assert!(start.elapsed() >= Duration::from_millis(1000));
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("GET "));
            assert!(request.contains("Last-Event-ID: event-7"));
            write!(
                stream,
                "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n"
            )
            .unwrap();
        });
        let changes = Arc::new(Changes::default());
        changes.enabled.store(true, Ordering::Release);
        subscribe(
            format!("http://{addr}"),
            BTreeMap::new(),
            Arc::clone(&changes),
        )
        .unwrap();
        worker.join().unwrap();
        assert!(!changes.failed.load(Ordering::Acquire));
        assert_eq!(changes.generation(), 2);
        changes.stopped.store(true, Ordering::Release);
    }

    #[test]
    fn optional_get_405_keeps_tools_available() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let raw = read_http_request(&mut stream);
                if raw.starts_with("GET ") {
                    write!(
                        stream,
                        "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n"
                    )
                    .unwrap();
                    continue;
                }
                let req: Value =
                    serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap();
                let result = if req["method"] == "initialize" {
                    json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":true}}})
                } else {
                    json!({"tools":[{"name":"ping"}]})
                };
                let body = json!({"jsonrpc":"2.0","id":req["id"],"result":result}).to_string();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
            }
        });
        let spec = ServerSpec {
            r#type: Some("http".into()),
            command: None,
            args: vec![],
            env: BTreeMap::new(),
            url: Some(format!("http://{addr}")),
            headers: BTreeMap::new(),
            cwd: None,
            enabled: None,
        };
        let server = super::super::stdio::attach_one_server("no-get", &spec).unwrap();
        worker.join().unwrap();
        assert!(server.is_healthy());
        assert!(!server.needs_refresh());
    }

    #[test]
    fn streamable_session_headers_idle_notifications_and_expiry() {
        use std::sync::mpsc;
        use std::time::Instant;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (release_tx, release_rx) = mpsc::channel();
        let (notify_tx, notify_rx) = mpsc::channel();
        let (subscribed_tx, subscribed_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut subscription = None;
            let mut release_rx = Some(release_rx);
            let mut lists = 0;
            let mut notify_rx = Some(notify_rx);
            let mut subscribed_tx = Some(subscribed_tx);
            // Initialize, initialized, GET subscription, initial list, refresh, expired call.
            for _ in 0..6 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let raw = read_http_request(&mut stream);
                if raw.starts_with("GET ") {
                    assert!(raw.contains("Mcp-Session-Id: fixture-session"));
                    assert!(raw.contains("MCP-Protocol-Version: 2025-06-18"));
                    let notify = notify_rx.take().unwrap();
                    let release_rx = release_rx.take().unwrap();
                    let ready = subscribed_tx.take().unwrap();
                    subscription = Some(thread::spawn(move || {
                        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").unwrap();
                        ready.send(()).unwrap();
                        notify.recv_timeout(Duration::from_secs(5)).unwrap();
                        write!(stream,"data: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}}\n\n").unwrap();
                        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    }));
                    continue;
                }
                let req: Value =
                    serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap();
                let method = req["method"].as_str().unwrap();
                if method != "initialize" {
                    assert!(raw.contains("Mcp-Session-Id: fixture-session"));
                    assert!(raw.contains("MCP-Protocol-Version: 2025-06-18"));
                }
                if method == "tools/call" {
                    write!(
                        stream,
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"
                    )
                    .unwrap();
                    continue;
                }
                if method == "notifications/initialized" {
                    write!(stream, "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n").unwrap();
                    continue;
                }
                let result = if method == "initialize" {
                    json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":true}}})
                } else {
                    lists += 1;
                    json!({"tools":[{"name":format!("tool-{lists}")}]})
                };
                let body = json!({"jsonrpc":"2.0","id":req["id"],"result":result}).to_string();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: fixture-session\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
            }
            subscription.unwrap().join().unwrap();
        });
        let spec = ServerSpec {
            r#type: Some("http".into()),
            command: None,
            args: vec![],
            env: BTreeMap::new(),
            url: Some(format!("http://{addr}/mcp")),
            headers: BTreeMap::from([("mcp-session-id".into(), "injected".into())]),
            cwd: None,
            enabled: None,
        };
        let mut server = super::super::stdio::attach_one_server("fixture", &spec).unwrap();
        subscribed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        notify_tx.send(()).unwrap();
        let start = Instant::now();
        // GET establishment and the idle notification each invalidate the catalog.
        while server.http_changes_for_test() < 2 {
            assert!(start.elapsed() < Duration::from_secs(5));
            thread::sleep(Duration::from_millis(10));
        }
        assert!(server.needs_refresh());
        assert!(!server.is_healthy());
        assert!(server.call_tool("tool-1", json!({})).is_err());
        server.refresh(false).unwrap();
        assert_eq!(server.tools[0].name, "tool-2");
        assert!(server.call_tool("tool-2", json!({})).is_err());
        assert!(server.tools.is_empty());
        release_tx.send(()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn sse_response_returns_before_eof_and_multiline_is_bounded() {
        use std::sync::mpsc;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_http_request(&mut stream);
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {{\"jsonrpc\":\"2.0\",\n data: ignored\ndata: \"id\":1,\"result\":{{}}}}\n\n").unwrap();
            rx.recv_timeout(Duration::from_secs(3)).unwrap();
        });
        let value = post_json(
            &format!("http://{addr}"),
            &mut BTreeMap::new(),
            &json!({"id":1,"method":"ping"}),
            &Changes::default(),
            false,
        )
        .unwrap();
        assert_eq!(value["id"], 1);
        tx.send(()).unwrap();
        worker.join().unwrap();
        let mut events = Events::default();
        assert!(events
            .next(&mut std::io::Cursor::new(
                "id: event-1\nretry: 2500\ndata:\n\n"
            ))
            .unwrap()
            .is_none());
        assert_eq!(events.id.as_deref(), Some("event-1"));
        assert_eq!(events.retry_ms, 2500);
        let oversized = format!("data: {}", "x".repeat(8 * 1024 * 1024));
        assert!(Events::default()
            .next(&mut std::io::Cursor::new(oversized))
            .is_err());
    }

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
                    r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"h","version":"0"}}}"#
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
