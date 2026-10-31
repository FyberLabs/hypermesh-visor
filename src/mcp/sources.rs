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
        out.push(Location {
            id: "zed_user".into(),
            label: "Zed user settings".into(),
            path: config.join("zed/settings.json"),
            status: "missing",
        });
    }
    if let Some(home) = home.filter(|p| p.is_absolute()) {
        out.push(Location {
            id: "devin_user".into(),
            label: "Devin Local user configuration".into(),
            path: home.join(".config/devin/mcp_config.json"),
            status: "missing",
        });
    }
    if let Some(home) = home.filter(|p| p.is_absolute()) {
        out.push(Location {
            id: "junie_user".into(),
            label: "Junie user configuration".into(),
            path: home.join(".junie/mcp/mcp.json"),
            status: "missing",
        });
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
        "selected_project"
            | "cursor_global"
            | "vscode_user"
            | "vscode_insiders_user"
            | "zed_user"
            | "devin_user"
            | "junie_user"
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
    let keys: Vec<_> = ["mcpServers", "servers", "context_servers"]
        .into_iter()
        .filter(|key| object.contains_key(*key))
        .collect();
    if keys.len() != 1 {
        return Err(McpError::message(
            "MCP import requires exactly one servers, mcpServers, or context_servers map",
        ));
    }
    let key = keys[0];
    // Zed stores MCP definitions in its general settings file. Only ignore
    // explicitly harmless presentation settings; never drop policy or layers.
    if object.keys().any(|k| {
        k != key
            && k != "$schema"
            && !(key == "context_servers"
                && matches!(
                    k.as_str(),
                    "theme"
                        | "icon_theme"
                        | "buffer_font_family"
                        | "buffer_font_size"
                        | "ui_font_family"
                        | "ui_font_size"
                        | "vim_mode"
                        | "tab_size"
                        | "soft_wrap"
                        | "show_whitespaces"
                ))
    }) {
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
        if let Some(transport) = entry.remove("transport") {
            if entry.contains_key("type") || transport != "http" {
                return Err(McpError::message(
                    "Unsupported or conflicting MCP transport",
                ));
            }
            entry.insert("type".into(), transport);
        }
        // External editors use `sse` for the legacy endpoint-discovery
        // protocol, unlike visor's historical POST-based internal alias.
        if let Some(transport) = entry.get_mut("type") {
            match transport.as_str() {
                Some("streamable-http") => *transport = json!("http"),
                Some("stdio" | "http") => {}
                _ => {
                    return Err(McpError::message(
                        "Unsupported MCP import transport; use stdio or Streamable HTTP",
                    ))
                }
            }
        }
        if key == "context_servers" {
            let command = entry
                .get("command")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty());
            let url = entry
                .get("url")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty());
            if command.is_some() == url.is_some()
                || entry.get("command").is_some_and(|v| !v.is_string())
                || entry.get("url").is_some_and(|v| !v.is_string())
            {
                return Err(McpError::message("Zed import requires a custom command or URL server; export extension definitions explicitly"));
            }
        }
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
        assert_eq!(sources.len(), 6);
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
                "vscode_insiders_user",
                "zed_user"
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
    fn additional_editor_locations_use_documented_roots_without_reading_files() {
        let dir = std::env::temp_dir().join(format!("visor-editors-{}", uuid::Uuid::new_v4()));
        let xdg = dir.join("custom");
        for path in [
            xdg.join("zed/settings.json"),
            dir.join(".config/devin/mcp_config.json"),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "not JSON: metadata only").unwrap();
        }
        let sources = locations(Some(&dir), Some(&xdg));
        for (id, path) in [
            ("zed_user", xdg.join("zed/settings.json")),
            ("devin_user", dir.join(".config/devin/mcp_config.json")),
        ] {
            let source = sources.iter().find(|s| s.id == id).unwrap();
            assert_eq!(source.path, path);
            assert_eq!(source.status, "available_for_review");
            assert!(valid_source_kind(id));
        }
        let fallback = locations(Some(&dir), Some(Path::new("relative")));
        assert_eq!(
            fallback.iter().find(|s| s.id == "zed_user").unwrap().path,
            dir.join(".config/zed/settings.json")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn zed_custom_servers_and_devin_http_import_preserve_controls() {
        let raw = r#"{// ordinary Zed presentation settings
            "theme":"One Dark", "vim_mode":true,
            "context_servers":{
                "local":{"command":"never-run","args":["private"],"enabled":false},
                "remote":{"url":"https://example.com/mcp","headers":{"Authorization":"private"}},
            },
        }"#;
        let parsed: Value = serde_json::from_str(&normalize(raw).unwrap()).unwrap();
        assert_eq!(parsed["mcpServers"]["local"]["enabled"], false);
        assert_eq!(
            parsed["mcpServers"]["remote"]["headers"]["Authorization"],
            "private"
        );
        let parsed: Value = serde_json::from_str(
            &normalize(
                r#"{"mcpServers":{"remote":{
            "url":"https://example.com/mcp","transport":"http","disabled":true}}}"#,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["mcpServers"]["remote"]["type"], "http");
        assert_eq!(parsed["mcpServers"]["remote"]["enabled"], false);
        for raw in [
            r#"{"context_servers":{},"servers":{}}"#,
            r#"{"context_servers":{},"nightly":{"context_servers":{}}}"#,
            r#"{"context_servers":{},"agent":{"tool_permissions":{}}}"#,
            r#"{"context_servers":{"extension":{"enabled":true}}}"#,
            r#"{"context_servers":{"legacy":{"command":{"path":"x"}}}}"#,
            r#"{"context_servers":{"both":{"command":"x","url":"https://example.com"}}}"#,
            r#"{"context_servers":{"extension":{"command":"x","settings":{}}}}"#,
            r#"{"mcpServers":{"remote":{"url":"https://example.com","transport":"sse"}}}"#,
            r#"{"mcpServers":{"remote":{"url":"https://example.com","transport":"http","type":"http"}}}"#,
            r#"{"mcpServers":{"remote":{"url":"https://example.com","oauthClientId":"private"}}}"#,
            r#"{"mcpServers":{"local":{"command":"x","env":{"TOKEN":"${file:/secret}"}}}}"#,
        ] {
            assert!(normalize(raw).is_err(), "accepted {raw}");
        }
    }

    #[test]
    fn junie_source_is_user_scoped_metadata_only() {
        let dir = std::env::temp_dir().join(format!("visor-junie-{}", uuid::Uuid::new_v4()));
        let path = dir.join(".junie/mcp/mcp.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not parsed while listing").unwrap();
        let sources = locations(Some(&dir), Some(&dir.join("xdg")));
        let source = sources.iter().find(|s| s.id == "junie_user").unwrap();
        assert_eq!(source.path, path);
        assert_eq!(source.status, "available_for_review");
        assert!(valid_source_kind("junie_user"));
        assert!(!valid_source_kind("junie_user:../project"));
        assert!(!locations(None, Some(&dir))
            .iter()
            .any(|s| s.id == "junie_user"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn imports_explicit_transports_without_reinterpreting_legacy_sse() {
        for key in ["mcpServers", "servers", "context_servers"] {
            let raw =
                json!({key:{"remote":{"type":"streamable-http","url":"https://example.com/mcp"}}});
            let parsed: Value =
                serde_json::from_str(&normalize(&raw.to_string()).unwrap()).unwrap();
            assert_eq!(parsed["mcpServers"]["remote"]["type"], "http");
            for transport in [
                json!("sse"),
                json!("unknown"),
                json!(""),
                json!(null),
                json!(3),
            ] {
                let raw =
                    json!({key:{"remote":{"type":transport,"url":"https://example.com/mcp"}}});
                assert!(normalize(&raw.to_string()).is_err());
            }
        }
        for field in ["timeout", "oauth", "autoApprove"] {
            let raw = json!({"mcpServers":{"remote":{"url":"https://example.com/mcp",field:1000}}});
            assert!(normalize(&raw.to_string()).is_err(), "discarded {field}");
        }
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
