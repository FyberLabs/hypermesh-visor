//! Visor pose for the desktop bug. Sign-in lives in `hypermesh-session`.
//! The access token is not written down. The visor session vault is not touched.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pose {
    Idle,
    Active {
        purpose: String,
        /// Last desktop verb (`view`, `mouse`, …), when any.
        verb: Option<String>,
        /// Prefer-MCP display: `mcp:<id>` when healthy binding matches, else the verb.
        mode: Option<String>,
    },
}

/// The visor poll is a GET of session purpose and verb. It does not send a key.
pub fn visor_request(host: &str) -> String {
    format!("GET /companion HTTP/1.1\r\nhost: {host}\r\nconnection: close\r\n\r\n")
}

pub fn pose_from_companion(body: &str) -> Pose {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return Pose::Idle;
    };
    if value.get("open").and_then(|open| open.as_bool()) != Some(true) {
        return Pose::Idle;
    }
    let Some(session) = value.get("session").filter(|session| !session.is_null()) else {
        return Pose::Idle;
    };
    let purpose = session
        .get("purpose")
        .and_then(|purpose| purpose.as_str())
        .unwrap_or("")
        .to_string();
    let verb = session
        .get("verb")
        .and_then(|verb| verb.as_str())
        .map(str::to_string);
    let mode = session
        .get("mode")
        .and_then(|mode| mode.as_str())
        .map(str::to_string)
        .or_else(|| {
            // Prefer mcp:<id> when the visor says prefer_mcp.
            if session
                .get("prefer_mcp")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                session
                    .get("mcp")
                    .and_then(|mcp| mcp.as_str())
                    .map(|id| format!("mcp:{id}"))
            } else {
                verb.clone()
            }
        });
    Pose::Active {
        purpose,
        verb,
        mode,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visor_poll_does_not_carry_a_key() {
        let request = visor_request("127.0.0.1:9847");
        assert!(request.starts_with("GET /companion "));
        assert!(!request.to_ascii_lowercase().contains("api_key"));
        assert!(!request.contains("authorization"));
    }

    #[test]
    fn idle_without_a_session() {
        assert_eq!(pose_from_companion(r#"{"open":false}"#), Pose::Idle);
        match pose_from_companion(
            r#"{"open":true,"session":{"id":"x","purpose":"review the desktop","verb":"view","mode":"view","prefer_mcp":false}}"#,
        ) {
            Pose::Active {
                purpose,
                verb,
                mode,
            } => {
                assert_eq!(purpose, "review the desktop");
                assert_eq!(verb.as_deref(), Some("view"));
                assert_eq!(mode.as_deref(), Some("view"));
            }
            Pose::Idle => panic!("expected active"),
        }
    }

    #[test]
    fn prefers_mcp_mode_on_the_label() {
        match pose_from_companion(
            r#"{"open":true,"session":{"id":"x","purpose":"drive","verb":"mouse","mode":"mcp:chrome","mcp":"chrome","prefer_mcp":true}}"#,
        ) {
            Pose::Active { mode, verb, .. } => {
                assert_eq!(mode.as_deref(), Some("mcp:chrome"));
                assert_eq!(verb.as_deref(), Some("mouse"));
            }
            Pose::Idle => panic!("expected active"),
        }
    }
}
