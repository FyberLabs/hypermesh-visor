//! Inverse MCP: expose visor desktop verbs as MCP tools for external agents.
//!
//! Stdio JSON-RPC. Tools: view, mouse_move, mouse_click, type_text.
//! Talks to the visor's control socket by default. `HYPERMESH_VISOR_URL` selects
//! the opt-in TCP listener, with `HYPERMESH_VISOR_TOKEN` or the `api.token` file.

use std::io::{self, BufRead, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

enum VisorEndpoint {
    Unix(PathBuf),
    Http { base: String, token: Option<String> },
}

impl VisorEndpoint {
    fn from_env() -> Result<Self, String> {
        match std::env::var("HYPERMESH_VISOR_URL") {
            Ok(url) if !url.trim().is_empty() => Ok(Self::Http {
                base: url,
                token: configured_token(),
            }),
            _ => {
                let dir = hypermesh_visor::runtime_dir().map_err(|err| err.to_string())?;
                Ok(Self::Unix(hypermesh_visor::control_socket_file(&dir)))
            }
        }
    }
}

fn configured_token() -> Option<String> {
    if let Ok(token) = std::env::var("HYPERMESH_VISOR_TOKEN") {
        let token = token.trim();
        if !token.is_empty() {
            return Some(token.to_string());
        }
    }
    let Ok(dir) = hypermesh_visor::runtime_dir() else {
        return None;
    };
    hypermesh_visor::read_api_token(&dir).ok()
}

fn main() {
    let visor = match VisorEndpoint::from_env() {
        Ok(visor) => visor,
        Err(err) => {
            eprintln!("hypermesh-desktop-mcp: {err}");
            std::process::exit(1);
        }
    };
    let session = std::env::var("HYPERMESH_SESSION_ID").unwrap_or_default();
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
            match method {
                "initialize" => {
                    reply(
                        &mut stdout,
                        msg.get("id").cloned(),
                        json!({
                            "protocolVersion": "2024-11-05",
                            "capabilities": {"tools": {}},
                            "serverInfo": {"name": "hypermesh-desktop", "version": "0.1.0"}
                        }),
                    );
                }
                "notifications/initialized" => {}
                "tools/list" => {
                    reply(
                        &mut stdout,
                        msg.get("id").cloned(),
                        json!({
                            "tools": [
                                {"name": "view", "description": "Capture the desktop (POST /session/{id}/view)", "inputSchema": {"type": "object", "properties": {}}},
                                {"name": "mouse_move", "description": "Move the pointer", "inputSchema": {"type": "object", "properties": {"x": {"type": "integer"}, "y": {"type": "integer"}}, "required": ["x", "y"]}},
                                {"name": "mouse_click", "description": "Click", "inputSchema": {"type": "object", "properties": {"x": {"type": "integer"}, "y": {"type": "integer"}, "button": {"type": "string"}}, "required": ["x", "y"]}},
                                {"name": "type_text", "description": "Type text", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}}
                            ]
                        }),
                    );
                }
                "tools/call" => {
                    let id = msg.get("id").cloned();
                    let params = msg.get("params").cloned().unwrap_or(json!({}));
                    let tool = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let args = params.get("arguments").cloned().unwrap_or(json!({}));
                    match visor.call(&session, tool, &args) {
                        Ok(text) => reply(
                            &mut stdout,
                            id,
                            json!({"content": [{"type": "text", "text": text}]}),
                        ),
                        Err(err) => {
                            let _ = writeln!(
                                stdout,
                                "{}",
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "error": {"code": -32000, "message": err}
                                })
                            );
                            let _ = stdout.flush();
                        }
                    }
                }
                _ => {
                    if msg.get("id").is_some() {
                        let _ = writeln!(
                            stdout,
                            "{}",
                            json!({
                                "jsonrpc": "2.0",
                                "id": msg.get("id"),
                                "error": {"code": -32601, "message": "method not found"}
                            })
                        );
                        let _ = stdout.flush();
                    }
                }
            }
        }
    }
}

fn reply(stdout: &mut impl Write, id: Option<Value>, result: Value) {
    let mut body = json!({"jsonrpc": "2.0", "result": result});
    if let Some(id) = id {
        body["id"] = id;
    }
    let _ = writeln!(stdout, "{body}");
    let _ = stdout.flush();
}

impl VisorEndpoint {
    fn call(&self, session: &str, tool: &str, args: &Value) -> Result<String, String> {
        if session.trim().is_empty() {
            return Err("HYPERMESH_SESSION_ID is required".into());
        }
        let (method, path, body) = tool_request(session, tool, args)?;
        match self {
            Self::Unix(path_sock) => {
                let (status, _) = unix_exchange(path_sock, method, &path, body.as_deref(), None)?;
                tool_result(tool, status)
            }
            Self::Http { base, token } => {
                let url = format!("{}{path}", base.trim_end_matches('/'));
                let status = http_exchange(&url, method, body.as_deref(), token.as_deref())?;
                tool_result(tool, status)
            }
        }
    }
}

fn tool_request(
    session: &str,
    tool: &str,
    args: &Value,
) -> Result<(&'static str, String, Option<String>), String> {
    match tool {
        "view" => Ok(("POST", format!("/session/{session}/view"), None)),
        "mouse_move" => {
            let x = args.get("x").and_then(|v| v.as_i64()).unwrap_or(0);
            let y = args.get("y").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok((
                "POST",
                format!("/session/{session}/mouse"),
                Some(json!({"action": "move", "x": x, "y": y}).to_string()),
            ))
        }
        "mouse_click" => {
            let x = args.get("x").and_then(|v| v.as_i64()).unwrap_or(0);
            let y = args.get("y").and_then(|v| v.as_i64()).unwrap_or(0);
            let button = args
                .get("button")
                .and_then(|v| v.as_str())
                .unwrap_or("left");
            Ok((
                "POST",
                format!("/session/{session}/mouse"),
                Some(json!({"action": "click", "x": x, "y": y, "button": button}).to_string()),
            ))
        }
        "type_text" => {
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            Ok((
                "POST",
                format!("/session/{session}/type"),
                Some(json!({"text": text}).to_string()),
            ))
        }
        other => Err(format!("unknown tool {other}")),
    }
}

fn tool_result(tool: &str, status: u16) -> Result<String, String> {
    if !(200..300).contains(&status) {
        return Err(format!("{tool} status {status}"));
    }
    Ok(match tool {
        "view" => format!("view status {status}"),
        "mouse_move" => "moved".into(),
        "mouse_click" => "clicked".into(),
        "type_text" => "typed".into(),
        other => format!("{other} status {status}"),
    })
}

#[cfg(test)]
fn call_visor(base: &str, session: &str, tool: &str, args: &Value) -> Result<String, String> {
    VisorEndpoint::Http {
        base: base.to_string(),
        token: None,
    }
    .call(session, tool, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_tools_cannot_control_the_companion() {
        let err = call_visor(
            "http://127.0.0.1:9",
            "00000000-0000-0000-0000-000000000001",
            "read_file",
            &json!({"path": "/run/user/1/hypermesh/companion.sock"}),
        )
        .unwrap_err();
        assert!(err.contains("unknown tool"));
        let err = call_visor(
            "http://127.0.0.1:9",
            "00000000-0000-0000-0000-000000000001",
            "resume",
            &json!({}),
        )
        .unwrap_err();
        assert!(err.contains("unknown tool"));
        let err = call_visor("http://127.0.0.1:9", "  ", "view", &json!({})).unwrap_err();
        assert!(err.contains("HYPERMESH_SESSION_ID"));
    }
}

fn http_exchange(
    url: &str,
    method: &str,
    body: Option<&str>,
    token: Option<&str>,
) -> Result<u16, String> {
    let mut request = match method {
        "POST" => ureq::post(url),
        other => return Err(format!("unsupported method {other}")),
    };
    request = request
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(30));
    if let Some(token) = token {
        request = request.set("Authorization", &format!("Bearer {token}"));
    }
    let response = match body {
        Some(body) => request.send_string(body),
        None => request.call(),
    };
    response
        .map(|response| response.status())
        .map_err(|err| err.to_string())
}

fn unix_exchange(
    socket: &Path,
    method: &str,
    path: &str,
    body: Option<&str>,
    token: Option<&str>,
) -> Result<(u16, String), String> {
    let body = body.unwrap_or("");
    let mut headers = format!(
        "{method} {path} HTTP/1.1\r\nhost: localhost\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
        body.len()
    );
    if let Some(token) = token {
        headers.push_str(&format!("authorization: Bearer {token}\r\n"));
    }
    headers.push_str("\r\n");
    let mut stream = UnixStream::connect(socket).map_err(|err| err.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|err| err.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .map_err(|err| err.to_string())?;
    stream
        .write_all(headers.as_bytes())
        .map_err(|err| err.to_string())?;
    stream
        .write_all(body.as_bytes())
        .map_err(|err| err.to_string())?;
    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .map_err(|err| err.to_string())?;
    let text = String::from_utf8_lossy(&buf);
    let (status, response_body) = split_http(&text);
    if status == 0 {
        return Err("visor request failed".into());
    }
    Ok((status, response_body))
}

fn split_http(text: &str) -> (u16, String) {
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return (0, String::new());
    };
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    (status, body.to_string())
}
