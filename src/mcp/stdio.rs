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

impl StdioClient {
    fn call(&mut self, name: &str, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        write_msg(
            &mut self.stdin,
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
        )?;
        let response = read_msg(name, &mut self.stdout)?;
        if response.get("id") != Some(&json!(id)) || response.get("jsonrpc") != Some(&json!("2.0"))
        {
            return Err(McpError::message("MCP stdio response id/version mismatch"));
        }
        Ok(response)
    }
}

pub struct AttachedServer {
    pub name: String,
    pub transport: String,
    pub tools: Vec<ToolInfo>,
    child: Option<Child>,
    client: Option<StdioClient>,
    http: Option<HttpClient>,
    spec: ServerSpec,
    pub refreshing: bool,
}

impl AttachedServer {
    pub(crate) fn from_http(
        name: String,
        transport: String,
        tools: Vec<ToolInfo>,
        http: HttpClient,
        spec: ServerSpec,
    ) -> Self {
        Self {
            name,
            transport,
            tools,
            child: None,
            client: None,
            http: Some(http),
            spec,
            refreshing: false,
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
                // kill(pid, 0) also succeeds for zombies. Do not advertise them.
                std::fs::read_to_string(format!("/proc/{pid}/stat"))
                    .ok()
                    .and_then(|stat| {
                        stat.rsplit_once(')')
                            .map(|(_, rest)| rest.trim_start().starts_with('Z'))
                    })
                    .is_some_and(|zombie| !zombie)
            }
            "http" | "sse" => self.http.is_some(),
            _ => false,
        }
    }

    fn disconnect(&mut self) {
        self.tools.clear();
        self.client.take();
        self.http.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        let result = if let Some(client) = self.client.as_mut() {
            client.call(&self.name, method, params)
        } else if let Some(client) = self.http.as_mut() {
            client.call(method, params)
        } else {
            Err(McpError::message(
                "MCP server disconnected; reconnect explicitly",
            ))
        };
        let result = result.and_then(|response| {
            if response.get("result").is_none() && response.get("error").is_none() {
                Err(McpError::message("MCP response missing result/error"))
            } else {
                Ok(response)
            }
        });
        if result.is_err() {
            self.disconnect();
        }
        result
    }

    pub fn refresh(&mut self, reconnect: bool) -> Result<(), McpError> {
        if reconnect {
            self.disconnect();
            let replacement = attach_one(&self.name, &self.spec)?;
            *self = replacement;
            return Ok(());
        }
        match super::discovery::list_tools(|params| self.request("tools/list", params)) {
            Ok(tools) => {
                self.tools = tools;
                Ok(())
            }
            Err(error) => {
                self.disconnect();
                Err(error)
            }
        }
    }

    pub fn call_tool(&mut self, tool: &str, arguments: Value) -> Result<Value, McpError> {
        let response = self.request("tools/call", json!({"name":tool,"arguments":arguments}))?;
        if response.get("error").is_some() {
            return Err(McpError::message("MCP tool call rejected"));
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| McpError::message("MCP tool call missing result"))
    }
}

/// Attached MCP servers for one session. Dropping kills stdio children.
#[derive(Debug, Default)]
pub struct McpBundle {
    pub profile: String,
    pub servers: Vec<AttachedServer>,
}

impl McpBundle {
    pub fn take_for_refresh(&mut self, name: &str) -> Result<AttachedServer, McpError> {
        let server = self
            .servers
            .iter_mut()
            .find(|s| s.name == name)
            .ok_or_else(|| McpError::message("MCP server not attached"))?;
        if server.refreshing {
            return Err(McpError::message("MCP server refresh in progress"));
        }
        let placeholder = AttachedServer {
            name: server.name.clone(),
            transport: server.transport.clone(),
            tools: Vec::new(),
            child: None,
            client: None,
            http: None,
            spec: server.spec.clone(),
            refreshing: true,
        };
        Ok(std::mem::replace(server, placeholder))
    }

    pub fn restore_after_refresh(&mut self, server: AttachedServer) {
        if let Some(slot) = self
            .servers
            .iter_mut()
            .find(|s| s.name == server.name && s.refreshing)
        {
            *slot = server;
        }
    }

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
pub fn attach_profile(dir: &Path, profile_name: Option<&str>) -> Result<McpBundle, McpError> {
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
                spec.clone(),
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
        .ok_or_else(|| {
            McpError::message(format!("mcp server \"{name}\": stdio requires command"))
        })?;

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

    let mut child = cmd
        .spawn()
        .map_err(|e| McpError::message(format!("mcp server \"{name}\": spawn {command}: {e}")))?;
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
        spec: spec.clone(),
        refreshing: false,
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
    stdin: ChildStdin,
    stdout: ChildStdout,
) -> Result<(Vec<ToolInfo>, StdioClient), McpError> {
    // Nonblocking reads allow a deadline even if a child never terminates a line.
    use std::os::fd::AsRawFd;
    let fd = stdout.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(McpError::message("MCP cannot configure stdout"));
    }
    let mut client = StdioClient {
        stdin,
        stdout: BufReader::new(stdout),
        next_id: 1,
    };
    let init = client.call(name, "initialize", json!({"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"hypermesh-visor","version":"0.1.0"}}))?;
    if init.get("error").is_some() {
        return Err(McpError::message("MCP initialize rejected"));
    }
    write_msg(
        &mut client.stdin,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )?;
    let tools = super::discovery::list_tools(|params| client.call(name, "tools/list", params))?;
    Ok((tools, client))
}

fn write_msg(stdin: &mut ChildStdin, value: Value) -> Result<(), McpError> {
    let raw = serde_json::to_string(&value)
        .map_err(|e| McpError::message(format!("encode mcp message: {e}")))?;
    writeln!(stdin, "{raw}").map_err(|e| McpError::message(format!("write mcp: {e}")))?;
    stdin
        .flush()
        .map_err(|e| McpError::message(format!("flush mcp: {e}")))
}

fn read_msg(_name: &str, reader: &mut BufReader<ChildStdout>) -> Result<Value, McpError> {
    read_msg_with_timeout(reader, std::time::Duration::from_secs(15))
}

fn read_msg_with_timeout(
    reader: &mut BufReader<ChildStdout>,
    timeout: std::time::Duration,
) -> Result<Value, McpError> {
    use std::os::fd::AsRawFd;
    use std::time::Instant;
    let deadline = Instant::now() + timeout;
    for _ in 0..32 {
        let mut line = Vec::new();
        loop {
            if Instant::now() >= deadline {
                return Err(McpError::message("MCP response timeout"));
            }
            // fill_buf bounds allocation; never read an unbounded line.
            match reader.fill_buf() {
                Ok([]) => return Err(McpError::message("MCP closed stdout")),
                Ok(buf) => {
                    let n = buf
                        .iter()
                        .position(|b| *b == b'\n')
                        .map(|i| i + 1)
                        .unwrap_or(buf.len());
                    if line.len() + n > 8 * 1024 * 1024 {
                        return Err(McpError::message("MCP response too large"));
                    }
                    line.extend_from_slice(&buf[..n]);
                    reader.consume(n);
                    if line.last() == Some(&b'\n') {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    let mut poll = libc::pollfd {
                        fd: reader.get_ref().as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    unsafe {
                        libc::poll(&mut poll, 1, 50);
                    }
                }
                Err(_) => return Err(McpError::message("MCP response read failed")),
            }
        }
        let value: Value = serde_json::from_slice(&line)
            .map_err(|_| McpError::message("MCP invalid JSON response"))?;
        if value.get("id").is_some() {
            return Ok(value);
        }
        // Notifications, including tools/list_changed, do not satisfy a request.
    }
    Err(McpError::message("MCP notification limit exceeded"))
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
    fn unterminated_stdio_response_has_a_deadline() {
        use std::os::fd::AsRawFd;
        let mut child = Command::new("python3")
            .args([
                "-u",
                "-c",
                "import sys,time; sys.stdout.write('{'); sys.stdout.flush(); time.sleep(5)",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let fd = stdout.as_raw_fd();
        unsafe {
            libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
        }
        let result = read_msg_with_timeout(
            &mut BufReader::new(stdout),
            std::time::Duration::from_millis(100),
        );
        let _ = child.kill();
        let _ = child.wait();
        assert!(result.unwrap_err().to_string().contains("timeout"));
    }

    #[test]
    fn paginated_refresh_failure_and_explicit_reconnect_never_replay_calls() {
        let dir = std::env::temp_dir().join(format!("hm-refresh-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let version = dir.join("version");
        let calls = dir.join("calls");
        fs::write(&version, "old").unwrap();
        let script = r#"
import json,sys,pathlib,os
version=pathlib.Path(sys.argv[1]); calls=pathlib.Path(sys.argv[2])
for line in sys.stdin:
 m=json.loads(line); method=m['method']
 if 'id' not in m: continue
 result={}
 if method=='initialize': result={'protocolVersion':'2024-11-05','capabilities':{'tools':{'listChanged':True}}}
 elif method=='tools/list':
  v=version.read_text()
  if v=='fail':
   print(json.dumps({'jsonrpc':'2.0','id':m['id'],'error':{'message':'private detail'}}),flush=True); continue
  second='cursor' in m['params']
  if second: assert m['params']['cursor']=='opaque +/='
  result={'tools':[{'name':v+('-b' if second else '-a')}]}
  if not second: result['nextCursor']='opaque +/='
  print(json.dumps({'jsonrpc':'2.0','method':'notifications/tools/list_changed'}),flush=True)
 elif method=='tools/call':
  with calls.open('a') as f: f.write('once\n')
  os._exit(0)
 print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':result}),flush=True)
"#;
        let spec = ServerSpec {
            r#type: Some("stdio".into()),
            command: Some("python3".into()),
            args: vec![
                "-u".into(),
                "-c".into(),
                script.into(),
                version.to_string_lossy().into(),
                calls.to_string_lossy().into(),
            ],
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            cwd: None,
            enabled: None,
        };
        let mut server = attach_one("fixture", &spec).unwrap();
        assert_eq!(
            server
                .tools
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["old-a", "old-b"]
        );
        fs::write(&version, "new").unwrap();
        server.refresh(false).unwrap();
        assert_eq!(server.tools[1].name, "new-b");
        fs::write(&version, "fail").unwrap();
        let error = server.refresh(false).unwrap_err().to_string();
        assert!(!error.contains("private detail"));
        assert!(!server.is_healthy());
        assert!(server.tools.is_empty());
        fs::write(&version, "again").unwrap();
        server.refresh(true).unwrap();
        assert_eq!(server.tools[0].name, "again-a");
        assert!(server.call_tool("again-a", json!({})).is_err());
        assert!(!server.is_healthy());
        assert_eq!(fs::read_to_string(&calls).unwrap(), "once\n");
        server.refresh(true).unwrap();
        assert_eq!(fs::read_to_string(&calls).unwrap(), "once\n");
        let mut bundle = McpBundle {
            profile: "default".into(),
            servers: vec![server],
        };
        let owned = bundle.take_for_refresh("fixture").unwrap();
        assert!(bundle.take_for_refresh("fixture").is_err());
        assert!(bundle.healthy_server_names().is_empty());
        bundle.restore_after_refresh(owned);
        assert_eq!(bundle.healthy_server_names(), ["fixture"]);
        drop(bundle);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn wrong_response_id_disconnects_stdio() {
        let spec = ServerSpec { r#type: Some("stdio".into()), command: Some("python3".into()),
            args: vec!["-u".into(), "-c".into(), "import sys,json; json.loads(sys.stdin.readline()); print(json.dumps({'jsonrpc':'2.0','id':999,'result':{}}),flush=True)".into()],
            env: BTreeMap::new(), url: None, headers: BTreeMap::new(), cwd: None, enabled: None };
        assert!(attach_one("wrong", &spec)
            .unwrap_err()
            .to_string()
            .contains("mismatch"));
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
        let result = bundle.call_tool("fixture", "ping", json!({})).unwrap();
        assert_eq!(result["content"][0]["text"], "pong");
        drop(bundle);
    }
}
