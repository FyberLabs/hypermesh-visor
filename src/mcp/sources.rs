//! Known user locations only. Listing metadata never imports or executes servers.
use super::McpError;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
#[derive(Serialize)]
pub struct Location {
    pub id: &'static str,
    pub label: &'static str,
    pub path: PathBuf,
    pub status: &'static str,
}
pub fn list() -> Vec<Location> {
    locations(
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .as_deref(),
    )
}
fn locations(home: Option<&Path>, xdg: Option<&Path>) -> Vec<Location> {
    let mut out = Vec::new();
    if let Some(home) = home.filter(|p| p.is_absolute()) {
        out.push(Location {
            id: "cursor_global",
            label: "Cursor user configuration",
            path: home.join(".cursor/mcp.json"),
            status: "missing",
        });
    }
    if let Some(config) = xdg
        .filter(|p| p.is_absolute())
        .map(Path::to_path_buf)
        .or_else(|| home.filter(|p| p.is_absolute()).map(|p| p.join(".config")))
    {
        out.push(Location {
            id: "vscode_user",
            label: "VS Code default user profile",
            path: config.join("Code/User/mcp.json"),
            status: "missing",
        });
    }
    for location in &mut out {
        location.status = match std::fs::metadata(&location.path) {
            Ok(m) if m.is_file() => "available_for_review",
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => "missing",
            _ => "unavailable",
        };
    }
    out
}
pub fn selected(id: &str) -> Result<PathBuf, McpError> {
    list()
        .into_iter()
        .find(|s| s.id == id)
        .map(|s| s.path)
        .ok_or_else(|| McpError::message("Unknown or unavailable MCP source"))
}
/// Import a deliberately small executable configuration surface, never editor policy.
/// Unsupported variables, sandbox/auth/input settings must not be silently discarded.
pub(super) fn normalize(raw: &str) -> Result<String, McpError> {
    let value: Value = serde_json::from_str(raw).map_err(|_| {
        McpError::message(
            "MCP import requires strict JSON; export JSON without comments or trailing commas",
        )
    })?;
    let object = value
        .as_object()
        .ok_or_else(|| McpError::message("MCP import must be an object"))?;
    let key = match (
        object.contains_key("mcpServers"),
        object.contains_key("servers"),
    ) {
        (true, false) => "mcpServers",
        (false, true) => "servers",
        _ => {
            return Err(McpError::message(
                "MCP import requires exactly one servers or mcpServers map",
            ))
        }
    };
    if object.keys().any(|k| k != key && k != "$schema") {
        return Err(McpError::message(
            "MCP import contains unsupported root settings",
        ));
    }
    let mut servers = object[key]
        .as_object()
        .cloned()
        .ok_or_else(|| McpError::message("MCP servers must be a map"))?;
    for server in servers.values_mut() {
        let entry = server
            .as_object_mut()
            .ok_or_else(|| McpError::message("Invalid MCP server"))?;
        if entry.keys().any(|k| {
            !matches!(
                k.as_str(),
                "type"
                    | "command"
                    | "args"
                    | "env"
                    | "url"
                    | "headers"
                    | "cwd"
                    | "enabled"
                    | "disabled"
            )
        }) {
            return Err(McpError::message(
                "MCP server has unsupported settings; review an explicit compatible export",
            ));
        }
        if let Some(disabled) = entry.remove("disabled") {
            if entry.contains_key("enabled") {
                return Err(McpError::message("Conflicting MCP enable flags"));
            }
            let disabled = disabled
                .as_bool()
                .ok_or_else(|| McpError::message("Invalid MCP disabled flag"))?;
            entry.insert("enabled".into(), json!(!disabled));
        }
        for (name, value) in entry.iter() {
            let text = value.to_string();
            if text.contains("${") {
                let allowed = name == "env"
                    && value.as_object().is_some_and(|env| {
                        env.values().all(|v| {
                            v.as_str().is_some_and(|s| {
                                !s.contains("${")
                                    || (s.starts_with("${env:")
                                        && s.ends_with('}')
                                        && !s[6..s.len() - 1].contains(['{', '}']))
                            })
                        })
                    });
                if !allowed {
                    return Err(McpError::message("Unsupported MCP variable expansion; provide an explicit compatible configuration"));
                }
            }
        }
    }
    Ok(json!({"mcpServers":servers}).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn known_sources_are_metadata_only_and_do_not_scan_projects() {
        let dir = std::env::temp_dir().join(format!("visor-sources-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(".cursor")).unwrap();
        std::fs::write(
            dir.join(".cursor/mcp.json"),
            "invalid JSON is not parsed by listing",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("nested/.cursor")).unwrap();
        std::fs::write(dir.join("nested/.cursor/mcp.json"), "must not discover").unwrap();
        let sources = locations(Some(&dir), Some(&dir.join("xdg")));
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].status, "available_for_review");
        assert_eq!(sources[1].path, dir.join("xdg/Code/User/mcp.json"));
        assert_eq!(sources[1].status, "missing");
        assert!(locations(Some(Path::new("relative")), None).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn imports_vscode_and_cursor_without_discarding_security_settings() {
        for key in ["servers", "mcpServers"] {
            let raw=json!({key:{"off":{"command":"never-run","disabled":true},"http":{"type":"http","url":"https://example.com/mcp"}}}).to_string();
            let parsed: Value = serde_json::from_str(&normalize(&raw).unwrap()).unwrap();
            assert_eq!(parsed["mcpServers"]["off"]["enabled"], false);
            assert_eq!(
                parsed["mcpServers"]["http"]["url"],
                "https://example.com/mcp"
            );
        }
        for raw in [
            r#"{"servers":{},"mcpServers":{}}"#,
            r#"{"servers":{},"sandbox":{"filesystem":{}}}"#,
            r#"{"servers":{"a":{"command":"x","sandboxEnabled":true}}}"#,
            r#"{"servers":{"a":{"command":"x","envFile":".env"}}}"#,
            r#"{"servers":{"a":{"url":"https://example.com","oauth":{}}}}"#,
            r#"{"servers":{"a":{"command":"x","args":["${workspaceFolder}"]}}}"#,
            r#"{"servers":{"a":{"command":"x","env":{"TOKEN":"${input:token}"}}}}"#,
            r#"{"servers":{"a":{"command":"x","enabled":true,"disabled":true}}}"#,
            "{ // comment\n \"servers\":{} }",
        ] {
            assert!(normalize(raw).is_err(), "accepted {raw}");
        }
    }
}
