use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

use super::config::{load_profile_servers, ServerSpec};
use super::http::{self, HttpClient};
use super::McpError;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

struct StdioClient {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

pub struct AttachedServer {
    pub name: String,
    pub transport: String,
    pub tools: Vec<ToolInfo>,
    child: Option<Child>,
    client: Option<StdioClient>,
    http: Option<HttpClient>,
}

impl AttachedServer {
    pub(crate) fn from_http(
        name: String,
        transport: String,
        tools: Vec<ToolInfo>,
        http: HttpClient,
    ) -> Self {
        Self {
            name,
            transport,
            tools,
            child: None,
            client: None,
            http: Some(http),
        }
    }
}

impl std::fmt::Debug for AttachedServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttachedServer")
            .field("name", &self.name)
            .field("transport", &self.transport)
            .field("tools", &self.tools)
            .field("healthy", &self.is_healthy())
            .finish()
    }
}

impl Drop for AttachedServer {
    fn drop(&mut self) {
        self.client.take();
        self.http.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl AttachedServer {
    pub fn is_healthy(&self) -> bool {
        if self.tools.is_empty() {
            return false;
        }
        match self.transport.as_str() {
            "stdio" => {
                if self.client.is_none() {
                    return false;
                }
                let Some(child) = self.child.as_ref() else {
                    return false;
                };
                let pid = child.id() as i32;
                unsafe { libc::kill(pid, 0) == 0 }
            }
            "http" | "sse" => self.http.is_some(),
            _ => false,
        }
    }

    pub fn call_tool(&mut self, tool: &str, arguments: Value) -> Result<Value, McpError> {
        let name = self.name.clone();
        if let Some(client) = self.client.as_mut() {
            let id = client.next_id;
            client.next_id += 1;
            write_msg(
                &mut client.stdin,
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": "tools/call",
                    "params": {
                        "name": tool,
                        "arguments": arguments,
                    }
                }),
            )?;
            let response = read_msg(&name, &mut client.stdout)?;
            if let Some(err) = response.get("error") {
                return Err(McpError::message(format!(
                    "mcp server \"{name}\" tool \"{tool}\" failed: {err}"
                )));
            }
            return Ok(response
                .get("result")
                .cloned()
                .unwrap_or(Value::Object(Default::default())));
        }
        if let Some(http) = self.http.as_mut() {
            let response = http.call(
                "tools/call",
                json!({
                    "name": tool,
                    "arguments": arguments,
                }),
            )?;
            if let Some(err) = response.get("error") {
                return Err(McpError::message(format!(
                    "mcp server \"{name}\" tool \"{tool}\" failed: {err}"
                )));
            }
            return Ok(response
                .get("result")
                .cloned()
                .unwrap_or(Value::Object(Default::default())));
        }
        Err(McpError::message(format!(
            "mcp server \"{name}\" is not callable"
        )))
    }
}

/// Attached MCP servers for one session. Dropping kills stdio children.
#[derive(Debug, Default)]
pub struct McpBundle {
    pub profile: String,
    pub servers: Vec<AttachedServer>,
}

impl McpBundle {
    pub fn server_names(&self) -> Vec<String> {
        self.servers.iter().map(|s| s.name.clone()).collect()
    }

    pub fn healthy_server_names(&self) -> Vec<String> {
        self.servers
            .iter()
            .filter(|s| s.is_healthy())
            .map(|s| s.name.clone())
            .collect()
    }

    pub fn call_tool(
        &mut self,
        server: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, McpError> {
        let attached = self
            .servers
            .iter_mut()
            .find(|s| s.name == server)
            .ok_or_else(|| McpError::message(format!("mcp server \"{server}\" is not attached")))?;
        if !attached.tools.iter().any(|t| t.name == tool) {
            return Err(McpError::message(format!(
                "mcp server \"{server}\" has no tool \"{tool}\""
            )));
        }
        attached.call_tool(tool, arguments)
    }
}

/// Attach every enabled server in the profile. HTTP/SSE is recorded as
/// not-yet-connected; stdio is spawned and tools/list is called.
pub fn attach_profile(
    dir: &Path,
    profile_name: Option<&str>,
) -> Result<McpBundle, McpError> {
    let (profile, specs) = load_profile_servers(dir, profile_name)?;
    if specs.is_empty() {
        return Ok(McpBundle {
            profile,
            servers: Vec::new(),
        });
    }
    let mut servers = Vec::new();
    for (name, spec) in specs {
        servers.push(attach_one(&name, &spec)?);
    }
    Ok(McpBundle { profile, servers })
}

pub(crate) fn attach_one_server(name: &str, spec: &ServerSpec) -> Result<AttachedServer, McpError> {
    attach_one(name, spec)
}

fn attach_one(name: &str, spec: &ServerSpec) -> Result<AttachedServer, McpError> {
    match spec.transport() {
        "stdio" => attach_stdio(name, spec),
        "http" | "sse" => {
            let (tools, client) = http::handshake_http(name, spec)?;
            Ok(AttachedServer::from_http(
                name.to_string(),
                spec.transport().to_string(),
                tools,
                client,
            ))
        }
        other => Err(McpError::message(format!(
            "mcp server \"{name}\": unknown transport {other}"
        ))),
    }
}

fn attach_stdio(name: &str, spec: &ServerSpec) -> Result<AttachedServer, McpError> {
    let command = spec
        .command
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| McpError::message(format!("mcp server \"{name}\": stdio requires command")))?;

    let mut cmd = Command::new(command);
    cmd.args(&spec.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(cwd) = spec.cwd.as_deref() {
        if !cwd.trim().is_empty() {
            cmd.current_dir(cwd);
        }
    }
    for (k, v) in &spec.env {
        cmd.env(k, resolve_env(v));
    }

    let mut child = cmd.spawn().map_err(|e| {
        McpError::message(format!("mcp server \"{name}\": spawn {command}: {e}"))
    })?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| McpError::message(format!("mcp server \"{name}\": missing stdin")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| McpError::message(format!("mcp server \"{name}\": missing stdout")))?;

    let (tools, client) = match handshake(name, stdin, stdout) {
        Ok(pair) => pair,
        Err(err) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(err);
        }
    };

    Ok(AttachedServer {
        name: name.to_string(),
        transport: "stdio".into(),
        tools,
        child: Some(child),
        client: Some(client),
        http: None,
    })
}

fn resolve_env(value: &str) -> String {
    let trimmed = value.trim();
    if let Some(inner) = trimmed
        .strip_prefix("${env:")
        .and_then(|s| s.strip_suffix('}'))
    {
        return std::env::var(inner).unwrap_or_default();
    }
    value.to_string()
}

fn handshake(
    name: &str,
    mut stdin: ChildStdin,
    stdout: ChildStdout,
) -> Result<(Vec<ToolInfo>, StdioClient), McpError> {
    let mut reader = BufReader::new(stdout);
    write_msg(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "hypermesh-visor", "version": "0.1.0"}
            }
        }),
    )?;
    let init = read_msg(name, &mut reader)?;
    if init.get("error").is_some() {
        return Err(McpError::message(format!(
            "mcp server \"{name}\": initialize failed: {init}"
        )));
    }
    write_msg(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    )?;
    write_msg(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }),
    )?;
    let listed = read_msg(name, &mut reader)?;
    if let Some(err) = listed.get("error") {
        return Err(McpError::message(format!(
            "mcp server \"{name}\": tools/list failed: {err}"
        )));
    }
    let tools = listed
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            McpError::message(format!("mcp server \"{name}\": tools/list missing tools"))
        })?;
    let mut out = Vec::new();
    for tool in tools {
        let tool_name = tool
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if tool_name.is_empty() {
            continue;
        }
        let description = tool
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string);
        out.push(ToolInfo {
            name: tool_name,
            description,
        });
    }
    Ok((
        out,
        StdioClient {
            stdin,
            stdout: reader,
            next_id: 3,
        },
    ))
}

fn write_msg(stdin: &mut ChildStdin, value: Value) -> Result<(), McpError> {
    let raw = serde_json::to_string(&value)
        .map_err(|e| McpError::message(format!("encode mcp message: {e}")))?;
    writeln!(stdin, "{raw}").map_err(|e| McpError::message(format!("write mcp: {e}")))?;
    stdin
        .flush()
        .map_err(|e| McpError::message(format!("flush mcp: {e}")))
}

fn read_msg(name: &str, reader: &mut BufReader<ChildStdout>) -> Result<Value, McpError> {
    for _ in 0..32 {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|e| McpError::message(format!("mcp server \"{name}\": read: {e}")))?;
        if line.trim().is_empty() {
            return Err(McpError::message(format!(
                "mcp server \"{name}\": closed stdout"
            )));
        }
        let value: Value = serde_json::from_str(line.trim()).map_err(|e| {
            McpError::message(format!("mcp server \"{name}\": bad json: {e}"))
        })?;
        if value.get("id").is_some() || value.get("error").is_some() {
            return Ok(value);
        }
    }
    Err(McpError::message(format!(
        "mcp server \"{name}\": no json-rpc response"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn fixture_script(dir: &Path) -> std::path::PathBuf {
        let script = dir.join("fixture-mcp.py");
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
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"0"}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"tools":[{"name":"ping","description":"ping","inputSchema":{"type":"object"}}]}})
    elif method == "tools/call":
        name = msg.get("params",{}).get("name")
        if name == "ping":
            send({"jsonrpc":"2.0","id":msg["id"],"result":{"content":[{"type":"text","text":"pong"}]}})
        else:
            send({"jsonrpc":"2.0","id":msg["id"],"error":{"code":-32601,"message":"unknown tool"}})
    else:
        if "id" in msg:
            send({"jsonrpc":"2.0","id":msg["id"],"error":{"code":-32601,"message":"unknown"}})
"#,
        )
        .unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();
        script
    }

    fn attach_servers_for_test(
        profile: &str,
        specs: BTreeMap<String, ServerSpec>,
    ) -> Result<McpBundle, McpError> {
        let mut servers = Vec::new();
        for (name, spec) in specs {
            servers.push(attach_one(&name, &spec)?);
        }
        Ok(McpBundle {
            profile: profile.into(),
            servers,
        })
    }

    #[test]
    fn attaches_fixture_stdio_lists_and_calls_tools() {
        let dir = std::env::temp_dir().join(format!(
            "hm-mcp-stdio-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let script = fixture_script(&dir);

        let mut specs = BTreeMap::new();
        specs.insert(
            "fixture".into(),
            ServerSpec {
                r#type: Some("stdio".into()),
                command: Some(script.to_string_lossy().into_owned()),
                args: Vec::new(),
                env: BTreeMap::new(),
                url: None,
                headers: BTreeMap::new(),
                cwd: None,
                enabled: None,
            },
        );
        let mut bundle = attach_servers_for_test("default", specs).unwrap();
        assert!(bundle.servers[0].is_healthy());
        assert_eq!(bundle.healthy_server_names(), vec!["fixture".to_string()]);
        let result = bundle
            .call_tool("fixture", "ping", json!({}))
            .unwrap();
        assert_eq!(result["content"][0]["text"], "pong");
        drop(bundle);
    }
}
