//! Daemon-local, snapshot-based review. Discovery never spawns or connects.
use super::{
    config::{self, Resolution},
    McpError,
};
use serde::Serialize;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub use super::config::Provenance;

const MAX_FILE: u64 = 256 * 1024;
const TTL: Duration = Duration::from_secs(300);
const MAX_REVIEWS: usize = 8;

struct Source {
    path: PathBuf,
    contents: Option<String>,
}
struct Pending {
    at: Instant,
    sources: Vec<Source>,
    resolution: Resolution,
}
#[derive(Default)]
pub struct TrustStore {
    pending: HashMap<Uuid, Pending>,
    grants: HashMap<String, Grant>,
    revoked: std::collections::HashSet<String>,
}
#[derive(Clone)]
pub struct Grant {
    pub id: Uuid,
    pub resolution: Resolution,
}
#[derive(Serialize)]
pub struct Preview {
    pub review_id: Uuid,
    pub profile: String,
    pub profiles_path: String,
    pub expires_in_seconds: u64,
    pub servers: Vec<Provenance>,
}
#[derive(Serialize)]
pub struct TrustedProfile {
    pub profile: String,
    pub review_id: Uuid,
    pub server_count: usize,
}

fn read(path: &Path, required: bool) -> Result<Option<String>, McpError> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => return Ok(None),
        Err(_) => return Err(McpError::message("MCP configuration cannot be read")),
    };
    if !file
        .metadata()
        .map_err(|_| McpError::message("MCP metadata unavailable"))?
        .is_file()
    {
        return Err(McpError::message(
            "MCP configuration must be a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| McpError::message("MCP configuration read failed"))?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(McpError::message("MCP configuration exceeds 256 KiB"));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| McpError::message("MCP configuration is not UTF-8"))
}

pub struct Review {
    sources: Vec<Source>,
    resolution: Resolution,
    profiles_path: String,
}
pub fn inspect(
    dir: &Path,
    profile: Option<&str>,
    project: Option<&str>,
) -> Result<Review, McpError> {
    let dir = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| McpError::message("MCP directory unavailable"))?
            .join(dir)
    };
    let project = config::selected_project_path(
        project
            .map(Into::into)
            .or_else(|| std::env::var_os("HYPERMESH_MCP_PROJECT")),
    )?;
    let mut sources = vec![
        Source {
            path: dir.join("mcp.json"),
            contents: None,
        },
        Source {
            path: dir.join("mcp-profiles.json"),
            contents: None,
        },
    ];
    for source in &mut sources {
        source.contents = read(&source.path, false)?;
    }
    if let Some(path) = &project {
        sources.push(Source {
            path: path.clone(),
            contents: read(path, true)?,
        });
    }
    let resolution = config::resolve_bytes(
        &dir,
        profile,
        project.as_deref(),
        sources[0].contents.as_deref(),
        sources.get(2).and_then(|s| s.contents.as_deref()),
        sources[1].contents.as_deref(),
    )?;
    if resolution.profile.len() > 512 {
        return Err(McpError::message("MCP profile name exceeds 512 bytes"));
    }
    if resolution.provenance.len() > 128 {
        return Err(McpError::message("MCP review exceeds 128 candidates"));
    }
    let profiles_path = sources[1].path.to_string_lossy().into_owned();
    Ok(Review {
        sources,
        resolution,
        profiles_path,
    })
}
impl TrustStore {
    pub fn stage(&mut self, review: Review) -> Result<Preview, McpError> {
        self.pending.retain(|_, v| v.at.elapsed() < TTL);
        if self.pending.len() >= MAX_REVIEWS {
            return Err(McpError::message(
                "Too many pending MCP reviews; cancel one or wait five minutes",
            ));
        }
        let id = Uuid::new_v4();
        let preview = Preview {
            review_id: id,
            profile: review.resolution.profile.clone(),
            profiles_path: review.profiles_path,
            expires_in_seconds: TTL.as_secs(),
            servers: review.resolution.provenance.clone(),
        };
        if serde_json::to_vec(&preview)
            .map_err(|_| McpError::message("MCP preview encoding failed"))?
            .len()
            > 200 * 1024
        {
            return Err(McpError::message("MCP preview too large"));
        }
        self.pending.insert(
            id,
            Pending {
                at: Instant::now(),
                sources: review.sources,
                resolution: review.resolution,
            },
        );
        Ok(preview)
    }
    pub fn approve(&mut self, id: Uuid) -> Result<String, McpError> {
        let pending = self
            .pending
            .remove(&id)
            .ok_or_else(|| McpError::message("MCP review missing or already used"))?;
        if pending.at.elapsed() >= TTL {
            return Err(McpError::message("MCP review expired"));
        }
        for source in &pending.sources {
            if read(&source.path, false)? != source.contents {
                return Err(McpError::message("MCP configuration changed; review again"));
            }
        }
        if self.grants.len() + self.revoked.len() >= MAX_REVIEWS
            && !self.grants.contains_key(&pending.resolution.profile)
            && !self.revoked.contains(&pending.resolution.profile)
        {
            return Err(McpError::message(
                "Too many trusted MCP profiles; revoke one first",
            ));
        }
        let profile = pending.resolution.profile.clone();
        self.revoked.remove(&profile);
        self.grants.insert(
            profile.clone(),
            Grant {
                id,
                resolution: pending.resolution,
            },
        );
        Ok(profile)
    }
    pub fn cancel(&mut self, id: Uuid) {
        self.pending.remove(&id);
    }
    pub fn revoke(&mut self, profile: &str) {
        if self.grants.remove(profile).is_some() {
            self.revoked.insert(profile.into());
        }
    }
    pub fn is_revoked(&self, profile: &str) -> bool {
        self.revoked.contains(profile)
    }
    pub fn approved(&self, profile: &str) -> Option<Grant> {
        self.grants.get(profile).cloned()
    }
    pub fn list(&self) -> Vec<TrustedProfile> {
        let mut profiles: Vec<_> = self
            .grants
            .iter()
            .map(|(name, g)| TrustedProfile {
                profile: name.clone(),
                review_id: g.id,
                server_count: g.resolution.servers.len(),
            })
            .collect();
        profiles.sort_by(|a, b| a.profile.cmp(&b.profile));
        profiles
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("visor-trust-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("mcp.json"),json!({"mcpServers":{"same":{"command":"user"},"disabled":{"command":"blocked","enabled":false},"http":{"url":"https://user:password@example.com/private?token=secret","headers":{"Authorization":"secret-header"},"env":{"TOKEN":"secret-env"},"args":["secret-arg"]}}}).to_string()).unwrap();
        std::fs::write(dir.join("project.json"),json!({"mcpServers":{"same":{"command":"project"},"disabled":{"command":"project"},"new":{"command":"never-execute"},"unused":{"command":"not-selected"}}}).to_string()).unwrap();
        std::fs::write(dir.join("mcp-profiles.json"),json!({"profiles":{"default":{"servers":["same","disabled","http","new"],"config":{"new":{"command":"overridden","env.KEY":"secret-override"}}}}}).to_string()).unwrap();
        dir
    }
    fn inspect_fixture(dir: &Path) -> Review {
        inspect(dir, None, Some(dir.join("project.json").to_str().unwrap())).unwrap()
    }
    #[test]
    fn preview_explains_precedence_and_never_serializes_secret_values() {
        let dir = fixture();
        let mut store = TrustStore::default();
        let preview = store.stage(inspect_fixture(&dir)).unwrap();
        let raw = serde_json::to_string(&preview).unwrap();
        for value in [
            "password",
            "secret-header",
            "secret-env",
            "secret-arg",
            "secret-override",
            "/private",
            "token=",
        ] {
            assert!(!raw.contains(value), "leaked {value}");
        }
        assert!(preview
            .servers
            .iter()
            .any(|s| s.name == "same" && s.source == "selected_project" && s.status == "shadowed"));
        assert!(preview
            .servers
            .iter()
            .any(|s| s.name == "disabled" && s.source == "user" && s.status == "disabled"));
        assert!(preview
            .servers
            .iter()
            .any(|s| s.name == "unused" && s.status == "not_in_profile"));
        assert!(store.list().is_empty());
        store.approve(preview.review_id).unwrap();
        let grant = store.approved("default").unwrap();
        assert_eq!(
            grant.resolution.servers["same"].command.as_deref(),
            Some("user")
        );
        assert_eq!(
            grant.resolution.servers["new"].command.as_deref(),
            Some("overridden")
        );
        assert!(!grant.resolution.servers.contains_key("disabled"));
        std::fs::write(dir.join("project.json"), "changed after approval").unwrap();
        assert_eq!(
            store.approved("default").unwrap().resolution.servers["new"]
                .command
                .as_deref(),
            Some("overridden")
        );
        store.revoke("default");
        assert!(store.is_revoked("default"));
        assert!(store.approved("default").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn changed_cancelled_expired_and_replayed_reviews_cannot_grant_trust() {
        let dir = fixture();
        let mut store = TrustStore::default();
        let id = store.stage(inspect_fixture(&dir)).unwrap().review_id;
        store.cancel(id);
        assert!(store.approve(id).is_err());
        let id = store.stage(inspect_fixture(&dir)).unwrap().review_id;
        store.pending.get_mut(&id).unwrap().at = Instant::now() - TTL;
        assert!(store.approve(id).is_err());
        let id = store.stage(inspect_fixture(&dir)).unwrap().review_id;
        std::fs::write(dir.join("mcp.json"), "{}").unwrap();
        assert!(store.approve(id).is_err());
        assert!(store.list().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
        let dir = fixture();
        let id = store.stage(inspect_fixture(&dir)).unwrap().review_id;
        store.approve(id).unwrap();
        assert!(store.approve(id).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn files_and_pending_reviews_are_bounded_and_gateway_origin_is_explicit() {
        let dir = fixture();
        let mut store = TrustStore::default();
        for _ in 0..MAX_REVIEWS {
            store.stage(inspect_fixture(&dir)).unwrap();
        }
        assert!(store.stage(inspect_fixture(&dir)).is_err());
        std::fs::write(dir.join("mcp.json"), " ".repeat(MAX_FILE as usize + 1)).unwrap();
        assert!(inspect(&dir, None, None).is_err());
        std::fs::write(dir.join("mcp.json"), "{}").unwrap();
        std::fs::write(
            dir.join("mcp-profiles.json"),
            r#"{"profiles":{"default":{"gateway":true}}}"#,
        )
        .unwrap();
        let review = inspect(&dir, None, None).unwrap();
        assert_eq!(review.resolution.provenance[0].source, "gateway_default");
        assert_eq!(review.resolution.servers.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
