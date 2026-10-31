//! Focused-app → MCP server bindings (companion prefer-MCP).

use std::fs;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::McpError;

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BindingsFile {
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub server: String,
    #[serde(default)]
    pub wm_class: Option<String>,
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default)]
    pub executable: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, Deserialize)]
pub struct FocusedApp {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wm_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
}

impl FocusedApp {
    pub fn is_empty(&self) -> bool {
        self.wm_class.as_deref().unwrap_or("").is_empty()
            && self.app_id.as_deref().unwrap_or("").is_empty()
            && self.executable.as_deref().unwrap_or("").is_empty()
    }
}

/// Re-read bounded configuration at most once per second. Content and permission
/// changes must invalidate stale routing even when timestamps are preserved.
pub const BINDINGS_REFRESH: Duration = Duration::from_secs(1);
const MAX_BYTES: u64 = 64 * 1024;
const MAX_BINDINGS: usize = 128;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingStatus {
    #[default]
    Missing,
    Ready,
    Invalid,
    Unavailable,
    TooLarge,
}

#[derive(Debug, Clone, Default)]
pub struct BindingSnapshot {
    status: BindingStatus,
    bindings: Vec<Binding>,
}

impl BindingSnapshot {
    pub fn explain(&self, focus: Option<&FocusedApp>, healthy: &[String]) -> MatchDecision {
        let mut decision = explain_preferred(focus, &self.bindings, healthy);
        decision.bindings_status = self.status;
        if matches!(
            self.status,
            BindingStatus::Invalid | BindingStatus::Unavailable | BindingStatus::TooLarge
        ) {
            decision.reason = MatchReason::BindingsUnavailable;
        }
        decision
    }
}

#[derive(Debug, Default)]
pub struct BindingCache {
    checked_at: Option<Instant>,
    snapshot: BindingSnapshot,
}

impl BindingCache {
    pub fn current(&mut self, dir: &Path, now: Instant) -> BindingSnapshot {
        if self
            .checked_at
            .is_none_or(|at| now.saturating_duration_since(at) >= BINDINGS_REFRESH)
        {
            self.snapshot = match read_bindings(dir) {
                Ok(file) => BindingSnapshot {
                    status: BindingStatus::Ready,
                    bindings: file.bindings,
                },
                Err(status) => BindingSnapshot {
                    status,
                    bindings: Vec::new(),
                },
            };
            self.checked_at = Some(now);
        }
        self.snapshot.clone()
    }
}

fn read_bindings(dir: &Path) -> Result<BindingsFile, BindingStatus> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(dir.join("mcp-bindings.json"))
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                BindingStatus::Missing
            } else {
                BindingStatus::Unavailable
            }
        })?;
    let meta = file.metadata().map_err(|_| BindingStatus::Unavailable)?;
    if !meta.is_file() {
        return Err(BindingStatus::Unavailable);
    }
    if meta.len() > MAX_BYTES {
        return Err(BindingStatus::TooLarge);
    }
    let mut raw = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|_| BindingStatus::Unavailable)?;
    if raw.len() as u64 > MAX_BYTES {
        return Err(BindingStatus::TooLarge);
    }
    let parsed: BindingsFile = serde_json::from_slice(&raw).map_err(|_| BindingStatus::Invalid)?;
    if parsed.bindings.len() > MAX_BINDINGS {
        return Err(BindingStatus::TooLarge);
    }
    for binding in &parsed.bindings {
        let selectors = [&binding.wm_class, &binding.app_id, &binding.executable];
        if binding.server.trim().is_empty()
            || binding.server.len() > 1024
            || !selectors.iter().any(|s| s.is_some())
            || selectors.iter().any(|s| {
                s.as_ref()
                    .is_some_and(|s| s.trim().is_empty() || s.len() > 4096 || s.contains('\0'))
            })
            || binding
                .executable
                .as_ref()
                .is_some_and(|s| !Path::new(s.trim()).is_absolute())
        {
            return Err(BindingStatus::Invalid);
        }
    }
    Ok(parsed)
}

pub fn load_bindings(dir: &Path) -> Result<BindingsFile, McpError> {
    match read_bindings(dir) {
        Ok(file) => Ok(file),
        Err(BindingStatus::Missing) => Ok(BindingsFile::default()),
        // Never expose raw parser errors or configuration fragments.
        Err(status) => Err(McpError::message(format!("MCP bindings {status:?}"))),
    }
}

/// A bounded explanation of routing. No config values, credentials, or titles.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MatchDecision {
    #[serde(skip)]
    pub preferred: Option<String>,
    pub reason: MatchReason,
    pub bindings_status: BindingStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_index: Option<usize>,
    pub identity_matches: usize,
    pub healthy_matches: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchReason {
    FocusUnavailable,
    NoBindings,
    BindingsUnavailable,
    IdentityMismatch,
    ServerUnavailable,
    Matched,
}

/// Binding order is the user's explicit precedence. All selectors must match;
/// an unavailable server never prevents a later healthy binding from winning.
pub fn explain_preferred(
    focus: Option<&FocusedApp>,
    bindings: &[Binding],
    healthy_servers: &[String],
) -> MatchDecision {
    let mut decision = MatchDecision {
        preferred: None,
        reason: MatchReason::FocusUnavailable,
        bindings_status: BindingStatus::Ready,
        binding_index: None,
        identity_matches: 0,
        healthy_matches: 0,
    };
    let Some(focus) = focus.filter(|app| !app.is_empty()) else {
        return decision;
    };
    decision.reason = if bindings.is_empty() {
        MatchReason::NoBindings
    } else {
        MatchReason::IdentityMismatch
    };
    for (index, binding) in bindings.iter().enumerate() {
        if !matches_focus(focus, binding) {
            continue;
        }
        decision.identity_matches += 1;
        if !binding.server.trim().is_empty()
            && healthy_servers.iter().any(|id| id == &binding.server)
        {
            decision.healthy_matches += 1;
            if decision.preferred.is_none() {
                decision.preferred = Some(binding.server.clone());
                decision.binding_index = Some(index);
            }
        }
    }
    if decision.preferred.is_some() {
        decision.reason = MatchReason::Matched;
    } else if decision.identity_matches > 0 {
        decision.reason = MatchReason::ServerUnavailable;
    }
    decision
}

#[cfg(test)]
fn resolve_preferred(
    focus: &FocusedApp,
    bindings: &[Binding],
    healthy_servers: &[String],
) -> Option<String> {
    explain_preferred(Some(focus), bindings, healthy_servers).preferred
}

fn matches_focus(focus: &FocusedApp, binding: &Binding) -> bool {
    let mut any = false;
    if let Some(want) = binding
        .wm_class
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        any = true;
        if !eq_ignore_case(focus.wm_class.as_deref(), want) {
            return false;
        }
    }
    if let Some(want) = binding
        .app_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        any = true;
        if !eq_ignore_case(focus.app_id.as_deref(), want) {
            return false;
        }
    }
    if let Some(want) = binding
        .executable
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        any = true;
        if focus.executable.as_deref() != Some(want) {
            return false;
        }
    }
    any
}

fn eq_ignore_case(got: Option<&str>, want: &str) -> bool {
    got.map(|g| g.eq_ignore_ascii_case(want)).unwrap_or(false)
}

/// Companion display mode: prefer `mcp:<id>` when a healthy binding matches.
pub fn companion_mode(preferred_mcp: Option<&str>, verb: Option<&str>) -> Option<String> {
    if let Some(id) = preferred_mcp.filter(|s| !s.is_empty()) {
        return Some(format!("mcp:{id}"));
    }
    verb.filter(|s| !s.is_empty()).map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_explain_precedence_and_health_fallback() {
        let focus = FocusedApp {
            wm_class: Some("Editor".into()),
            ..FocusedApp::default()
        };
        let bindings: Vec<Binding> = serde_json::from_value(serde_json::json!([
            {"server":"offline", "wm_class":"Editor"},
            {"server":"first", "wm_class":"Editor"},
            {"server":"second", "wm_class":"Editor"}
        ]))
        .unwrap();
        let chosen = explain_preferred(Some(&focus), &bindings, &["second".into(), "first".into()]);
        assert_eq!(chosen.preferred.as_deref(), Some("first"));
        assert_eq!(chosen.binding_index, Some(1));
        assert_eq!(chosen.identity_matches, 3);
        assert_eq!(chosen.healthy_matches, 2);
        assert_eq!(chosen.reason, MatchReason::Matched);
        let fallback = explain_preferred(Some(&focus), &bindings, &["second".into()]);
        assert_eq!(fallback.binding_index, Some(2));
        assert_eq!(
            explain_preferred(Some(&focus), &bindings, &[]).reason,
            MatchReason::ServerUnavailable
        );
        assert_eq!(
            explain_preferred(None, &bindings, &[]).reason,
            MatchReason::FocusUnavailable
        );
        assert_eq!(
            explain_preferred(Some(&focus), &[], &[]).reason,
            MatchReason::NoBindings
        );
        let unrelated = FocusedApp {
            wm_class: Some("Browser".into()),
            ..FocusedApp::default()
        };
        assert_eq!(
            explain_preferred(Some(&unrelated), &bindings, &[]).reason,
            MatchReason::IdentityMismatch
        );
        // Only bounded counters/reason/index are serialized, not even server IDs.
        assert_eq!(
            serde_json::to_value(chosen).unwrap(),
            serde_json::json!({
                "reason":"matched", "bindings_status":"ready", "binding_index":1, "identity_matches":3, "healthy_matches":2
            })
        );
    }

    #[test]
    fn executable_match_is_exact_and_all_selectors_are_required() {
        let binding: Binding = serde_json::from_value(serde_json::json!({
            "server":"editor", "wm_class":"Editor", "executable":"/opt/Editor"
        }))
        .unwrap();
        let mut focus = FocusedApp {
            wm_class: Some("editor".into()),
            executable: Some("/opt/editor".into()),
            ..FocusedApp::default()
        };
        assert!(!matches_focus(&focus, &binding));
        focus.executable = Some("/opt/Editor".into());
        assert!(matches_focus(&focus, &binding));
        focus.wm_class = None;
        assert!(!matches_focus(&focus, &binding));
        let empty: Binding =
            serde_json::from_value(serde_json::json!({"server":"editor"})).unwrap();
        assert!(!matches_focus(&focus, &empty));
    }

    #[test]
    fn prefers_bound_healthy_server() {
        let focus = FocusedApp {
            wm_class: Some("Google-chrome".into()),
            app_id: None,
            executable: None,
        };
        let bindings = vec![Binding {
            server: "chrome".into(),
            wm_class: Some("google-chrome".into()),
            app_id: None,
            executable: None,
        }];
        let healthy = vec!["chrome".into(), "filesystem".into()];
        assert_eq!(
            resolve_preferred(&focus, &bindings, &healthy).as_deref(),
            Some("chrome")
        );
        assert_eq!(
            companion_mode(Some("chrome"), Some("mouse")).as_deref(),
            Some("mcp:chrome")
        );
        assert_eq!(
            companion_mode(None, Some("mouse")).as_deref(),
            Some("mouse")
        );
    }

    #[test]
    fn unbound_or_unhealthy_falls_back() {
        let focus = FocusedApp {
            wm_class: Some("Code".into()),
            ..FocusedApp::default()
        };
        let bindings = vec![Binding {
            server: "cursor".into(),
            wm_class: Some("Code".into()),
            app_id: None,
            executable: None,
        }];
        assert!(resolve_preferred(&focus, &bindings, &["filesystem".into()]).is_none());
        assert!(resolve_preferred(&FocusedApp::default(), &bindings, &["cursor".into()]).is_none());
    }

    #[test]
    fn binding_cache_invalidates_stale_routes_and_recovers_without_mtime_changes() {
        let dir = std::env::temp_dir().join(format!("visor-bindings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mcp-bindings.json");
        let mut cache = BindingCache::default();
        let now = Instant::now();
        assert_eq!(cache.current(&dir, now).status, BindingStatus::Missing);
        let raw = r#"{"bindings":[{"server":"editor","wm_class":"Editor"}]}"#;
        fs::write(&path, raw).unwrap();
        let focus = FocusedApp {
            wm_class: Some("Editor".into()),
            ..Default::default()
        };
        let healthy = vec!["editor".into()];
        assert_eq!(cache.current(&dir, now).status, BindingStatus::Missing);
        assert!(cache
            .current(&dir, now + BINDINGS_REFRESH)
            .explain(Some(&focus), &healthy)
            .preferred
            .is_some());
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, raw.replace("wm_class", "wm_clazz")).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        let invalid = cache
            .current(&dir, now + BINDINGS_REFRESH * 2)
            .explain(Some(&focus), &healthy);
        assert_eq!(invalid.bindings_status, BindingStatus::Invalid);
        assert_eq!(invalid.reason, MatchReason::BindingsUnavailable);
        assert!(invalid.preferred.is_none());
        fs::write(&path, raw).unwrap();
        assert_eq!(
            cache.current(&dir, now + BINDINGS_REFRESH * 3).status,
            BindingStatus::Ready
        );
        fs::remove_file(&path).unwrap();
        assert_eq!(
            cache.current(&dir, now + BINDINGS_REFRESH * 4).status,
            BindingStatus::Missing
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn invalid_selectors_and_unknown_fields_fail_the_entire_file() {
        let dir = std::env::temp_dir().join(format!("visor-bindings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        for raw in [
            r#"{}"#,
            r#"{"bindings":[],"policy":"private"}"#,
            r#"{"bindings":[{"server":"x","wm_class":"Editor","executable_path":"private"}]}"#,
            r#"{"bindings":[{"server":"x"}]}"#,
            r#"{"bindings":[{"server":"x","wm_class":"Editor","app_id":" "}]}"#,
            r#"{"bindings":[{"server":"x","executable":"editor"}]}"#,
            r#"{"bindings":[{"server":"","wm_class":"Editor"}]}"#,
            r#"{"bindings":[{"server":"x","wm_class":"Editor"},{"server":"bad"}]}"#,
            "private invalid JSON",
        ] {
            fs::write(dir.join("mcp-bindings.json"), raw).unwrap();
            assert_eq!(read_bindings(&dir).unwrap_err(), BindingStatus::Invalid);
            let mut cache = BindingCache::default();
            let decision = cache.current(&dir, Instant::now()).explain(None, &[]);
            assert_eq!(decision.reason, MatchReason::BindingsUnavailable);
            let displayed = serde_json::to_string(&decision).unwrap();
            assert!(!displayed.contains("private"));
            assert!(decision.preferred.is_none());
        }
        fs::write(dir.join("mcp-bindings.json"), r#"{"bindings":[]}"#).unwrap();
        assert!(read_bindings(&dir).unwrap().bindings.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bounded_reads_reject_large_files_rule_counts_and_special_files() {
        let dir = std::env::temp_dir().join(format!("visor-bindings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mcp-bindings.json");
        fs::write(&path, vec![b' '; MAX_BYTES as usize + 1]).unwrap();
        assert_eq!(read_bindings(&dir).unwrap_err(), BindingStatus::TooLarge);
        let binding = serde_json::json!({"server":"x","wm_class":"Editor"});
        fs::write(
            &path,
            serde_json::json!({"bindings":vec![binding; MAX_BINDINGS + 1]}).to_string(),
        )
        .unwrap();
        assert_eq!(read_bindings(&dir).unwrap_err(), BindingStatus::TooLarge);
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert_eq!(read_bindings(&dir).unwrap_err(), BindingStatus::Unavailable);
        fs::remove_dir(&path).unwrap();
        let name = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert_eq!(read_bindings(&dir).unwrap_err(), BindingStatus::Unavailable);
        fs::remove_dir_all(dir).unwrap();
    }
}
