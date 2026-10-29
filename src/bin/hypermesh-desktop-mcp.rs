//! Inverse MCP: expose visor desktop verbs as MCP tools for external agents.
//!
//! Stdio JSON-RPC. Tools: view, mouse_move, mouse_click, mouse_scroll, type_text.
//! Talks to the visor's control socket by default. `HYPERMESH_VISOR_URL` selects
//! the opt-in TCP listener, with `HYPERMESH_VISOR_TOKEN` or the `api.token` file.

use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper_util::rt::TokioIo;
use hypermesh_visor::SCROLL_MAX_TICKS;
use serde_json::{json, Value};

const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const MAX_TEXT_CHARS: usize = 4096;
const MAX_COORD: i64 = 32767;

struct VisorResponse {
    status: u16,
    mime: String,
    body: Vec<u8>,
}

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
                    reply(&mut stdout, msg.get("id").cloned(), tool_list());
                }
                "tools/call" => {
                    // A tool invocation is a request, never an executable notification.
                    if let Some(id) = msg.get("id").filter(|id| id.is_string() || id.is_number()) {
                        let result = handle_call(&visor, &session, msg.get("params"));
                        let envelope = match result {
                            Ok(result) => json!({"jsonrpc":"2.0", "id":id, "result":result}),
                            Err(message) => json!({"jsonrpc":"2.0", "id":id,
                                "error":{"code":-32602,"message":message}}),
                        };
                        let _ = writeln!(stdout, "{envelope}");
                        let _ = stdout.flush();
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

fn tool_list() -> Value {
    let coordinate = json!({"type":"integer", "minimum":0, "maximum":MAX_COORD});
    let delta = json!({"type":"integer","minimum":-SCROLL_MAX_TICKS,"maximum":SCROLL_MAX_TICKS});
    json!({"tools":[
        {"name":"view", "description":"Capture the desktop as a PNG image", "inputSchema":
            {"type":"object", "properties":{}, "additionalProperties":false}},
        {"name":"mouse_move", "description":"Move the pointer in root-window pixels", "inputSchema":
            {"type":"object", "properties":{"x":coordinate,"y":coordinate}, "required":["x","y"], "additionalProperties":false}},
        {"name":"mouse_click", "description":"Click in root-window pixels", "inputSchema":
            {"type":"object", "properties":{"x":coordinate,"y":coordinate,
                "button":{"type":"string","enum":["left","right","middle"],"default":"left"}},
             "required":["x","y"],"additionalProperties":false}},
        {"name":"mouse_scroll", "description":"Scroll at root-window x/y in wheel ticks: positive right/down; 1 to 128 total ticks, horizontal then vertical", "inputSchema":
            {"type":"object", "properties":{"x":coordinate,"y":coordinate,"delta_x":delta,"delta_y":delta},
             "required":["x","y","delta_x","delta_y"],"additionalProperties":false}},
        {"name":"type_text", "description":"Type text; the visor also validates key support and the planned keystroke limit", "inputSchema":
            {"type":"object", "properties":{"text":{"type":"string","minLength":1,"maxLength":MAX_TEXT_CHARS}},
             "required":["text"],"additionalProperties":false}}
    ]})
}

fn handle_call(
    visor: &VisorEndpoint,
    session: &str,
    params: Option<&Value>,
) -> Result<Value, String> {
    let params = params
        .and_then(Value::as_object)
        .ok_or("params must be an object")?;
    let tool = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("name must be a string")?;
    let empty = json!({});
    let args = params.get("arguments").unwrap_or(&empty);
    // Validate before connecting, so malformed input can never drive the desktop.
    let (method, path, body) = tool_request(session, tool, args)?;
    let response = match visor {
        VisorEndpoint::Unix(socket) => unix_exchange(socket, method, &path, body.as_deref()),
        VisorEndpoint::Http { base, token } => http_exchange(
            &format!("{}{path}", base.trim_end_matches('/')),
            method,
            body.as_deref(),
            token.as_deref(),
        ),
    };
    Ok(match response {
        Ok(response) => tool_result(tool, response),
        Err(error) => failure(format!("{tool}: {error}")),
    })
}

fn tool_request(
    session: &str,
    tool: &str,
    args: &Value,
) -> Result<(&'static str, String, Option<String>), String> {
    let allowed: &[&str] = match tool {
        "view" => &[],
        "mouse_move" => &["x", "y"],
        "mouse_click" => &["x", "y", "button"],
        "mouse_scroll" => &["x", "y", "delta_x", "delta_y"],
        "type_text" => &["text"],
        _ => return Err("unknown tool".into()),
    };
    let args = args.as_object().ok_or("arguments must be an object")?;
    if args.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("unexpected argument".into());
    }
    let session =
        uuid::Uuid::parse_str(session).map_err(|_| "HYPERMESH_SESSION_ID must be a UUID")?;
    let coord = |name: &str| -> Result<i64, String> {
        args.get(name)
            .and_then(Value::as_i64)
            .filter(|value| (0..=MAX_COORD).contains(value))
            .ok_or_else(|| format!("{name} must be an integer from 0 to {MAX_COORD}"))
    };
    match tool {
        "view" => Ok(("POST", format!("/session/{session}/view"), None)),
        "mouse_move" | "mouse_click" => {
            let x = coord("x")?;
            let y = coord("y")?;
            let mut body =
                json!({"action":if tool == "mouse_move" {"move"} else {"click"},"x":x,"y":y});
            if tool == "mouse_click" {
                let button = match args.get("button") {
                    None => "left",
                    Some(value) => value
                        .as_str()
                        .filter(|b| ["left", "right", "middle"].contains(b))
                        .ok_or("button must be left, right, or middle")?,
                };
                body["button"] = json!(button);
            }
            Ok((
                "POST",
                format!("/session/{session}/mouse"),
                Some(body.to_string()),
            ))
        }
        "mouse_scroll" => {
            let x = coord("x")?;
            let y = coord("y")?;
            let delta = |name: &str| -> Result<i64, String> {
                args.get(name).and_then(Value::as_i64).filter(|n| (-SCROLL_MAX_TICKS..=SCROLL_MAX_TICKS).contains(n))
                    .ok_or_else(|| format!("{name} must be an integer from -{SCROLL_MAX_TICKS} to {SCROLL_MAX_TICKS}"))
            };
            let dx = delta("delta_x")?;
            let dy = delta("delta_y")?;
            if !(1..=SCROLL_MAX_TICKS).contains(&(dx.abs() + dy.abs())) {
                return Err("scroll must contain 1 to 128 total ticks".into());
            }
            Ok((
                "POST",
                format!("/session/{session}/mouse"),
                Some(json!({"action":"scroll","x":x,"y":y,"delta_x":dx,"delta_y":dy}).to_string()),
            ))
        }
        "type_text" => {
            let text = args
                .get("text")
                .and_then(Value::as_str)
                .ok_or("text must be a string")?;
            if text.is_empty() || text.chars().count() > MAX_TEXT_CHARS {
                return Err(format!(
                    "text must contain 1 to {MAX_TEXT_CHARS} characters"
                ));
            }
            Ok((
                "POST",
                format!("/session/{session}/type"),
                Some(json!({"text":text}).to_string()),
            ))
        }
        _ => unreachable!(),
    }
}

fn failure(message: String) -> Value {
    json!({"content":[{"type":"text","text":message}],"isError":true})
}

fn tool_result(tool: &str, response: VisorResponse) -> Value {
    if !(200..300).contains(&response.status) {
        let error = serde_json::from_slice::<Value>(&response.body)
            .ok()
            .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_owned));
        let detail = error
            .map(|s| format!(": {}", s.chars().take(2048).collect::<String>()))
            .unwrap_or_default();
        return failure(format!("{tool} status {}{detail}", response.status));
    }
    if tool == "view" {
        if !response
            .mime
            .split(';')
            .next()
            .is_some_and(|m| m.trim().eq_ignore_ascii_case("image/png"))
            || !response.body.starts_with(b"\x89PNG\r\n\x1a\n")
        {
            return failure("view returned an invalid PNG response".into());
        }
        return json!({"content":[{"type":"image", "mimeType":"image/png",
            "data":base64::engine::general_purpose::STANDARD.encode(response.body)}], "isError":false});
    }
    let text = match tool {
        "mouse_move" => "moved",
        "mouse_click" => "clicked",
        "mouse_scroll" => "scrolled",
        "type_text" => "typed",
        _ => "done",
    };
    json!({"content":[{"type":"text","text":text}],"isError":false})
}

fn http_exchange(
    url: &str,
    method: &str,
    body: Option<&str>,
    token: Option<&str>,
) -> Result<VisorResponse, String> {
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(Duration::from_secs(30))
        .build();
    let mut request = agent
        .request(method, url)
        .set("Content-Type", "application/json");
    if let Some(token) = token {
        request = request.set("Authorization", &format!("Bearer {token}"));
    }
    let result = match body {
        Some(body) => request.send_string(body),
        None => request.call(),
    };
    let response = match result {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(_) => return Err("visor HTTP transport failed".into()),
    };
    let status = response.status();
    let mime = response.header("content-type").unwrap_or("").to_owned();
    let mut body = Vec::new();
    response
        .into_reader()
        .take((MAX_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|_| "visor response read failed")?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err("visor response exceeds 32 MiB".into());
    }
    Ok(VisorResponse { status, mime, body })
}

fn unix_exchange(
    socket: &Path,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<VisorResponse, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), async {
            let stream = tokio::net::UnixStream::connect(socket)
                .await
                .map_err(|e| e.to_string())?;
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(stream))
                    .await
                    .map_err(|e| e.to_string())?;
            tokio::spawn(async move {
                let _ = connection.await;
            });
            let request = hyper::Request::builder()
                .method(method)
                .uri(path)
                .header("host", "localhost")
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(body.unwrap_or("").to_owned())))
                .map_err(|e| e.to_string())?;
            let response = sender
                .send_request(request)
                .await
                .map_err(|e| e.to_string())?;
            let status = response.status().as_u16();
            let mime = response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned();
            let body = Limited::new(response.into_body(), MAX_RESPONSE_BYTES)
                .collect()
                .await
                .map_err(|_| "visor response is incomplete or exceeds 32 MiB")?
                .to_bytes()
                .to_vec();
            Ok(VisorResponse { status, mime, body })
        })
        .await
        .map_err(|_| "visor request timed out".to_string())?
    })
}
