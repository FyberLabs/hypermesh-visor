//! Snapshot-based review with private, content-bound persistent receipts. Discovery never spawns or connects.
use super::trust_disk::{self, Fingerprint, Ledger, Receipt};
use super::{
    config::{self, Resolution},
    McpError,
};
use serde::Serialize;
use std::collections::BTreeMap;
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
    source_kind: String,
}
#[derive(Default)]
pub struct TrustStore {
    pending: HashMap<Uuid, Pending>,
    grants: HashMap<String, Grant>,
    revoked: std::collections::HashSet<String>,
    storage: Option<PathBuf>,
    disk: Option<Vec<u8>>,
    receipts: BTreeMap<String, Receipt>,
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
    pub status: &'static str,
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
    source_kind: String,
}
#[cfg(test)]
pub fn inspect(
    dir: &Path,
    profile: Option<&str>,
    project: Option<&str>,
) -> Result<Review, McpError> {
    inspect_source(dir, profile, project, None)
}
pub fn inspect_source(
    dir: &Path,
    profile: Option<&str>,
    project: Option<&str>,
    source_id: Option<&str>,
) -> Result<Review, McpError> {
    if project.is_some() && source_id.is_some() {
        return Err(McpError::message("Choose a source ID or a path, not both"));
    }
    let selected = source_id.map(super::sources::selected).transpose()?;
    let selected_text = selected
        .as_ref()
        .map(|p| {
            p.to_str()
                .ok_or_else(|| McpError::message("MCP source path is not UTF-8"))
        })
        .transpose()?;
    let project = selected_text.or(project);
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
    let source_kind = source_id.unwrap_or("selected_project").to_string();
    let normalized = sources
        .get(2)
        .and_then(|s| s.contents.as_deref())
        .map(super::sources::normalize)
        .transpose()?;
    let mut resolution = config::resolve_bytes(
        &dir,
        profile,
        project.as_deref(),
        sources[0].contents.as_deref(),
        normalized.as_deref(),
        sources[1].contents.as_deref(),
    )?;
    for entry in &mut resolution.provenance {
        if entry.source == "selected_project" {
            entry.source = source_kind.clone();
        }
    }
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
        source_kind,
    })
}
fn restore(base: &Path, profile: &str, receipt: &Receipt) -> Result<Grant, McpError> {
    let mut contents = Vec::new();
    for source in &receipt.sources {
        let raw = read(&source.path, false)?;
        if raw.as_ref().map(|v| trust_disk::digest(v.as_bytes())) != source.sha256 {
            return Err(McpError::message("MCP source changed; review required"));
        }
        contents.push(raw);
    }
    let project = receipt.sources.get(2).map(|s| s.path.as_path());
    let normalized = contents
        .get(2)
        .and_then(|v| v.as_deref())
        .map(super::sources::normalize)
        .transpose()?;
    let mut resolution = config::resolve_bytes(
        base,
        Some(profile),
        project,
        contents[0].as_deref(),
        normalized.as_deref(),
        contents[1].as_deref(),
    )?;
    for entry in &mut resolution.provenance {
        if entry.source == "selected_project" {
            entry.source = receipt.source_kind.clone();
        }
    }
    Ok(Grant {
        id: receipt.review_id,
        resolution,
    })
}

impl TrustStore {
    /// Refresh receipts before trust decisions and session publication. No writes on reads.
    pub fn sync(&mut self, dir: &Path) -> Result<(), McpError> {
        let base = if dir.is_absolute() {
            dir.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|_| McpError::message("MCP directory unavailable"))?
                .join(dir)
        };
        if self.storage.as_ref().is_some_and(|old| old != &base) {
            return Err(McpError::message("MCP trust directory changed"));
        }
        let bytes = trust_disk::read(&base)?;
        if self.disk.is_some() && bytes.is_none() {
            return Err(McpError::message(
                "MCP trust store removed; restore it before opening sessions",
            ));
        }
        if self.storage.is_some() && bytes == self.disk {
            return Ok(());
        }
        let ledger = bytes
            .as_deref()
            .map(|b| trust_disk::decode(&base, b))
            .transpose()?
            .unwrap_or(Ledger {
                version: 1,
                profiles: BTreeMap::new(),
            });
        let mut grants = HashMap::new();
        let mut revoked = std::collections::HashSet::new();
        for (profile, receipt) in &ledger.profiles {
            if !receipt.revoked {
                let grant = if self.receipts.get(profile) == Some(receipt) {
                    self.grants.get(profile).cloned()
                } else {
                    restore(&base, profile, receipt).ok()
                };
                if let Some(grant) = grant {
                    grants.insert(profile.clone(), grant);
                    continue;
                }
            }
            revoked.insert(profile.clone());
        }
        self.storage = Some(base);
        self.disk = bytes;
        self.receipts = ledger.profiles;
        self.grants = grants;
        self.revoked = revoked;
        Ok(())
    }
    fn persist(&mut self, next: BTreeMap<String, Receipt>) -> Result<(), McpError> {
        if let Some(base) = &self.storage {
            let ledger = Ledger {
                version: 1,
                profiles: next.clone(),
            };
            self.disk = Some(trust_disk::save(base, self.disk.as_deref(), &ledger)?);
        }
        self.receipts = next;
        Ok(())
    }
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
                source_kind: review.source_kind,
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
        let receipt = Receipt {
            review_id: id,
            revoked: false,
            source_kind: pending.source_kind,
            sources: pending
                .sources
                .iter()
                .map(|s| Fingerprint {
                    path: s.path.clone(),
                    sha256: s
                        .contents
                        .as_ref()
                        .map(|v| trust_disk::digest(v.as_bytes())),
                })
                .collect(),
        };
        let mut next = self.receipts.clone();
        next.insert(profile.clone(), receipt);
        self.persist(next)?;
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
    pub fn revoke(&mut self, profile: &str) -> Result<(), McpError> {
        if let Some(receipt) = self.receipts.get(profile) {
            let mut next = self.receipts.clone();
            let mut receipt = receipt.clone();
            receipt.revoked = true;
            next.insert(profile.into(), receipt);
            self.persist(next)?;
            self.grants.remove(profile);
            self.revoked.insert(profile.into());
        }
        Ok(())
    }
    /// Profile edits never confer trust, including newly created profiles.
    pub fn block_for_edit(&mut self, profile: &str) -> Result<(), McpError> {
        self.block_profiles(&[profile.to_string()])
    }
    pub fn block_profiles(&mut self, profiles: &[String]) -> Result<(), McpError> {
        let base = self
            .storage
            .as_ref()
            .ok_or_else(|| McpError::message("Load trust before editing configuration"))?;
        let mut next = self.receipts.clone();
        for profile in profiles {
            let receipt = next.entry(profile.clone()).or_insert_with(|| Receipt {
                review_id: Uuid::new_v4(),
                revoked: true,
                source_kind: "selected_project".into(),
                sources: vec![
                    Fingerprint {
                        path: base.join("mcp.json"),
                        sha256: None,
                    },
                    Fingerprint {
                        path: base.join("mcp-profiles.json"),
                        sha256: None,
                    },
                ],
            });
            receipt.revoked = true;
        }
        if next.len() > MAX_REVIEWS {
            return Err(McpError::message(
                "Trust profile limit reached; configuration edit not saved",
            ));
        }
        self.persist(next)?;
        for profile in profiles {
            self.grants.remove(profile);
            self.revoked.insert(profile.clone());
        }
        self.pending
            .retain(|_, p| !profiles.contains(&p.resolution.profile));
        Ok(())
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
                status: "approved",
            })
            .collect();
        for (profile, receipt) in &self.receipts {
            if !self.grants.contains_key(profile) {
                profiles.push(TrustedProfile {
                    profile: profile.clone(),
                    review_id: receipt.review_id,
                    server_count: 0,
                    status: if receipt.revoked {
                        "revoked"
                    } else {
                        "needs_review"
                    },
                });
            }
        }
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
    fn locked_storage_does_not_acknowledge_an_unwritten_decision() {
        use std::os::fd::AsRawFd;
        let dir = fixture();
        let mut store = TrustStore::default();
        store.sync(&dir).unwrap();
        let id = store.stage(inspect_fixture(&dir)).unwrap().review_id;
        store.approve(id).unwrap();
        let lock = std::fs::File::open(dir.join("mcp-trust/lock")).unwrap();
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert!(store.revoke("default").is_err());
        assert_eq!(store.approved("default").unwrap().id, id);
        let mut restarted = TrustStore::default();
        restarted.sync(&dir).unwrap();
        assert!(restarted.approved("default").is_some());
        drop(lock);
        store.revoke("default").unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn persisted_receipts_restore_without_secrets_and_keep_revocations() {
        use std::os::unix::fs::MetadataExt;
        let dir = fixture();
        let mut first = TrustStore::default();
        first.sync(&dir).unwrap();
        let id = first.stage(inspect_fixture(&dir)).unwrap().review_id;
        first.approve(id).unwrap();
        let path = dir.join("mcp-trust/approvals.json");
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            std::fs::metadata(dir.join("mcp-trust")).unwrap().mode() & 0o777,
            0o700
        );
        let data = std::fs::read_to_string(&path).unwrap();
        for secret in [
            "secret-header",
            "secret-env",
            "secret-arg",
            "secret-override",
            "password",
            "overridden",
        ] {
            assert!(!data.contains(secret));
        }
        let mut restarted = TrustStore::default();
        restarted.sync(&dir).unwrap();
        assert_eq!(restarted.approved("default").unwrap().id, id);
        assert_eq!(
            restarted.approved("default").unwrap().resolution.servers["new"]
                .command
                .as_deref(),
            Some("overridden")
        );
        restarted.revoke("default").unwrap();
        first.sync(&dir).unwrap();
        assert!(first.is_revoked("default"));
        let mut again = TrustStore::default();
        again.sync(&dir).unwrap();
        assert!(again.is_revoked("default"));
        assert_eq!(again.list()[0].status, "revoked");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn editor_profile_jsonc_receipts_restore_and_pin_comments() {
        let dir = fixture();
        let path = dir.join("editor.json");
        let raw = r#"{ // reviewed editor source
            "servers":{"new":{"command":"never-run",},},
        }"#;
        std::fs::write(&path, raw).unwrap();
        let mut review = inspect(&dir, Some("default"), path.to_str()).unwrap();
        review.source_kind = "vscode_insiders_profile:abc123".into();
        let mut first = TrustStore::default();
        first.sync(&dir).unwrap();
        let id = first.stage(review).unwrap().review_id;
        first.approve(id).unwrap();
        let mut restarted = TrustStore::default();
        restarted.sync(&dir).unwrap();
        let grant = restarted.approved("default").unwrap();
        assert_eq!(grant.id, id);
        assert!(grant
            .resolution
            .provenance
            .iter()
            .any(|p| p.source == "vscode_insiders_profile:abc123"));
        // A comment-only edit still changes the approved source bytes.
        std::fs::write(&path, raw.replace("reviewed", "edited")).unwrap();
        let mut changed = TrustStore::default();
        changed.sync(&dir).unwrap();
        assert_eq!(changed.list()[0].status, "needs_review");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn additional_editor_receipts_restore_provenance_and_pin_full_source() {
        for (kind, raw) in [
            (
                "zed_user",
                r#"{"theme":"reviewed","context_servers":{"new":{"command":"never-run"}}}"#,
            ),
            (
                "devin_user",
                r#"{/*reviewed*/"mcpServers":{"new":{"url":"https://example.com/mcp","transport":"http"}}}"#,
            ),
        ] {
            let dir = fixture();
            let path = dir.join("editor.json");
            std::fs::write(&path, raw).unwrap();
            let mut review = inspect(&dir, Some("default"), path.to_str()).unwrap();
            review.source_kind = kind.into();
            let mut first = TrustStore::default();
            first.sync(&dir).unwrap();
            let id = first.stage(review).unwrap().review_id;
            first.approve(id).unwrap();
            let mut restarted = TrustStore::default();
            restarted.sync(&dir).unwrap();
            let grant = restarted.approved("default").unwrap();
            assert_eq!(grant.id, id);
            assert!(grant.resolution.provenance.iter().any(|p| p.source == kind));
            std::fs::write(&path, raw.replace("reviewed", "edited")).unwrap();
            let mut changed = TrustStore::default();
            changed.sync(&dir).unwrap();
            assert_eq!(changed.list()[0].status, "needs_review");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn changed_sources_require_review_after_restart_and_updates_do_not_clobber() {
        let dir = fixture();
        let mut first = TrustStore::default();
        first.sync(&dir).unwrap();
        let id = first.stage(inspect_fixture(&dir)).unwrap().review_id;
        first.approve(id).unwrap();
        let mut second = TrustStore::default();
        second.sync(&dir).unwrap();
        let pending = second.stage(inspect_fixture(&dir)).unwrap().review_id;
        first.revoke("default").unwrap();
        assert!(
            second.approve(pending).is_err(),
            "stale writer overwrote revocation"
        );
        second.sync(&dir).unwrap();
        assert!(second.is_revoked("default"));
        let id = first.stage(inspect_fixture(&dir)).unwrap().review_id;
        first.approve(id).unwrap();
        std::fs::write(dir.join("project.json"), r#"{"mcpServers":{}}"#).unwrap();
        let mut restarted = TrustStore::default();
        restarted.sync(&dir).unwrap();
        assert!(restarted.is_revoked("default"));
        assert!(restarted.approved("default").is_none());
        assert_eq!(restarted.list()[0].status, "needs_review");
        assert!(
            first.approved("default").is_some(),
            "running snapshot changed without reload"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn unsafe_corrupt_or_missing_persistence_never_falls_back() {
        use std::os::unix::fs::PermissionsExt;
        let dir = fixture();
        let mut first = TrustStore::default();
        first.sync(&dir).unwrap();
        let id = first.stage(inspect_fixture(&dir)).unwrap().review_id;
        first.approve(id).unwrap();
        let path = dir.join("mcp-trust/approvals.json");
        let original = std::fs::read(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(first.sync(&dir).is_err());
        assert!(TrustStore::default().sync(&dir).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, "broken JSON").unwrap();
        assert!(TrustStore::default().sync(&dir).is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(TrustStore::default().sync(&dir).is_err());
        std::fs::write(dir.join("outside"), &original).unwrap();
        std::os::unix::fs::symlink(dir.join("outside"), &path).unwrap();
        assert!(TrustStore::default().sync(&dir).is_err());
        std::fs::remove_dir_all(dir).unwrap();
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
        store.revoke("default").unwrap();
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
