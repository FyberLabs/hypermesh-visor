//! Focused-app → MCP server bindings (companion prefer-MCP).

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;

use super::McpError;

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct BindingsFile {
    #[serde(default)]
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
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

/// How often `GET /companion` may stat `mcp-bindings.json`.
pub const BINDINGS_REFRESH: Duration = Duration::from_secs(1);

/// In-memory bindings. The file is read only when this is due and the mtime changed.
#[derive(Debug, Default)]
pub struct BindingCache {
    checked_at: Option<Instant>,
    mtime: Option<SystemTime>,
    bindings: Vec<Binding>,
}

impl BindingCache {
    pub fn current(&mut self, dir: &Path, now: Instant) -> Vec<Binding> {
        let due = self
            .checked_at
            .map(|at| now.saturating_duration_since(at) >= BINDINGS_REFRESH)
            .unwrap_or(true);
        if !due {
            return self.bindings.clone();
        }
        let path = dir.join("mcp-bindings.json");
        let mtime = fs::metadata(&path).and_then(|meta| meta.modified()).ok();
        refresh_cached_bindings(self, now, BINDINGS_REFRESH, mtime, || {
            load_bindings(dir).unwrap_or_default().bindings
        })
    }
}

/// Apply one refresh decision. `load` runs only on the first fill or when `mtime` changes.
pub fn refresh_cached_bindings(
    cache: &mut BindingCache,
    now: Instant,
    interval: Duration,
    mtime: Option<SystemTime>,
    load: impl FnOnce() -> Vec<Binding>,
) -> Vec<Binding> {
    if cache
        .checked_at
        .is_some_and(|at| now.saturating_duration_since(at) < interval)
    {
        return cache.bindings.clone();
    }
    if cache.checked_at.is_some() && cache.mtime == mtime {
        cache.checked_at = Some(now);
        return cache.bindings.clone();
    }
    let bindings = load();
    cache.bindings.clone_from(&bindings);
    cache.mtime = mtime;
    cache.checked_at = Some(now);
    bindings
}

pub fn load_bindings(dir: &Path) -> Result<BindingsFile, McpError> {
    let path = dir.join("mcp-bindings.json");
    if !path.exists() {
        return Ok(BindingsFile::default());
    }
    let raw = fs::read_to_string(&path)
        .map_err(|e| McpError::message(format!("read {}: {e}", path.display())))?;
    serde_json::from_str(&raw)
        .map_err(|e| McpError::message(format!("parse {}: {e}", path.display())))
}

/// A bounded explanation of routing. No config values, credentials, or titles.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MatchDecision {
    #[serde(skip)]
    pub preferred: Option<String>,
    pub reason: MatchReason,
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
                "reason":"matched", "binding_index":1, "identity_matches":3, "healthy_matches":2
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
    fn bindings_reload_only_after_the_interval_when_mtime_changes() {
        let mut cache = BindingCache::default();
        let start = Instant::now();
        let mtime = SystemTime::UNIX_EPOCH;
        let mut loads = 0;
        let first =
            refresh_cached_bindings(&mut cache, start, BINDINGS_REFRESH, Some(mtime), || {
                loads += 1;
                vec![Binding {
                    server: "chrome".into(),
                    wm_class: Some("chrome".into()),
                    app_id: None,
                    executable: None,
                }]
            });
        assert_eq!(loads, 1);
        assert_eq!(first[0].server, "chrome");
        let again = refresh_cached_bindings(
            &mut cache,
            start + Duration::from_millis(40),
            BINDINGS_REFRESH,
            Some(mtime + Duration::from_secs(5)),
            || {
                loads += 1;
                Vec::new()
            },
        );
        assert_eq!(loads, 1);
        assert_eq!(again[0].server, "chrome");
        let same = refresh_cached_bindings(
            &mut cache,
            start + BINDINGS_REFRESH,
            BINDINGS_REFRESH,
            Some(mtime),
            || {
                loads += 1;
                Vec::new()
            },
        );
        assert_eq!(loads, 1);
        assert_eq!(same[0].server, "chrome");
        let changed = refresh_cached_bindings(
            &mut cache,
            start + BINDINGS_REFRESH + BINDINGS_REFRESH,
            BINDINGS_REFRESH,
            Some(mtime + Duration::from_secs(5)),
            || {
                loads += 1;
                vec![Binding {
                    server: "editor".into(),
                    wm_class: None,
                    app_id: None,
                    executable: None,
                }]
            },
        );
        assert_eq!(loads, 2);
        assert_eq!(changed[0].server, "editor");
    }
}
