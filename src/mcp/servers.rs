//! Write-only patches for private server fields; listing exposes metadata only.
use super::{config::ServerSpec, profiles, trust::TrustStore, trust_disk, McpError};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

fn error(s: &str) -> McpError {
    McpError::message(s)
}
fn document(raw: Option<&[u8]>) -> Result<Value, McpError> {
    let v: Value = raw
        .map(serde_json::from_slice)
        .transpose()
        .map_err(|_| error("Invalid MCP server JSON"))?
        .unwrap_or_else(|| json!({"mcpServers":{}}));
    if !v.is_object() || !v["mcpServers"].is_object() {
        return Err(error("MCP server configuration requires an mcpServers map"));
    }
    for (name, spec) in v["mcpServers"].as_object().unwrap() {
        profiles::valid_name(name)?;
        serde_json::from_value::<ServerSpec>(spec.clone())
            .map_err(|_| error("Invalid MCP server definition"))?;
    }
    Ok(v)
}
fn snapshot(v: &Value, revision: String) -> Result<Value, McpError> {
    let mut servers = Vec::new();
    for (name, raw) in v["mcpServers"].as_object().unwrap() {
        let s: ServerSpec = serde_json::from_value(raw.clone())
            .map_err(|_| error("Invalid MCP server definition"))?;
        let origin = s
            .url
            .as_deref()
            .and_then(|s| url::Url::parse(s).ok())
            .map(|u| u.origin().ascii_serialization());
        servers.push(json!({"name":name,"transport":s.transport(),"enabled":s.is_enabled(),
            "command":s.command,"cwd":s.cwd,"endpoint":origin,"argument_count":s.args.len(),
            "environment_names":s.env.keys().collect::<Vec<_>>(),"header_names":s.headers.keys().collect::<Vec<_>>()}));
    }
    let result = json!({"revision":revision,"servers":servers});
    if result.to_string().len() > 200 * 1024 {
        return Err(error("Server list exceeds 200 KiB"));
    }
    Ok(result)
}
pub fn list(base: &Path) -> Result<Value, McpError> {
    let raw = profiles::read(&base.join("mcp.json"))?;
    snapshot(
        &document(raw.as_deref())?,
        profiles::revision(raw.as_deref()),
    )
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    Save {
        revision: String,
        name: String,
        create: bool,
        fields: Value,
    },
    Delete {
        revision: String,
        name: String,
    },
}
pub fn edit(base: &Path, trust: &mut TrustStore, edit: Edit) -> Result<Value, McpError> {
    let lock = profiles::config_lock(base)?;
    let _unlock = trust_disk::UnlockOnDrop(&lock);
    let path = base.join("mcp.json");
    let raw = profiles::read(&path)?;
    let mut v = document(raw.as_deref())?;
    let (rev, name) = match &edit {
        Edit::Save { revision, name, .. } | Edit::Delete { revision, name } => (revision, name),
    };
    profiles::valid_name(name)?;
    if *rev != profiles::revision(raw.as_deref()) {
        return Err(error("MCP servers changed; reopen the editor"));
    }
    let servers = v["mcpServers"].as_object_mut().unwrap();
    match edit {
        Edit::Save {
            name,
            create,
            fields,
            ..
        } => {
            if create == servers.contains_key(&name) {
                return Err(error(
                    "Server already exists or was removed; reopen the editor",
                ));
            }
            let fields = fields
                .as_object()
                .ok_or_else(|| error("Server fields must be an object"))?;
            if fields.is_empty() {
                return Err(error("No server fields supplied"));
            }
            if fields.keys().any(|k| {
                !matches!(
                    k.as_str(),
                    "type" | "command" | "args" | "cwd" | "url" | "env" | "headers" | "enabled"
                )
            }) {
                return Err(error("Unsupported server edit field"));
            }
            if create && servers.len() >= 128 {
                return Err(error("At most 128 servers in the graphical editor"));
            }
            let server = servers.entry(name).or_insert(json!({}));
            let object = server
                .as_object_mut()
                .ok_or_else(|| error("Invalid server definition"))?;
            for (key, value) in fields {
                if value.is_null() {
                    object.remove(key);
                } else {
                    object.insert(key.clone(), value.clone());
                }
            }
            let spec: ServerSpec = serde_json::from_value(server.clone())
                .map_err(|_| error("Invalid server field types"))?;
            match spec.transport() {
                "stdio"
                    if spec
                        .command
                        .as_deref()
                        .is_some_and(|s| !s.trim().is_empty() && !s.contains('\0')) => {}
                "http" | "sse" => {
                    let u = spec
                        .url
                        .as_deref()
                        .and_then(|s| url::Url::parse(s).ok())
                        .ok_or_else(|| error("HTTP server requires a valid URL"))?;
                    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
                        return Err(error("Server URL must use HTTP or HTTPS"));
                    }
                }
                _ => {
                    return Err(error(
                        "Server requires a stdio command or HTTP/SSE endpoint",
                    ))
                }
            }
        }
        Edit::Delete { name, .. } => {
            if servers.remove(&name).is_none() {
                return Err(error("Server not found"));
            }
        }
    }
    let data = serde_json::to_vec_pretty(&v).map_err(|_| error("Cannot encode MCP servers"))?;
    if data.len() > 256 * 1024 {
        return Err(error("MCP server configuration exceeds 256 KiB"));
    }
    let result = snapshot(&v, profiles::revision(Some(&data)))?;
    let profile_raw = profiles::read(&base.join("mcp-profiles.json"))?;
    let profile_doc = profiles::document(profile_raw.as_deref())?;
    let mut affected: Vec<String> = profile_doc["profiles"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    profiles::valid_name(profile_doc["active"].as_str().unwrap())?;
    affected.push(profile_doc["active"].as_str().unwrap().to_string());
    trust.sync(base)?;
    affected.extend(trust.list().into_iter().map(|p| p.profile));
    affected.sort();
    affected.dedup();
    trust.block_profiles(&affected)?;
    // Shared lock coordinates graphical profile/server editors; detect outside writers too.
    if profiles::read(&path)? != raw
        || profiles::read(&base.join("mcp-profiles.json"))? != profile_raw
    {
        return Err(error("MCP configuration changed; reopen the editor"));
    }
    profiles::atomic_write(base, "mcp.json", &data)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    fn fixture() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("visor-server-edit-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("mcp.json"),json!({"extra":"preserve-root","mcpServers":{
            "local":{"command":"never-run","args":["private-arg"],"env":{"TOKEN":"private-env"},"headers":{"Authorization":"private-header"},"future":"preserve-server"},
            "remote":{"type":"http","url":"https://user:private-password@example.com/private-path?token=private-query"}
        }}).to_string()).unwrap();
        fs::write(
            dir.join("mcp-profiles.json"),
            json!({"active":"a","profiles":{"a":{"servers":["local"]},"b":{"servers":["remote"]}}})
                .to_string(),
        )
        .unwrap();
        dir
    }
    fn save(dir: &Path, fields: Value) -> Edit {
        Edit::Save {
            revision: list(dir).unwrap()["revision"].as_str().unwrap().into(),
            name: "local".into(),
            create: false,
            fields,
        }
    }
    #[test]
    fn metadata_is_redacted_and_patches_preserve_secrets_and_unknown_fields() {
        let dir = fixture();
        let before = list(&dir).unwrap();
        for secret in [
            "private-arg",
            "private-env",
            "private-header",
            "private-password",
            "private-path",
            "private-query",
        ] {
            assert!(!before.to_string().contains(secret), "leaked {secret}");
        }
        let mut trust = TrustStore::default();
        let after = edit(&dir, &mut trust, save(&dir, json!({"enabled":false}))).unwrap();
        assert!(!after.to_string().contains("private-"));
        let saved: Value =
            serde_json::from_slice(&fs::read(dir.join("mcp.json")).unwrap()).unwrap();
        assert_eq!(saved["extra"], "preserve-root");
        assert_eq!(saved["mcpServers"]["local"]["future"], "preserve-server");
        assert_eq!(saved["mcpServers"]["local"]["env"]["TOKEN"], "private-env");
        assert_eq!(saved["mcpServers"]["local"]["args"][0], "private-arg");
        assert!(trust.is_revoked("a") && trust.is_revoked("b"));
        let mut restarted = TrustStore::default();
        restarted.sync(&dir).unwrap();
        assert!(restarted.is_revoked("a") && restarted.is_revoked("b"));
        assert!(edit(
            &dir,
            &mut trust,
            Edit::Delete {
                revision: before["revision"].as_str().unwrap().into(),
                name: "local".into()
            }
        )
        .is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn replacements_delete_and_new_servers_require_review_without_changing_membership() {
        let dir = fixture();
        let profiles = fs::read(dir.join("mcp-profiles.json")).unwrap();
        let mut trust = TrustStore::default();
        edit(
            &dir,
            &mut trust,
            save(
                &dir,
                json!({"env":{},"args":["replacement"],"headers":null}),
            ),
        )
        .unwrap();
        let v: Value = serde_json::from_slice(&fs::read(dir.join("mcp.json")).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["local"]["env"], json!({}));
        assert!(v["mcpServers"]["local"].get("headers").is_none());
        let rev = list(&dir).unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_string();
        edit(
            &dir,
            &mut trust,
            Edit::Delete {
                revision: rev,
                name: "local".into(),
            },
        )
        .unwrap();
        let rev = list(&dir).unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_string();
        edit(
            &dir,
            &mut trust,
            Edit::Save {
                revision: rev,
                name: "created".into(),
                create: true,
                fields: json!({"type":"http","url":"https://example.com/new","enabled":true}),
            },
        )
        .unwrap();
        assert_eq!(fs::read(dir.join("mcp-profiles.json")).unwrap(), profiles);
        assert!(trust.is_revoked("a") && trust.is_revoked("b"));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn invalid_edits_and_trust_capacity_failure_do_not_replace_files() {
        let dir = fixture();
        let original = fs::read(dir.join("mcp.json")).unwrap();
        let mut trust = TrustStore::default();
        for fields in [
            json!({"unknown":"value"}),
            json!({"args":{}}),
            json!({"env":{"TOKEN":7}}),
            json!({"type":"http","url":"file:///secret"}),
            json!({"command":null}),
        ] {
            assert!(edit(&dir, &mut trust, save(&dir, fields)).is_err());
            assert_eq!(fs::read(dir.join("mcp.json")).unwrap(), original);
        }
        trust.sync(&dir).unwrap();
        trust
            .block_profiles(&(0..8).map(|i| format!("old-{i}")).collect::<Vec<_>>())
            .unwrap();
        assert!(edit(&dir, &mut trust, save(&dir, json!({"enabled":false}))).is_err());
        assert_eq!(fs::read(dir.join("mcp.json")).unwrap(), original);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn reads_are_side_effect_free_and_file_links_are_refused() {
        let dir =
            std::env::temp_dir().join(format!("visor-server-absent-{}", uuid::Uuid::new_v4()));
        assert_eq!(list(&dir).unwrap()["revision"], "missing");
        assert!(!dir.exists());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("target"), "{}").unwrap();
        std::os::unix::fs::symlink(dir.join("target"), dir.join("mcp.json")).unwrap();
        assert!(list(&dir).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
