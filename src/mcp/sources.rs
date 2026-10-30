//! Known user locations only. Listing metadata never imports or executes servers.
use super::McpError;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
#[derive(Serialize)]
pub struct Location {
    pub id: String,
    pub label: String,
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
            id: "cursor_global".into(),
            label: "Cursor user configuration".into(),
            path: home.join(".cursor/mcp.json"),
            status: "missing",
        });
    }
    if let Some(config) = xdg
        .filter(|p| p.is_absolute())
        .map(Path::to_path_buf)
        .or_else(|| home.filter(|p| p.is_absolute()).map(|p| p.join(".config")))
    {
        for (product, id, label) in [
            ("Code", "vscode", "VS Code"),
            ("Code - Insiders", "vscode_insiders", "VS Code Insiders"),
        ] {
            let user = config.join(product).join("User");
            out.push(Location {
                id: format!("{id}_user"),
                label: format!("{label} default user profile"),
                path: user.join("mcp.json"),
                status: "missing",
            });
            add_profiles(&mut out, &user.join("profiles"), id, label);
        }
    }
    for location in &mut out {
        if location.status == "use_explicit_path" {
            continue;
        }
        location.status = match std::fs::metadata(&location.path) {
            Ok(m) if m.is_file() => "available_for_review",
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => "missing",
            _ => "unavailable",
        };
    }
    out
}
// Read only one bounded directory level. IDs are the editor's storage IDs, not
// display names or the visor profile names that control server membership.
fn profile_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}
pub(super) fn valid_source_kind(kind: &str) -> bool {
    matches!(
        kind,
        "selected_project" | "cursor_global" | "vscode_user" | "vscode_insiders_user"
    ) || ["vscode_profile:", "vscode_insiders_profile:"]
        .iter()
        .any(|prefix| kind.strip_prefix(prefix).is_some_and(profile_id))
}
fn add_profiles(out: &mut Vec<Location>, root: &Path, id: &str, label: &str) {
    let notice = || Location {
        id: format!("{id}_profiles"),
        label: format!("{label} profiles: use an explicit file path"),
        path: root.to_path_buf(),
        status: "use_explicit_path",
    };
    match std::fs::symlink_metadata(root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Ok(m) if m.is_dir() => {}
        _ => {
            out.push(notice());
            return;
        }
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        out.push(notice());
        return;
    };
    let mut profiles = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= 128 {
            out.push(notice());
            return;
        }
        let Ok(entry) = entry else {
            out.push(notice());
            return;
        };
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| profile_id(name)) else {
            continue;
        };
        // Do not follow directory symlinks or infer default-profile inheritance.
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let path = entry.path().join("mcp.json");
        if path.as_os_str().len() > 4096 {
            continue;
        }
        if std::fs::symlink_metadata(&path).is_err() {
            continue;
        }
        profiles.push(Location {
            id: format!("{id}_profile:{name}"),
            label: format!("{label} profile {name}"),
            path,
            status: "missing",
        });
    }
    profiles.sort_by(|a, b| a.id.cmp(&b.id));
    if profiles.len() > 16 {
        out.push(notice());
    }
    out.extend(profiles.into_iter().take(16));
}

pub fn selected(id: &str) -> Result<PathBuf, McpError> {
    list()
        .into_iter()
        .find(|s| s.id == id && s.status != "use_explicit_path")
        .map(|s| s.path)
        .ok_or_else(|| McpError::message("Unknown or unavailable MCP source"))
}
/// Import a deliberately small executable configuration surface, never editor policy.
/// Unsupported variables, sandbox/auth/input settings must not be silently discarded.
pub(super) fn normalize(raw: &str) -> Result<String, McpError> {
    let value: Value = super::jsonc::parse(raw)?;
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
        assert_eq!(sources.len(), 3);
        assert_eq!(sources[0].status, "available_for_review");
        assert_eq!(sources[1].path, dir.join("xdg/Code/User/mcp.json"));
        assert_eq!(sources[1].status, "missing");
        assert!(locations(Some(Path::new("relative")), None).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn profiles_are_sorted_bounded_and_never_follow_directory_links() {
        let dir = std::env::temp_dir().join(format!("visor-profiles-{}", uuid::Uuid::new_v4()));
        let root = dir.join("Code/User/profiles");
        for name in ["z-last", "a-first", "empty", "nested"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        for name in ["z-last", "a-first"] {
            std::fs::write(root.join(name).join("mcp.json"), "not parsed by discovery").unwrap();
        }
        std::fs::create_dir_all(root.join("nested/hidden")).unwrap();
        std::fs::write(root.join("nested/hidden/mcp.json"), "{}").unwrap();
        std::os::unix::fs::symlink(root.join("a-first"), root.join("link")).unwrap();
        let sources = locations(None, Some(&dir));
        let ids: Vec<_> = sources.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "vscode_user",
                "vscode_profile:a-first",
                "vscode_profile:z-last",
                "vscode_insiders_user"
            ]
        );
        assert!(sources[1].path.ends_with("profiles/a-first/mcp.json"));
        for n in 0..20 {
            let profile = root.join(format!("p{n:02}"));
            std::fs::create_dir_all(&profile).unwrap();
            std::fs::write(profile.join("mcp.json"), "{}").unwrap();
        }
        let sources = locations(None, Some(&dir));
        assert_eq!(
            sources
                .iter()
                .filter(|s| s.id.starts_with("vscode_profile:"))
                .count(),
            16
        );
        assert!(sources.iter().any(|s| s.status == "use_explicit_path"));
        for n in 0..129 {
            std::fs::write(root.join(format!("junk{n}")), "").unwrap();
        }
        let sources = locations(None, Some(&dir));
        assert!(!sources.iter().any(|s| s.id.starts_with("vscode_profile:")));
        assert!(sources.iter().any(|s| s.status == "use_explicit_path"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn profile_receipt_ids_cannot_encode_paths() {
        for id in [
            "vscode_profile:abc-123",
            "vscode_insiders_profile:abc_123",
            "vscode_insiders_user",
        ] {
            assert!(valid_source_kind(id));
        }
        for id in [
            "vscode_profile:",
            "vscode_profile:../x",
            "vscode_profile:a/b",
            "vscode_profile:a:b",
            "vscode_profiles",
        ] {
            assert!(!valid_source_kind(id));
        }
        assert!(!valid_source_kind(&format!(
            "vscode_profile:{}",
            "a".repeat(41)
        )));
    }
    #[test]
    fn jsonc_import_preserves_transport_and_security_validation() {
        let raw = r#"{/*editor configuration*/"servers":{"a":{"command":"never-run","args":["https://host/a//b",],},},}"#;
        let parsed: Value = serde_json::from_str(&normalize(raw).unwrap()).unwrap();
        assert_eq!(parsed["mcpServers"]["a"]["args"][0], "https://host/a//b");
        assert!(normalize(
            r#"{"servers":{"a":{"command":"x",/*policy*/"sandboxEnabled":true,},},}"#
        )
        .is_err());
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
        ] {
            assert!(normalize(raw).is_err(), "accepted {raw}");
        }
    }
}
