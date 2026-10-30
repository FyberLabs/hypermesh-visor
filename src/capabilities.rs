//! Implementation discovery only. This does not probe devices or grant access.
use serde_json::{json, Value};

pub(crate) fn describe(backend: &str) -> Value {
    let capture = match backend {
        "x11" | "wayland" => "conditional",
        "fixture" => "fixture",
        _ => "unavailable",
    };
    let input = match backend {
        "x11" => "conditional",
        "fixture" => "fixture",
        "wayland" => "disabled",
        _ => "unavailable",
    };
    json!({
        "schema_version": 1,
        "backend": backend,
        "readiness_probed": false,
        "authorization_granted": false,
        "desktop": {
            "view": capture, "watch": capture, "listen": capture,
            "mouse": input, "type": input,
            "input_disabled_reason": if backend == "wayland" {
                Some(crate::desktop::WAYLAND_AUTOMATION)
            } else { None },
            "mouse_actions": ["move", "click", "drag", "scroll"],
            "mouse_buttons": ["left", "right", "middle"],
            "type_fields": ["text", "keys", "secret"],
            "key_actions": ["tap", "down", "up"],
            "unsupported": ["clipboard", "display_enumeration", "accessibility_tree", "audio_playback"],
            "coordinates": {
                "space": if backend == "x11" { "root_window_pixels" } else { "unspecified" },
                "display_selection": false,
                "dpi_transform": false
            },
            "audio_format": crate::desktop::AUDIO_FORMAT
        },
        "limits": {
            "type_max_keystrokes": crate::input::TYPE_MAX_KEYSTROKES,
            "stream_max_bytes": crate::stream::MAX_STREAM_BYTES,
            "scroll_max_total_ticks": crate::input::SCROLL_MAX_TICKS
        },
        "requirements": {
            "desktop_verbs_require_session": true,
            "input_requires_automation_gate": true,
            "tcp_requires_bearer": true
        },
        "mcp": {
            "discovery": "/mcp/discovery",
            "trust": "/mcp/trust",
            "trust_requires_companion_socket": true,
            "attached_tools": "/session/{id}/mcp",
            "refresh_tools": "/session/{id}/mcp/refresh",
            "refresh_actions": ["refresh", "reconnect"],
            "inverse_tools": ["view", "mouse_move", "mouse_click", "mouse_scroll", "type_text"]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_never_advertises_disabled_or_unknown_input_as_ready() {
        for (backend, status) in [
            ("x11", "conditional"),
            ("wayland", "disabled"),
            ("fixture", "fixture"),
            ("unknown", "unavailable"),
            ("future", "unavailable"),
        ] {
            let value = describe(backend);
            assert_eq!(value["desktop"]["mouse"], status);
            assert_eq!(value["desktop"]["type"], status);
            assert_eq!(value["readiness_probed"], false);
            assert_eq!(value["authorization_granted"], false);
        }
        assert!(describe("wayland")["desktop"]["input_disabled_reason"]
            .as_str()
            .unwrap()
            .contains("#17"));
    }
}
