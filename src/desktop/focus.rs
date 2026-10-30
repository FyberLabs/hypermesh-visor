//! Best-effort focused-app probes for Wayland (and helpers).

use std::process::Command;

use crate::mcp::FocusedApp;

/// Probe common Wayland compositors for the focused window identity.
pub fn wayland_focused_app() -> Option<FocusedApp> {
    hyprland().or_else(sway).or_else(kwin_xwayland_class)
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
    hyprland_identity(&v, process_executable)
}

fn hyprland_identity(
    v: &serde_json::Value,
    executable: impl FnOnce(u32) -> Option<String>,
) -> Option<FocusedApp> {
    let app = FocusedApp {
        wm_class: identity_string(v.get("class")),
        app_id: identity_string(v.get("initialClass")),
        executable: process_id(v).and_then(executable),
    };
    (!app.is_empty()).then_some(app)
}

fn identity_string(value: Option<&serde_json::Value>) -> Option<String> {
    value
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

fn process_id(value: &serde_json::Value) -> Option<u32> {
    value
        .get("pid")?
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
}

/// Best-effort local process hint, never a title or command line. A missing,
/// inaccessible, foreign-user, or deleted process contributes no executable.
fn process_executable(pid: u32) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let proc = std::path::PathBuf::from(format!("/proc/{pid}"));
    if std::fs::metadata(&proc).ok()?.uid() != unsafe { libc::geteuid() } {
        return None;
    }
    let path = std::fs::read_link(proc.join("exe")).ok()?;
    let value = path.to_str()?;
    if !path.is_absolute() || value.ends_with(" (deleted)") {
        return None;
    }
    Some(value.to_string())
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
    sway_identity(&tree, process_executable)
}

fn sway_identity(
    tree: &serde_json::Value,
    executable: impl FnOnce(u32) -> Option<String>,
) -> Option<FocusedApp> {
    let focused = find_focused(tree)?;
    let app_id = identity_string(focused.get("app_id"));
    let app = FocusedApp {
        wm_class: identity_string(focused.pointer("/window_properties/class"))
            .or_else(|| app_id.clone()),
        app_id,
        executable: process_id(focused).and_then(executable),
    };
    (!app.is_empty()).then_some(app)
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
        .args(["org.kde.KWin", "/KWin", "org.kde.KWin.activeWindowClass"])
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
    fn hyprland_never_uses_titles_as_executable_identity() {
        let window = serde_json::json!({"class":"Editor", "initialClass":"org.editor", "initialTitle":"/usr/bin/trusted", "title":"secret document", "pid":42});
        let app = hyprland_identity(&window, |pid| {
            assert_eq!(pid, 42);
            Some("/usr/bin/editor".into())
        })
        .unwrap();
        assert_eq!(app.executable.as_deref(), Some("/usr/bin/editor"));
        assert_eq!(app.app_id.as_deref(), Some("org.editor"));
        let missing = hyprland_identity(&window, |_| None).unwrap();
        assert!(missing.executable.is_none());
        assert!(
            hyprland_identity(&serde_json::json!({"initialTitle":"Editor"}), |_| panic!(
                "no pid"
            ))
            .is_none()
        );
    }

    #[test]
    fn invalid_pids_do_not_probe_processes() {
        for pid in [
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(4294967296_u64),
            serde_json::json!("42"),
            serde_json::json!(1.5),
        ] {
            let app = hyprland_identity(&serde_json::json!({"class":"Editor", "pid":pid}), |_| {
                panic!("invalid pid")
            })
            .unwrap();
            assert!(app.executable.is_none());
        }
    }

    #[test]
    fn sway_floating_app_restart_replaces_executable_hint() {
        for (pid, expected) in [(42, "/opt/editor-v1"), (43, "/opt/editor-v2")] {
            let tree = serde_json::json!({"nodes":[], "floating_nodes":[{"focused":true,"app_id":"org.editor","pid":pid,"name":"not an executable"}]});
            let app = sway_identity(&tree, |got| {
                assert_eq!(got, pid);
                Some(expected.into())
            })
            .unwrap();
            assert_eq!(app.executable.as_deref(), Some(expected));
            assert_eq!(app.app_id.as_deref(), Some("org.editor"));
        }
    }

    #[test]
    fn local_process_executable_is_an_absolute_path() {
        assert_eq!(
            process_executable(std::process::id()),
            std::env::current_exe()
                .unwrap()
                .to_str()
                .map(str::to_string)
        );
        assert!(process_executable(0).is_none());
    }

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
