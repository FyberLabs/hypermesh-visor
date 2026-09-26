//! Inverse MCP: expose visor desktop verbs as MCP tools for external agents.
//!
//! Stdio JSON-RPC. Tools: view, mouse_move, mouse_click, type_text.
//! Talks to a running visor on HYPERMESH_VISOR_URL (default http://127.0.0.1:9847).

use std::io::{self, BufRead, Write};

use serde_json::{json, Value};

fn main() {
    let visor = std::env::var("HYPERMESH_VISOR_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9847".into());
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
                    match call_visor(&visor, &session, tool, &args) {
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

fn call_visor(base: &str, session: &str, tool: &str, args: &Value) -> Result<String, String> {
    if session.trim().is_empty() {
        return Err("HYPERMESH_SESSION_ID is required".into());
    }
    let base = base.trim_end_matches('/');
    match tool {
        "view" => {
            let url = format!("{base}/session/{session}/view");
            let resp = ureq::post(&url)
                .timeout(std::time::Duration::from_secs(30))
                .call()
                .map_err(|e| e.to_string())?;
            Ok(format!("view status {}", resp.status()))
        }
        "mouse_move" => {
            let x = args.get("x").and_then(|v| v.as_i64()).unwrap_or(0);
            let y = args.get("y").and_then(|v| v.as_i64()).unwrap_or(0);
            let url = format!("{base}/session/{session}/mouse");
            post_json(
                &url,
                &json!({"action": "move", "x": x, "y": y}),
            )?;
            Ok("moved".into())
        }
        "mouse_click" => {
            let x = args.get("x").and_then(|v| v.as_i64()).unwrap_or(0);
            let y = args.get("y").and_then(|v| v.as_i64()).unwrap_or(0);
            let button = args
                .get("button")
                .and_then(|v| v.as_str())
                .unwrap_or("left");
            let url = format!("{base}/session/{session}/mouse");
            post_json(
                &url,
                &json!({"action": "click", "x": x, "y": y, "button": button}),
            )?;
            Ok("clicked".into())
        }
        "type_text" => {
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let url = format!("{base}/session/{session}/type");
            post_json(&url, &json!({"text": text}))?;
            Ok("typed".into())
        }
        other => Err(format!("unknown tool {other}")),
    }
}

fn post_json(url: &str, body: &Value) -> Result<(), String> {
    let raw = serde_json::to_string(body).map_err(|e| e.to_string())?;
    ureq::post(url)
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(30))
        .send_string(&raw)
        .map_err(|e| e.to_string())?;
    Ok(())
}
