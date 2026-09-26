//! Vault role B: fetch one secret via a short-lived MCP tool call.
//!
//! This is not session tool attach (role A). The child lives only for the call.

use std::path::Path;

use serde_json::{json, Value};
use zeroize::Zeroizing;

use super::config::load_named_server;
use super::stdio::attach_one_server;
use super::McpError;

/// Call `server`/`tool` once and return concatenated text content as secret bytes.
pub fn fetch_secret_bytes(
    dir: &Path,
    server: &str,
    tool: &str,
    secret_name: &str,
) -> Result<Zeroizing<Vec<u8>>, McpError> {
    let spec = load_named_server(dir, server)?;
    let mut attached = attach_one_server(server, &spec)?;
    if !attached.tools.iter().any(|t| t.name == tool) {
        return Err(McpError::message(format!(
            "mcp server \"{server}\" has no tool \"{tool}\""
        )));
    }
    let result = attached.call_tool(
        tool,
        json!({
            "name": secret_name,
        }),
    )?;
    let bytes = text_from_tool_result(&result).ok_or_else(|| {
        McpError::message(format!(
            "mcp server \"{server}\" tool \"{tool}\" returned no secret text"
        ))
    })?;
    if bytes.is_empty() {
        return Err(McpError::message(format!(
            "mcp server \"{server}\" tool \"{tool}\" returned empty secret text"
        )));
    }
    Ok(Zeroizing::new(bytes))
}

fn text_from_tool_result(result: &Value) -> Option<Vec<u8>> {
    let content = result.get("content")?.as_array()?;
    let mut out = String::new();
    for part in content {
        if part.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            out.push_str(text);
        }
    }
    if out.is_empty() {
        // Some servers put the payload under result directly as a string.
        if let Some(text) = result.as_str() {
            return Some(text.as_bytes().to_vec());
        }
        return None;
    }
    Some(out.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn fetches_secret_text_from_fixture_stdio() {
        let dir = std::env::temp_dir().join(format!("hm-mcp-secret-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("secret-mcp.py");
        fs::write(
            &script,
            r#"#!/usr/bin/env python3
import json, sys
def recv():
    line = sys.stdin.readline()
    if not line:
        raise SystemExit(0)
    return json.loads(line)
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()
while True:
    msg = recv()
    method = msg.get("method")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"secrets","version":"0"}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"tools":[{"name":"get_secret","description":"get","inputSchema":{"type":"object"}}]}})
    elif method == "tools/call":
        name = msg.get("params",{}).get("arguments",{}).get("name","")
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"content":[{"type":"text","text":"secret-for-"+name}]}})
"#,
        )
        .unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();
        fs::write(
            dir.join("mcp.json"),
            format!(
                r#"{{"mcpServers":{{"vault-fixture":{{"command":"{}","type":"stdio"}}}}}}"#,
                script.display()
            ),
        )
        .unwrap();

        let bytes = fetch_secret_bytes(&dir, "vault-fixture", "get_secret", "token").unwrap();
        assert_eq!(bytes.as_slice(), b"secret-for-token");
        let _ = fs::remove_dir_all(&dir);
    }
}
