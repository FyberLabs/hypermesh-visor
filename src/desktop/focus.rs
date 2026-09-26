//! Best-effort focused-app probes for Wayland (and helpers).

use std::process::Command;

use crate::mcp::FocusedApp;

/// Probe common Wayland compositors for the focused window identity.
pub fn wayland_focused_app() -> Option<FocusedApp> {
    hyprland()
        .or_else(sway)
        .or_else(kwin_xwayland_class)
}

fn hyprland() -> Option<FocusedApp> {
    let out = Command::new("hyprctl")
        .args(["-j", "activewindow"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let class = v.get("class").and_then(|c| c.as_str()).filter(|s| !s.is_empty())?;
    let app_id = v
        .get("initialClass")
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Some(FocusedApp {
        wm_class: Some(class.to_string()),
        app_id,
        executable: v
            .get("initialTitle")
            .and_then(|c| c.as_str())
            .map(str::to_string),
    })
}

fn sway() -> Option<FocusedApp> {
    let out = Command::new("swaymsg")
        .args(["-t", "get_tree"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let tree: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let focused = find_focused(&tree)?;
    let app_id = focused
        .get("app_id")
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let class = focused
        .pointer("/window_properties/class")
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| app_id.clone());
    if class.is_none() && app_id.is_none() {
        return None;
    }
    Some(FocusedApp {
        wm_class: class,
        app_id,
        executable: None,
    })
}

fn find_focused(node: &serde_json::Value) -> Option<&serde_json::Value> {
    if node.get("focused").and_then(|f| f.as_bool()) == Some(true) {
        return Some(node);
    }
    for key in ["nodes", "floating_nodes"] {
        if let Some(arr) = node.get(key).and_then(|a| a.as_array()) {
            for child in arr {
                if let Some(hit) = find_focused(child) {
                    return Some(hit);
                }
            }
        }
    }
    None
}

/// KWin sometimes still exposes an X11 class for XWayland windows.
fn kwin_xwayland_class() -> Option<FocusedApp> {
    let out = Command::new("qdbus")
        .args([
            "org.kde.KWin",
            "/KWin",
            "org.kde.KWin.activeWindowClass",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let class = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if class.is_empty() {
        return None;
    }
    Some(FocusedApp {
        wm_class: Some(class),
        app_id: None,
        executable: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_focused_walks_sway_tree() {
        let tree = serde_json::json!({
            "nodes": [{
                "focused": false,
                "nodes": [{
                    "focused": true,
                    "app_id": "firefox",
                    "window_properties": {"class": "firefox"}
                }]
            }]
        });
        let hit = find_focused(&tree).unwrap();
        assert_eq!(hit["app_id"], "firefox");
    }
}
