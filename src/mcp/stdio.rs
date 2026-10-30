use super::subscription::Changes;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{atomic::Ordering, mpsc, Arc, Mutex};

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
    stdin: Arc<Mutex<ChildStdin>>,
    responses: mpsc::Receiver<Value>,
    changes: Arc<Changes>,
    reader: Option<std::thread::JoinHandle<()>>,
    supports_tools: bool,
    listed_generation: u64,
    next_id: u64,
}

impl StdioClient {
    fn call(&mut self, _name: &str, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        write_msg(
            &self.stdin,
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            &self.changes,
        )?;
        let response = self
            .responses
            .recv_timeout(std::time::Duration::from_secs(15))
            .map_err(|_| McpError::message("MCP response timeout or reader disconnected"))?;
        if response.get("id") != Some(&json!(id)) || response.get("jsonrpc") != Some(&json!("2.0"))
        {
            return Err(McpError::message("MCP stdio response id/version mismatch"));
        }
        Ok(response)
    }
}

impl Drop for StdioClient {
    fn drop(&mut self) {
        self.changes.stopped.store(true, Ordering::Release);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

// A single reader owns stdout for the entire connection, including idle periods.
fn read_background(
    mut stdout: BufReader<ChildStdout>,
    stdin: Arc<Mutex<ChildStdin>>,
    tx: mpsc::SyncSender<Value>,
    changes: Arc<Changes>,
    frame_timeout: std::time::Duration,
) {
    use std::time::{Duration, Instant};
    let mut line = Vec::new();
    let mut started = Instant::now();
    while !changes.stopped.load(Ordering::Acquire) {
        if !line.is_empty() && started.elapsed() > frame_timeout {
            break;
        }
        match stdout.fill_buf() {
            Ok([]) => break,
            Ok(buf) => {
                if line.is_empty() {
                    started = Instant::now();
                }
                let n = buf
                    .iter()
                    .position(|b| *b == b'\n')
                    .map(|i| i + 1)
                    .unwrap_or(buf.len());
                if line.len() + n > 8 * 1024 * 1024 {
                    break;
                }
                line.extend_from_slice(&buf[..n]);
                stdout.consume(n);
                if line.last() != Some(&b'\n') {
                    continue;
                }
                let Ok(value) = serde_json::from_slice::<Value>(&line) else {
                    break;
                };
                line.clear();
                if let Some(reply) = super::protocol::server_reply(&value) {
                    if write_msg(&stdin, reply, &changes).is_err() {
                        break;
                    }
                } else if value.get("id").is_some() {
                    // Bounded channel: unsolicited response floods terminate the reader.
                    if tx.try_send(value).is_err() {
                        break;
                    }
                } else {
                    changes.notify(&value);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(_) => break,
        }
    }
    changes.failed.store(true, Ordering::Release);
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
    listed_generation: u64,
}

impl AttachedServer {
    pub(crate) fn from_http(
        name: String,
        transport: String,
        tools: Vec<ToolInfo>,
        http: HttpClient,
        spec: ServerSpec,
    ) -> Self {
        let listed_generation = http.listed_generation;
        Self {
            name,
            transport,
            tools,
            child: None,
            client: None,
            http: Some(http),
            spec,
            refreshing: false,
            listed_generation,
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
    fn changes(&self) -> Option<&Changes> {
        self.client
            .as_ref()
            .map(|c| c.changes.as_ref())
            .or_else(|| self.http.as_ref().map(|c| c.changes.as_ref()))
    }

    #[cfg(test)]
    pub(super) fn http_changes_for_test(&self) -> u64 {
        self.changes().unwrap().generation()
    }

    pub fn needs_refresh(&self) -> bool {
        !self.refreshing
            && self.changes().is_some_and(|c| {
                c.failed.load(Ordering::Acquire) || c.generation() != self.listed_generation
            })
    }

    pub fn is_healthy(&self) -> bool {
        if self.needs_refresh() {
            return false;
        }

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
        if self
            .changes()
            .is_some_and(|c| c.failed.load(Ordering::Acquire))
        {
            self.disconnect();
            return Err(McpError::message(
                "MCP subscription disconnected; reconnect explicitly",
            ));
        }
        if self.client.as_ref().is_some_and(|c| !c.supports_tools)
            || self.http.as_ref().is_some_and(|c| !c.supports_tools)
        {
            self.tools.clear();
            return Ok(());
        }
        // Keep invalidations received during pagination pending for another refresh.
        let generation = self.changes().map(Changes::generation).unwrap_or(0);
        match super::discovery::list_tools(|params| self.request("tools/list", params)) {
            Ok(tools) => {
                self.tools = tools;
                self.listed_generation = generation;
                Ok(())
            }
            Err(error) => {
                self.disconnect();
                Err(error)
            }
        }
    }

    pub fn call_tool(&mut self, tool: &str, arguments: Value) -> Result<Value, McpError> {
        if self.needs_refresh() {
            return Err(McpError::message(
                "MCP catalog invalidated; wait for refresh or reconnect",
            ));
        }
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
            listed_generation: 0,
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

    let listed_generation = client.listed_generation;
    Ok(AttachedServer {
        name: name.to_string(),
        transport: "stdio".into(),
        tools,
        child: Some(child),
        client: Some(client),
        http: None,
        spec: spec.clone(),
        refreshing: false,
        listed_generation,
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
    let flags = unsafe { libc::fcntl(stdin.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(stdin.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(McpError::message("MCP cannot configure stdin"));
    }
    let stdin = Arc::new(Mutex::new(stdin));
    let writer = Arc::clone(&stdin);
    let changes = Arc::new(Changes::default());
    let (tx, responses) = mpsc::sync_channel(2);
    let shared = Arc::clone(&changes);
    let reader = std::thread::spawn(move || {
        read_background(
            BufReader::new(stdout),
            writer,
            tx,
            shared,
            std::time::Duration::from_secs(15),
        )
    });
    let mut client = StdioClient {
        stdin,
        responses,
        changes,
        reader: Some(reader),
        supports_tools: false,
        listed_generation: 0,
        next_id: 1,
    };
    let init = client.call(name, "initialize", json!({"protocolVersion":super::protocol::VERSION,"capabilities":{},"clientInfo":{"name":"hypermesh-visor","version":"0.1.0"}}))?;
    let negotiated = super::protocol::negotiate(&init)?;
    client.supports_tools = negotiated.tools;
    client
        .changes
        .enabled
        .store(negotiated.list_changed, Ordering::Release);
    write_msg(
        &client.stdin,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        &client.changes,
    )?;
    let generation = client.changes.generation();
    let tools = if client.supports_tools {
        super::discovery::list_tools(|params| client.call(name, "tools/list", params))?
    } else {
        Vec::new()
    };
    client.listed_generation = generation;
    Ok((tools, client))
}

fn write_msg(stdin: &Mutex<ChildStdin>, value: Value, changes: &Changes) -> Result<(), McpError> {
    use std::time::{Duration, Instant};
    let mut raw = serde_json::to_vec(&value).map_err(|_| McpError::message("MCP encode failed"))?;
    if raw.len() > 8 * 1024 * 1024 {
        return Err(McpError::message("MCP request too large"));
    }
    raw.push(b'\n');
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut writer = stdin
        .lock()
        .map_err(|_| McpError::message("MCP writer unavailable"))?;
    let mut offset = 0;
    while offset < raw.len() {
        if changes.stopped.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err(McpError::message("MCP write stopped or timed out"));
        }
        match writer.write(&raw[offset..]) {
            Ok(0) => return Err(McpError::message("MCP stdin closed")),
            Ok(n) => offset += n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(_) => return Err(McpError::message("MCP write failed")),
        }
    }
    Ok(())
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

    fn python_spec(script: &str) -> ServerSpec {
        ServerSpec {
            r#type: Some("stdio".into()),
            command: Some("python3".into()),
            args: vec!["-u".into(), "-c".into(), script.into()],
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            cwd: None,
            enabled: None,
        }
    }

    #[test]
    fn no_tools_capability_skips_discovery_and_refresh() {
        let spec = python_spec(
            r#"
import sys,json
for line in sys.stdin:
 m=json.loads(line)
 assert m['method'] in ['initialize','notifications/initialized']
 if 'id' in m: print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':{'protocolVersion':'2025-06-18','capabilities':{}}}),flush=True)
"#,
        );
        let mut server = attach_one("empty", &spec).unwrap();
        server.refresh(false).unwrap();
        assert!(server.tools.is_empty());
        assert!(!server.needs_refresh());
        assert!(!server.is_healthy());
    }

    #[test]
    fn notifications_during_pagination_remain_pending_until_next_pass() {
        let spec = python_spec(
            r#"
import sys,json
lists=0
for line in sys.stdin:
 m=json.loads(line)
 if 'id' not in m: continue
 if m['method']=='initialize': result={'protocolVersion':'2025-06-18','capabilities':{'tools':{'listChanged':True}}}
 else:
  lists+=1
  if lists in [1,4]: print(json.dumps({'jsonrpc':'2.0','method':'notifications/tools/list_changed'}),flush=True)
  result={'tools':[{'name':'b' if 'cursor' in m['params'] else 'a'}]}
  if 'cursor' not in m['params']: result['nextCursor']='page2'
 print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':result}),flush=True)
"#,
        );
        let mut server = attach_one("changing", &spec).unwrap();
        assert!(server.needs_refresh());
        assert!(server.call_tool("a", json!({})).is_err());
        server.refresh(false).unwrap();
        assert!(server.needs_refresh(), "in-flight change was lost");
        server.refresh(false).unwrap();
        assert!(!server.needs_refresh());
        assert!(server.is_healthy());
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
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        unsafe {
            libc::fcntl(stdout.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
        }
        let stdin = Arc::new(Mutex::new(child.stdin.take().unwrap()));
        let changes = Arc::new(Changes::default());
        let shared = Arc::clone(&changes);
        let (tx, rx) = mpsc::sync_channel(2);
        let reader = std::thread::spawn(move || {
            read_background(
                BufReader::new(stdout),
                stdin,
                tx,
                shared,
                std::time::Duration::from_millis(100),
            )
        });
        assert!(matches!(
            rx.recv_timeout(std::time::Duration::from_secs(1)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
        reader.join().unwrap();
        assert!(changes.failed.load(Ordering::Acquire));
        let _ = child.kill();
        let _ = child.wait();
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
 if method=='initialize': result={'protocolVersion':'2024-11-05','capabilities':{'tools':{'listChanged':False}}}
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
                command: Some("python3".into()),
                args: vec![script.to_string_lossy().into_owned()],
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
