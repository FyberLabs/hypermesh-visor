//! Focused-app → MCP server bindings (companion prefer-MCP).

use std::fs;
use std::path::Path;

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

/// First binding that matches the focused app and names an attached healthy server.
pub fn resolve_preferred(
    focus: &FocusedApp,
    bindings: &[Binding],
    healthy_servers: &[String],
) -> Option<String> {
    if focus.is_empty() {
        return None;
    }
    for binding in bindings {
        if !matches_focus(focus, binding) {
            continue;
        }
        if healthy_servers.iter().any(|id| id == &binding.server) {
            return Some(binding.server.clone());
        }
    }
    None
}

fn matches_focus(focus: &FocusedApp, binding: &Binding) -> bool {
    let mut any = false;
    if let Some(want) = binding.wm_class.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        any = true;
        if !eq_ignore_case(focus.wm_class.as_deref(), want) {
            return false;
        }
    }
    if let Some(want) = binding.app_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
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
        if !eq_ignore_case(focus.executable.as_deref(), want) {
            return false;
        }
    }
    any
}

fn eq_ignore_case(got: Option<&str>, want: &str) -> bool {
    got.map(|g| g.eq_ignore_ascii_case(want)).unwrap_or(false)
}

/// Companion display mode: prefer `mcp:<id>` when a healthy binding matches.
pub fn companion_mode(
    preferred_mcp: Option<&str>,
    verb: Option<&str>,
) -> Option<String> {
    if let Some(id) = preferred_mcp.filter(|s| !s.is_empty()) {
        return Some(format!("mcp:{id}"));
    }
    verb.filter(|s| !s.is_empty()).map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
