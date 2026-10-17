//! Visor pose for the desktop bug. Sign-in lives in `hypermesh-session`.
//! The access token is not written down. The visor session vault is not touched.

use crate::automation::{MotionKind, VisorPhase};

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

/// One successful `GET /companion` body. `connected` is false only for the
/// disconnected placeholder; a parsed body is always connected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub connected: bool,
    pub upgrading: bool,
    pub pose: Pose,
    pub idle_ms: Option<u64>,
    pub phase: VisorPhase,
    pub drive: Option<DriveView>,
    /// Visor resume sequence. Bumps on resume and start, and stays put across stop.
    pub resume_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveView {
    pub kind: MotionKind,
    pub at_ms: u64,
    pub distance_px: u32,
    pub elapsed_ms: u32,
    pub clickable: bool,
    pub x: i32,
    pub y: i32,
    /// False for a type sample that had no pointer. The distance fallback
    /// must ignore it.
    pub pointer_known: bool,
    /// Resume sequence when this sample was recorded.
    pub resume_seq: u64,
}

impl Snapshot {
    pub fn disconnected() -> Self {
        Self {
            connected: false,
            upgrading: false,
            pose: Pose::Idle,
            idle_ms: None,
            phase: VisorPhase::Off,
            drive: None,
            resume_seq: 0,
        }
    }
}

pub fn snapshot_from_body(body: &str) -> Option<Snapshot> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    if !value.is_object() {
        return None;
    }
    let pose = pose_from_companion(body);
    let open = value.get("open").and_then(|open| open.as_bool()) == Some(true);
    let upgrading = value.get("notice").and_then(|notice| notice.as_str()) == Some("upgrading");
    let idle_ms = if open {
        value
            .pointer("/session/idle_ms")
            .and_then(|idle| idle.as_u64())
    } else {
        None
    };
    let phase = if open {
        match value
            .pointer("/automation/phase")
            .and_then(|phase| phase.as_str())
            .unwrap_or("off")
        {
            "running" => VisorPhase::Running,
            "paused" => VisorPhase::Paused,
            _ => VisorPhase::Off,
        }
    } else {
        VisorPhase::Off
    };
    let drive = if phase == VisorPhase::Off {
        None
    } else {
        value.pointer("/automation/drive").and_then(parse_drive)
    };
    let resume_seq = value
        .pointer("/automation/resume_seq")
        .and_then(|item| item.as_u64())
        .unwrap_or(0);
    Some(Snapshot {
        connected: true,
        upgrading,
        pose,
        idle_ms,
        phase,
        drive,
        resume_seq,
    })
}

fn parse_drive(value: &serde_json::Value) -> Option<DriveView> {
    let kind = match value.get("kind").and_then(|kind| kind.as_str())? {
        "move" => MotionKind::Move,
        "click" => MotionKind::Click,
        "drag" => MotionKind::Drag,
        "type" => MotionKind::Type,
        _ => return None,
    };
    Some(DriveView {
        kind,
        at_ms: value.get("at_ms").and_then(|item| item.as_u64())?,
        distance_px: value
            .get("distance_px")
            .and_then(|item| item.as_u64())
            .unwrap_or(0) as u32,
        elapsed_ms: value
            .get("elapsed_ms")
            .and_then(|item| item.as_u64())
            .unwrap_or(0) as u32,
        clickable: value
            .get("clickable")
            .and_then(|item| item.as_bool())
            .unwrap_or(false),
        x: value.get("x").and_then(|item| item.as_i64()).unwrap_or(0) as i32,
        y: value.get("y").and_then(|item| item.as_i64()).unwrap_or(0) as i32,
        pointer_known: value
            .get("pointer_known")
            .and_then(|item| item.as_bool())
            .unwrap_or(true),
        resume_seq: value
            .get("resume_seq")
            .and_then(|item| item.as_u64())
            .unwrap_or(0),
    })
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

    #[test]
    fn snapshot_reads_automation_and_notice() {
        let snap = snapshot_from_body(
            r#"{"open":true,"notice":"upgrading","automation":{"phase":"running","drive":{"kind":"move","x":4,"y":5,"distance_px":9,"elapsed_ms":40,"clickable":true,"at_ms":80}},"session":{"id":"x","purpose":"drive","verb":"mouse","idle_ms":12}}"#,
        )
        .unwrap();
        assert!(snap.connected);
        assert!(snap.upgrading);
        assert_eq!(snap.idle_ms, Some(12));
        assert_eq!(snap.phase, VisorPhase::Running);
        let drive = snap.drive.unwrap();
        assert_eq!(drive.kind, MotionKind::Move);
        assert!(drive.clickable);
        assert_eq!(drive.distance_px, 9);
        assert_eq!(snap.resume_seq, 0);
        assert_eq!(drive.resume_seq, 0);
        let stamped = snapshot_from_body(
            r#"{"open":true,"automation":{"phase":"running","resume_seq":4,"drive":{"kind":"click","x":1,"y":2,"at_ms":9,"resume_seq":4}}}"#,
        )
        .unwrap();
        assert_eq!(stamped.resume_seq, 4);
        assert_eq!(stamped.drive.unwrap().resume_seq, 4);
        assert!(snapshot_from_body("not-json").is_none());
        let idle = snapshot_from_body(r#"{"open":false}"#).unwrap();
        assert!(idle.connected);
        assert_eq!(idle.phase, VisorPhase::Off);
        assert_eq!(idle.pose, Pose::Idle);
    }
}
