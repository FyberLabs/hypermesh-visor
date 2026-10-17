//! Desktop-driving phase for the companion.
//!
//! Mouse and type put the phase in `running`. There is no separate automation
//! runner. `start` and `stop` are the seam a runner can use without closing
//! the session. `clickable` on a move is reported by the caller. This process
//! does not discover clickable targets.

use uuid::Uuid;

use crate::input::MouseOp;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Off,
    Running,
    Paused,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Running => "running",
            Self::Paused => "paused",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind {
    Move,
    Click,
    Drag,
    Type,
}

impl DriveKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::Click => "click",
            Self::Drag => "drag",
            Self::Type => "type",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    None,
    Upgrading,
}

impl Notice {
    pub fn as_str(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Upgrading => Some("upgrading"),
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "upgrading" => Some(Self::Upgrading),
            "clear" | "none" => Some(Self::None),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Start,
    Pause,
    Resume,
    Stop,
}

impl Control {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "start" => Some(Self::Start),
            "pause" => Some(Self::Pause),
            "resume" => Some(Self::Resume),
            "stop" => Some(Self::Stop),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drive {
    pub kind: DriveKind,
    pub x: i32,
    pub y: i32,
    pub from_x: i32,
    pub from_y: i32,
    pub clickable: bool,
    pub distance_px: u32,
    pub elapsed_ms: u32,
    pub at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gate {
    session: Option<Uuid>,
    phase: Phase,
    drive: Option<Drive>,
    pointer: Option<(i32, i32)>,
    last_at_ms: Option<u64>,
    notice: Notice,
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            session: None,
            phase: Phase::Off,
            drive: None,
            pointer: None,
            last_at_ms: None,
            notice: Notice::None,
        }
    }
}

impl Gate {
    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn session(&self) -> Option<Uuid> {
        self.session
    }

    pub fn drive(&self) -> Option<&Drive> {
        self.drive.as_ref()
    }

    pub fn notice(&self) -> Notice {
        self.notice
    }

    pub fn set_notice(&mut self, notice: Notice) {
        self.notice = notice;
    }

    pub fn blocks(&self, session: Uuid) -> bool {
        self.phase == Phase::Paused && self.session == Some(session)
    }

    pub fn clear_session(&mut self, session: Uuid) {
        if self.session == Some(session) {
            self.reset();
        }
    }

    pub fn reset(&mut self) {
        let notice = self.notice;
        *self = Self {
            notice,
            ..Self::default()
        };
    }

    /// `start` needs the session the companion is showing. The other actions
    /// apply to the drive already in progress.
    pub fn control(&mut self, session: Option<Uuid>, action: Control) -> Result<(), ()> {
        match action {
            Control::Start => {
                let Some(id) = session else {
                    return Err(());
                };
                self.session = Some(id);
                self.phase = Phase::Running;
                Ok(())
            }
            Control::Pause => {
                if self.phase == Phase::Running {
                    self.phase = Phase::Paused;
                }
                Ok(())
            }
            Control::Resume => {
                if self.phase == Phase::Paused {
                    self.phase = Phase::Running;
                }
                Ok(())
            }
            Control::Stop => {
                self.reset();
                Ok(())
            }
        }
    }

    pub fn record_mouse(
        &mut self,
        session: Uuid,
        op: &MouseOp,
        from: Option<(i32, i32)>,
        now_ms: u64,
    ) {
        if self.blocks(session) {
            return;
        }
        let (kind, x, y, from_x, from_y, clickable) = match *op {
            MouseOp::Move { x, y, clickable } => {
                let (from_x, from_y) = from.or(self.pointer).unwrap_or((x, y));
                (DriveKind::Move, x, y, from_x, from_y, clickable)
            }
            MouseOp::Click { x, y, .. } => {
                let (from_x, from_y) = from.or(self.pointer).unwrap_or((x, y));
                (DriveKind::Click, x, y, from_x, from_y, false)
            }
            MouseOp::Drag {
                x, y, to_x, to_y, ..
            } => (DriveKind::Drag, to_x, to_y, x, y, false),
        };
        self.store(
            session,
            kind,
            x,
            y,
            from_x,
            from_y,
            clickable,
            now_ms,
        );
    }

    pub fn record_type(&mut self, session: Uuid, now_ms: u64) {
        if self.blocks(session) {
            return;
        }
        let (x, y) = self.pointer.unwrap_or((0, 0));
        self.store(session, DriveKind::Type, x, y, x, y, false, now_ms);
    }

    fn store(
        &mut self,
        session: Uuid,
        kind: DriveKind,
        x: i32,
        y: i32,
        from_x: i32,
        from_y: i32,
        clickable: bool,
        now_ms: u64,
    ) {
        let elapsed_ms = self
            .last_at_ms
            .map(|then| now_ms.saturating_sub(then).min(u32::MAX as u64) as u32)
            .unwrap_or(0);
        self.session = Some(session);
        self.phase = Phase::Running;
        self.pointer = Some((x, y));
        self.last_at_ms = Some(now_ms);
        self.drive = Some(Drive {
            kind,
            x,
            y,
            from_x,
            from_y,
            clickable,
            distance_px: distance(from_x, from_y, x, y),
            elapsed_ms,
            at_ms: now_ms,
        });
    }
}

pub fn distance(x0: i32, y0: i32, x1: i32, y1: i32) -> u32 {
    let dx = i64::from(x1) - i64::from(x0);
    let dy = i64::from(y1) - i64::from(y0);
    ((dx * dx + dy * dy) as f64).sqrt().round() as u32
}

pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Button;

    #[test]
    fn mouse_and_type_run_until_pause_stop_or_close() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        assert!(!gate.blocks(id));
        gate.record_mouse(
            id,
            &MouseOp::Move {
                x: 10,
                y: 0,
                clickable: false,
            },
            Some((0, 0)),
            1_000,
        );
        assert_eq!(gate.phase(), Phase::Running);
        let drive = gate.drive().unwrap();
        assert_eq!(drive.kind, DriveKind::Move);
        assert_eq!(drive.distance_px, 10);
        assert!(!drive.clickable);

        gate.record_mouse(
            id,
            &MouseOp::Move {
                x: 300,
                y: 0,
                clickable: true,
            },
            None,
            1_100,
        );
        let drive = gate.drive().unwrap();
        assert!(drive.clickable);
        assert_eq!(drive.distance_px, 290);
        assert_eq!(drive.elapsed_ms, 100);
        assert_eq!(drive.from_x, 10);

        gate.control(Some(id), Control::Pause).unwrap();
        assert!(gate.blocks(id));
        gate.record_type(id, 2_000);
        assert_eq!(gate.drive().unwrap().kind, DriveKind::Move);
        gate.control(Some(id), Control::Resume).unwrap();
        gate.record_type(id, 2_100);
        assert_eq!(gate.drive().unwrap().kind, DriveKind::Type);

        gate.control(None, Control::Stop).unwrap();
        assert_eq!(gate.phase(), Phase::Off);
        assert!(gate.drive().is_none());

        assert!(gate.control(None, Control::Start).is_err());
        gate.control(Some(id), Control::Start).unwrap();
        assert_eq!(gate.phase(), Phase::Running);
        gate.clear_session(id);
        assert_eq!(gate.phase(), Phase::Off);
    }

    #[test]
    fn drag_distance_uses_the_segment_and_notice_survives_stop() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        gate.set_notice(Notice::Upgrading);
        gate.record_mouse(
            id,
            &MouseOp::Drag {
                x: 0,
                y: 0,
                to_x: 3,
                to_y: 4,
                button: Button::Left,
            },
            None,
            50,
        );
        assert_eq!(gate.drive().unwrap().distance_px, 5);
        assert_eq!(gate.drive().unwrap().kind, DriveKind::Drag);
        gate.control(None, Control::Stop).unwrap();
        assert_eq!(gate.notice(), Notice::Upgrading);
        assert_eq!(Notice::parse("upgrading"), Some(Notice::Upgrading));
        assert_eq!(Notice::parse("nope"), None);
    }
}
