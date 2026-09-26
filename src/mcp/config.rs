use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::McpError;

#[derive(Debug, Clone, Deserialize)]
pub struct ServersFile {
    #[serde(rename = "mcpServers", default)]
    pub servers: BTreeMap<String, ServerSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerSpec {
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

impl ServerSpec {
    pub fn transport(&self) -> &str {
        if let Some(t) = self.r#type.as_deref() {
            let t = t.trim();
            if !t.is_empty() {
                return t;
            }
        }
        if self.url.as_deref().unwrap_or("").trim().is_empty() {
            "stdio"
        } else {
            "http"
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ProfilesFile {
    #[serde(default = "default_active")]
    active: String,
    #[serde(default)]
    profiles: BTreeMap<String, Profile>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct Profile {
    #[serde(default)]
    servers: Vec<String>,
    #[serde(default)]
    config: BTreeMap<String, BTreeMap<String, String>>,
    /// When true, attach only the Docker MCP gateway process for this profile.
    #[serde(default)]
    gateway: bool,
}

fn default_active() -> String {
    "default".into()
}

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("HYPERMESH_CONFIG_DIR") {
        let dir = dir.trim();
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let xdg = xdg.trim();
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("hypermesh");
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config/hypermesh");
    }
    PathBuf::from(".config/hypermesh")
}

/// Load one named server from `mcp.json` for vault secret fetch (role B).
/// Profile membership is not required; disabled servers are rejected.
pub fn load_named_server(dir: &Path, id: &str) -> Result<ServerSpec, McpError> {
    let id = id.trim();
    if id.is_empty() {
        return Err(McpError::message("mcp server id is required"));
    }
    let servers = read_servers(&dir.join("mcp.json"))?;
    let spec = servers.servers.get(id).cloned().ok_or_else(|| {
        McpError::message(format!("mcp server \"{id}\" not found in mcp.json"))
    })?;
    if !spec.is_enabled() {
        return Err(McpError::message(format!("mcp server \"{id}\" is disabled")));
    }
    Ok(spec)
}

/// Load enabled servers for `profile_name` (or the active profile when empty).
/// Missing config files yield an empty map (no attach).
/// Merges project-local `mcp.json` (`.hypermesh/` or `.cursor/`) when present.
pub fn load_profile_servers(
    dir: &Path,
    profile_name: Option<&str>,
) -> Result<(String, BTreeMap<String, ServerSpec>), McpError> {
    let servers_path = dir.join("mcp.json");
    let profiles_path = dir.join("mcp-profiles.json");
    if !servers_path.exists() && !profiles_path.exists() && project_mcp_path().is_none() {
        let name = profile_name.unwrap_or("default").to_string();
        return Ok((name, BTreeMap::new()));
    }
    let mut servers = read_servers(&servers_path)?;
    if let Some(project) = project_mcp_path() {
        let project_servers = read_servers(&project)?;
        for (id, spec) in project_servers.servers {
            servers.servers.entry(id).or_insert(spec);
        }
    }
    let profiles = read_profiles(&profiles_path)?;
    let name = profile_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(profiles.active.as_str())
        .to_string();
    let profile = profiles.profiles.get(&name).ok_or_else(|| {
        McpError::message(format!("mcp profile \"{name}\" not found"))
    })?;

    if profile.gateway {
        let id = "docker-gateway";
        let mut spec = servers.servers.get(id).cloned().unwrap_or(ServerSpec {
            r#type: Some("stdio".into()),
            command: Some("docker".into()),
            args: vec!["mcp".into(), "gateway".into(), "run".into()],
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            cwd: None,
            enabled: Some(true),
        });
        if let Some(cfg) = profile.config.get(id) {
            apply_config(&mut spec, cfg);
        }
        let mut out = BTreeMap::new();
        if spec.is_enabled() {
            out.insert(id.to_string(), spec);
        }
        return Ok((name, out));
    }

    let mut out = BTreeMap::new();
    for id in &profile.servers {
        let Some(mut spec) = servers.servers.get(id).cloned() else {
            return Err(McpError::message(format!(
                "mcp profile \"{name}\" names unknown server \"{id}\""
            )));
        };
        if let Some(cfg) = profile.config.get(id) {
            apply_config(&mut spec, cfg);
        }
        if !spec.is_enabled() {
            continue;
        }
        out.insert(id.clone(), spec);
    }
    Ok((name, out))
}

/// Walk cwd (and parents) for project MCP config, or `HYPERMESH_MCP_PROJECT`.
pub fn project_mcp_path() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("HYPERMESH_MCP_PROJECT") {
        let p = PathBuf::from(explicit.trim());
        if p.is_file() {
            return Some(p);
        }
        let nested = p.join("mcp.json");
        if nested.is_file() {
            return Some(nested);
        }
    }
    let mut dir = std::env::current_dir().ok()?;
    for _ in 0..8 {
        for rel in [".hypermesh/mcp.json", ".cursor/mcp.json"] {
            let candidate = dir.join(rel);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

fn read_servers(path: &Path) -> Result<ServersFile, McpError> {
    if !path.exists() {
        return Ok(ServersFile {
            servers: BTreeMap::new(),
        });
    }
    let raw = fs::read_to_string(path)
        .map_err(|e| McpError::message(format!("read {}: {e}", path.display())))?;
    serde_json::from_str(&raw)
        .map_err(|e| McpError::message(format!("parse {}: {e}", path.display())))
}

fn read_profiles(path: &Path) -> Result<ProfilesFile, McpError> {
    if !path.exists() {
        return Ok(ProfilesFile {
            active: "default".into(),
            profiles: BTreeMap::from([(
                "default".into(),
                Profile::default(),
            )]),
        });
    }
    let raw = fs::read_to_string(path)
        .map_err(|e| McpError::message(format!("read {}: {e}", path.display())))?;
    serde_json::from_str(&raw)
        .map_err(|e| McpError::message(format!("parse {}: {e}", path.display())))
}

fn apply_config(spec: &mut ServerSpec, cfg: &BTreeMap<String, String>) {
    if let Some(v) = cfg.get("command") {
        spec.command = Some(v.clone());
    }
    if let Some(v) = cfg.get("url") {
        spec.url = Some(v.clone());
    }
    if let Some(v) = cfg.get("cwd") {
        spec.cwd = Some(v.clone());
    }
    if let Some(v) = cfg.get("type") {
        spec.r#type = Some(v.clone());
    }
    if let Some(v) = cfg.get("args") {
        spec.args = v.split_whitespace().map(str::to_string).collect();
    }
    for (k, v) in cfg {
        if let Some(name) = k.strip_prefix("env.") {
            spec.env.insert(name.to_string(), v.clone());
        }
        if let Some(name) = k.strip_prefix("headers.") {
            spec.headers.insert(name.to_string(), v.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn loads_active_profile_servers() {
        let dir = tempfile_dir();
        write(
            &dir.join("mcp.json"),
            r#"{"mcpServers":{"fixture":{"command":"true"},"skip":{"command":"false","enabled":false}}}"#,
        );
        write(
            &dir.join("mcp-profiles.json"),
            r#"{"active":"default","profiles":{"default":{"servers":["fixture","skip"]}}}"#,
        );
        let (name, servers) = load_profile_servers(&dir, None).unwrap();
        assert_eq!(name, "default");
        assert!(servers.contains_key("fixture"));
        assert!(!servers.contains_key("skip"));
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hm-mcp-cfg-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn gateway_profile_attaches_only_docker_gateway() {
        let dir = tempfile_dir();
        write(
            &dir.join("mcp.json"),
            r#"{"mcpServers":{"filesystem":{"command":"true"},"docker-gateway":{"command":"docker","args":["mcp","gateway","run"]}}}"#,
        );
        write(
            &dir.join("mcp-profiles.json"),
            r#"{"active":"default","profiles":{"default":{"servers":["filesystem"],"gateway":true}}}"#,
        );
        let (name, servers) = load_profile_servers(&dir, None).unwrap();
        assert_eq!(name, "default");
        assert_eq!(servers.len(), 1);
        assert!(servers.contains_key("docker-gateway"));
    }

    fn write(path: &Path, body: &str) {
        let mut f = fs::File::create(path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }
}
