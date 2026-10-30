//! Companion profile editor. Secrets/overrides stay on disk, never in responses.
use super::{trust::TrustStore, trust_disk, McpError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
};
const LIMIT: u64 = 256 * 1024;
fn error(message: &str) -> McpError {
    McpError::message(message)
}
fn read(path: &Path) -> Result<Option<Vec<u8>>, McpError> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(error("Cannot read MCP profile configuration")),
    };
    let meta = file
        .metadata()
        .map_err(|_| error("Cannot inspect MCP configuration"))?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.nlink() != 1 {
        return Err(error(
            "MCP configuration must be a same-user regular file without links",
        ));
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| error("Cannot read MCP configuration"))?;
    if bytes.len() as u64 > LIMIT {
        return Err(error("MCP configuration exceeds 256 KiB"));
    }
    Ok(Some(bytes))
}
fn revision(raw: Option<&[u8]>) -> String {
    raw.map(trust_disk::digest)
        .unwrap_or_else(|| "missing".into())
}
fn document(raw: Option<&[u8]>) -> Result<Value, McpError> {
    let mut v = raw
        .map(serde_json::from_slice)
        .transpose()
        .map_err(|_| error("Invalid profile JSON"))?
        .unwrap_or_else(|| json!({"active":"default","profiles":{}}));
    let o = v
        .as_object_mut()
        .ok_or_else(|| error("Invalid profile document"))?;
    o.entry("active").or_insert(json!("default"));
    o.entry("profiles").or_insert(json!({}));
    if !o["active"].is_string() || !o["profiles"].is_object() {
        return Err(error("Invalid profile document"));
    }
    for (name, p) in o["profiles"].as_object().unwrap() {
        valid_name(name)?;
        if !p.is_object()
            || p.get("servers").is_some_and(|s| !s.is_array())
            || p.get("gateway").is_some_and(|g| !g.is_boolean())
        {
            return Err(error("Invalid profile fields"));
        }
        for s in p
            .get("servers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            valid_name(s.as_str().ok_or_else(|| error("Invalid server ID"))?)?;
        }
    }
    Ok(v)
}
fn valid_name(s: &str) -> Result<(), McpError> {
    if s.is_empty() || s.len() > 512 || s.chars().any(char::is_control) || s.trim() != s {
        return Err(error("Names must be nonempty, at most 512 bytes, and contain no control characters or surrounding whitespace"));
    }
    Ok(())
}
#[derive(Serialize)]
pub struct Profile {
    name: String,
    servers: Vec<String>,
    gateway: bool,
    has_overrides: bool,
}
#[derive(Serialize)]
pub struct Snapshot {
    revision: String,
    active: String,
    profiles: Vec<Profile>,
}
pub fn list(base: &Path) -> Result<Snapshot, McpError> {
    let raw = read(&base.join("mcp-profiles.json"))?;
    snapshot(&document(raw.as_deref())?, revision(raw.as_deref()))
}
fn snapshot(v: &Value, revision: String) -> Result<Snapshot, McpError> {
    let mut profiles = Vec::new();
    for (name, p) in v["profiles"].as_object().unwrap() {
        profiles.push(Profile {
            name: name.clone(),
            servers: p
                .get("servers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|s| s.as_str().unwrap().into())
                .collect(),
            gateway: p["gateway"].as_bool().unwrap_or(false),
            has_overrides: p.get("config").is_some(),
        });
    }
    let result = Snapshot {
        revision,
        active: v["active"].as_str().unwrap().into(),
        profiles,
    };
    if serde_json::to_vec(&result)
        .map_err(|_| error("Cannot encode profiles"))?
        .len()
        > 200 * 1024
    {
        return Err(error("Profile list exceeds 200 KiB"));
    }
    Ok(result)
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    Save {
        revision: String,
        name: String,
        servers: Vec<String>,
        gateway: bool,
        create: bool,
    },
    Delete {
        revision: String,
        name: String,
    },
    Activate {
        revision: String,
        name: String,
    },
}
pub fn edit(base: &Path, trust: &mut TrustStore, edit: Edit) -> Result<Snapshot, McpError> {
    fs::create_dir_all(base).map_err(|_| error("Cannot create MCP configuration directory"))?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(base.join(".mcp-profiles.lock"))
        .map_err(|_| error("Cannot lock MCP profiles"))?;
    let meta = lock
        .metadata()
        .map_err(|_| error("Cannot inspect profile lock"))?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0
    {
        return Err(error("MCP profiles are busy or their lock is unsafe"));
    }
    let _unlock = trust_disk::UnlockOnDrop(&lock);
    let path = base.join("mcp-profiles.json");
    let raw = read(&path)?;
    let mut v = document(raw.as_deref())?;
    let (expected, name) = match &edit {
        Edit::Save { revision, name, .. }
        | Edit::Delete { revision, name }
        | Edit::Activate { revision, name } => (revision, name),
    };
    valid_name(name)?;
    if expected != &revision(raw.as_deref()) {
        return Err(error(
            "MCP profiles changed; reopen the editor before saving",
        ));
    }
    let mut changed_profile = None;
    match &edit {
        Edit::Save {
            name,
            servers,
            gateway,
            create,
            ..
        } => {
            if servers.len() > 128 {
                return Err(error("At most 128 server IDs per profile"));
            }
            let mut seen = std::collections::HashSet::new();
            for s in servers {
                valid_name(s)?;
                if !seen.insert(s) {
                    return Err(error("Duplicate server ID"));
                }
            }
            let profiles = v["profiles"].as_object_mut().unwrap();
            if *create == profiles.contains_key(name) {
                return Err(error(
                    "Profile already exists or was removed; reopen the editor",
                ));
            }
            if *create && profiles.len() >= 8 {
                return Err(error(
                    "The graphical editor supports at most eight profiles",
                ));
            }
            let p = profiles.entry(name.clone()).or_insert(json!({}));
            p["servers"] = json!(servers);
            p["gateway"] = json!(gateway);
            if profiles.len() == 1 {
                v["active"] = json!(name);
            }
            changed_profile = Some(name.clone());
        }
        Edit::Delete { name, .. } => {
            if v["active"] == *name {
                return Err(error(
                    "Choose another active profile before deleting this profile",
                ));
            }
            if v["profiles"]
                .as_object_mut()
                .unwrap()
                .remove(name)
                .is_none()
            {
                return Err(error("Profile not found"));
            }
            changed_profile = Some(name.clone());
        }
        Edit::Activate { name, .. } => {
            if !v["profiles"].as_object().unwrap().contains_key(name) {
                return Err(error("Profile not found"));
            }
            v["active"] = json!(name);
        }
    }
    let data = serde_json::to_vec_pretty(&v).map_err(|_| error("Cannot encode MCP profiles"))?;
    if data.len() as u64 > LIMIT {
        return Err(error("MCP profiles exceed 256 KiB"));
    }
    let result = snapshot(&v, revision(Some(&data)))?;
    trust.sync(base)?;
    // Revoke before publication: a failed write may leave trust blocked, never granted.
    if let Some(name) = changed_profile {
        trust.block_for_edit(&name)?;
    }
    // Reject concurrent external edits detected after trust I/O as well.
    if read(&path)? != raw {
        return Err(error("MCP profiles changed; reopen the editor"));
    }
    let temp = base.join(format!(".mcp-profiles-{}", uuid::Uuid::new_v4()));
    let write = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|_| error("Cannot write MCP profiles"))?;
        file.write_all(&data)
            .and_then(|_| file.sync_all())
            .map_err(|_| error("Cannot persist MCP profiles"))?;
        fs::rename(&temp, &path).map_err(|_| error("Cannot replace MCP profiles"))?;
        fs::File::open(base)
            .and_then(|d| d.sync_all())
            .map_err(|_| error("Cannot sync MCP profile directory"))?;
        Ok(result)
    })();
    if write.is_err() {
        let _ = fs::remove_file(temp);
    }
    write
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("visor-profile-edit-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("mcp-profiles.json"),r#"{"active":"default","extra":"keep","profiles":{"default":{"servers":["old"],"config":{"old":{"TOKEN":"private-secret"}},"future":{"keep":true}},"other":{"servers":[]}}}"#).unwrap();
        dir
    }
    fn save(revision: String, name: &str, create: bool) -> Edit {
        Edit::Save {
            revision,
            name: name.into(),
            servers: vec!["new".into()],
            gateway: false,
            create,
        }
    }
    #[test]
    fn edits_preserve_overrides_redact_responses_and_require_new_trust() {
        let dir = fixture();
        let original = list(&dir).unwrap();
        assert!(!serde_json::to_string(&original)
            .unwrap()
            .contains("private-secret"));
        let mut trust = TrustStore::default();
        let after = edit(
            &dir,
            &mut trust,
            save(original.revision.clone(), "default", false),
        )
        .unwrap();
        assert!(trust.is_revoked("default"));
        let v: Value =
            serde_json::from_slice(&fs::read(dir.join("mcp-profiles.json")).unwrap()).unwrap();
        assert_eq!(
            v["profiles"]["default"]["config"]["old"]["TOKEN"],
            "private-secret"
        );
        assert_eq!(v["profiles"]["default"]["future"]["keep"], true);
        assert_eq!(v["extra"], "keep");
        assert!(edit(&dir, &mut trust, save(original.revision, "other", false)).is_err());
        assert_eq!(list(&dir).unwrap().revision, after.revision);
        let mut restarted = TrustStore::default();
        restarted.sync(&dir).unwrap();
        assert!(restarted.is_revoked("default"));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn create_activate_delete_and_reuse_remain_blocked() {
        let dir = fixture();
        let mut trust = TrustStore::default();
        let v = edit(
            &dir,
            &mut trust,
            save(list(&dir).unwrap().revision, "new", true),
        )
        .unwrap();
        assert!(trust.is_revoked("new"));
        assert!(edit(
            &dir,
            &mut trust,
            Edit::Delete {
                revision: v.revision.clone(),
                name: "default".into()
            }
        )
        .is_err());
        let v = edit(
            &dir,
            &mut trust,
            Edit::Activate {
                revision: v.revision,
                name: "new".into(),
            },
        )
        .unwrap();
        assert_eq!(v.active, "new");
        let v = edit(
            &dir,
            &mut trust,
            Edit::Delete {
                revision: v.revision,
                name: "default".into(),
            },
        )
        .unwrap();
        assert!(trust.is_revoked("default"));
        edit(&dir, &mut trust, save(v.revision, "default", true)).unwrap();
        assert!(trust.is_revoked("default"));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn absent_files_are_read_only_and_unsafe_or_failed_writes_cannot_grant() {
        let dir =
            std::env::temp_dir().join(format!("visor-profile-absent-{}", uuid::Uuid::new_v4()));
        assert_eq!(list(&dir).unwrap().revision, "missing");
        assert!(!dir.exists());
        let mut trust = TrustStore::default();
        edit(&dir, &mut trust, save("missing".into(), "first", true)).unwrap();
        assert_eq!(list(&dir).unwrap().active, "first");
        assert!(trust.is_revoked("first"));
        let path = dir.join("mcp-profiles.json");
        fs::remove_file(&path).unwrap();
        fs::write(dir.join("target"), "{}").unwrap();
        std::os::unix::fs::symlink(dir.join("target"), &path).unwrap();
        assert!(list(&dir).is_err());
        assert!(edit(&dir, &mut trust, save("missing".into(), "second", true)).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn locked_or_corrupt_files_and_invalid_memberships_do_not_change_config() {
        let dir = fixture();
        let before = fs::read(dir.join("mcp-profiles.json")).unwrap();
        let mut trust = TrustStore::default();
        let lock = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dir.join(".mcp-profiles.lock"))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert!(edit(
            &dir,
            &mut trust,
            save(list(&dir).unwrap().revision, "default", false)
        )
        .is_err());
        unsafe {
            libc::flock(lock.as_raw_fd(), libc::LOCK_UN);
        }
        drop(lock);
        let duplicate = Edit::Save {
            revision: list(&dir).unwrap().revision,
            name: "default".into(),
            servers: vec!["x".into(), "x".into()],
            gateway: false,
            create: false,
        };
        assert!(edit(&dir, &mut trust, duplicate).is_err());
        assert_eq!(fs::read(dir.join("mcp-profiles.json")).unwrap(), before);
        fs::write(dir.join("mcp-profiles.json"), "broken").unwrap();
        assert!(list(&dir).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
