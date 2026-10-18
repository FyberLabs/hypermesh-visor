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
    /// False when a type sample had no pointer to record. The companion must
    /// not compare that placeholder with the real pointer.
    pub pointer_known: bool,
    /// Gate resume sequence at the time this sample was recorded. The
    /// companion ignores a sample from the sequence before a resume.
    pub resume_seq: u64,
    /// Injection sequence at the time this sample was recorded. The companion
    /// defers fallback checks until a drive carries the sequence it saw in flight.
    pub inject_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gate {
    session: Option<Uuid>,
    phase: Phase,
    drive: Option<Drive>,
    pointer: Option<(i32, i32)>,
    last_at_ms: Option<u64>,
    notice: Notice,
    /// Bumped when the gate is reset. A drive captured before that bump does
    /// not record, so an in-flight mouse cannot revive a stop.
    generation: u64,
    /// Bumped on resume and start. A drive keeps the sequence from when it
    /// was recorded, so the companion can tell a sample from before the
    /// transition apart from one recorded after it and before the next poll.
    resume_seq: u64,
    /// Bumped when mouse or type is about to inject. Stays put when that
    /// injection finishes so a drive can be matched to it.
    inject_seq: u64,
    /// Highest sequence whose desktop call has returned, including a failure
    /// that recorded no drive. The companion stops deferring at this sequence.
    last_settled_seq: u64,
    /// True from the moment an injection starts until its desktop call returns.
    injecting: bool,
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
            generation: 0,
            resume_seq: 0,
            inject_seq: 0,
            last_settled_seq: 0,
            injecting: false,
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

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn resume_seq(&self) -> u64 {
        self.resume_seq
    }

    pub fn inject_seq(&self) -> u64 {
        self.inject_seq
    }

    pub fn injecting(&self) -> bool {
        self.injecting
    }

    pub fn last_settled_seq(&self) -> u64 {
        self.last_settled_seq
    }

    /// Mark an injection before the desktop call. The sequence is the value
    /// the drive will store if this injection records.
    pub fn begin_inject(&mut self) -> u64 {
        self.inject_seq = self.inject_seq.saturating_add(1);
        self.injecting = true;
        self.inject_seq
    }

    /// The desktop call returned. A failure records no drive, so this sequence
    /// is settled on its own and the companion does not keep deferring.
    pub fn end_inject(&mut self) {
        self.injecting = false;
        self.last_settled_seq = self.last_settled_seq.max(self.inject_seq);
    }

    /// A pause covers the whole desktop. Every session injects into the same
    /// pointer, so another session must not move it until resume.
    pub fn blocks(&self, _session: Uuid) -> bool {
        self.phase == Phase::Paused
    }

    /// Losing the driving session ends a running automation. A user pause
    /// stays paused: closing the session must not hand the pointer back.
    pub fn clear_session(&mut self, session: Uuid) {
        if self.session != Some(session) {
            return;
        }
        if self.phase == Phase::Paused {
            self.session = None;
            self.drive = None;
            self.generation = self.generation.wrapping_add(1);
            return;
        }
        self.reset();
    }

    pub fn reset(&mut self) {
        let notice = self.notice;
        let generation = self.generation.wrapping_add(1);
        // Keep the sequence. Rewinding it would make a drive after the next
        // start look older than a cutoff the companion still holds.
        let resume_seq = self.resume_seq;
        let inject_seq = self.inject_seq;
        let last_settled_seq = self.last_settled_seq;
        *self = Self {
            notice,
            generation,
            resume_seq,
            inject_seq,
            last_settled_seq,
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
                // A sample from the previous drive would look like a user move
                // to the pointer fallback the moment this start is running.
                self.drive = None;
                self.bump_resume();
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
                    // The pointer is already away from this sample; that is
                    // why the automation paused. Leave the last point so a
                    // later type still has somewhere to land, but do not let
                    // the fallback compare against the pre-pause target.
                    self.drive = None;
                    self.bump_resume();
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
        generation: u64,
        now_ms: u64,
    ) {
        if !self.accepts(generation) {
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
        self.store(session, kind, x, y, from_x, from_y, clickable, true, now_ms);
    }

    pub fn record_type(
        &mut self,
        session: Uuid,
        at: Option<(i32, i32)>,
        generation: u64,
        now_ms: u64,
    ) {
        if !self.accepts(generation) {
            return;
        }
        let known = at.or(self.pointer);
        let (x, y, pointer_known) = match known {
            Some((x, y)) => (x, y, true),
            None => (0, 0, false),
        };
        self.store(
            session,
            DriveKind::Type,
            x,
            y,
            x,
            y,
            false,
            pointer_known,
            now_ms,
        );
    }

    fn accepts(&self, generation: u64) -> bool {
        self.generation == generation && self.phase != Phase::Paused
    }

    fn bump_resume(&mut self) {
        self.resume_seq = self.resume_seq.saturating_add(1);
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
        pointer_known: bool,
        now_ms: u64,
    ) {
        let elapsed_ms = self
            .last_at_ms
            .map(|then| now_ms.saturating_sub(then).min(u32::MAX as u64) as u32)
            .unwrap_or(0);
        self.session = Some(session);
        self.phase = Phase::Running;
        if pointer_known {
            self.pointer = Some((x, y));
        }
        self.last_at_ms = Some(now_ms);
        self.drive = Some(Drive {
            kind,
            x,
            y,
            from_x,
            from_y,
            clickable,
            distance_px: if pointer_known {
                distance(from_x, from_y, x, y)
            } else {
                0
            },
            elapsed_ms,
            at_ms: now_ms,
            pointer_known,
            resume_seq: self.resume_seq,
            inject_seq: self.inject_seq,
        });
    }
}

/// Leaving `paused` is the user-interrupt boundary. Pause itself is not.
pub fn lifts_user_pause(action: Control, phase: Phase) -> bool {
    phase == Phase::Paused && matches!(action, Control::Start | Control::Resume | Control::Stop)
}

pub fn distance(x0: i32, y0: i32, x1: i32, y1: i32) -> u32 {
    let dx = f64::from(x1) - f64::from(x0);
    let dy = f64::from(y1) - f64::from(y0);
    let pixels = dx.hypot(dy).round();
    if !pixels.is_finite() || pixels <= 0.0 {
        0
    } else if pixels >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        pixels as u32
    }
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
        let generation = gate.generation();
        gate.record_mouse(
            id,
            &MouseOp::Move {
                x: 10,
                y: 0,
                clickable: false,
            },
            Some((0, 0)),
            generation,
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
            generation,
            1_100,
        );
        let drive = gate.drive().unwrap();
        assert!(drive.clickable);
        assert_eq!(drive.distance_px, 290);
        assert_eq!(drive.elapsed_ms, 100);
        assert_eq!(drive.from_x, 10);

        gate.control(Some(id), Control::Pause).unwrap();
        assert!(gate.blocks(id));
        gate.record_type(id, None, generation, 2_000);
        assert_eq!(gate.drive().unwrap().kind, DriveKind::Move);
        gate.control(Some(id), Control::Resume).unwrap();
        assert!(gate.drive().is_none());
        gate.record_type(id, None, generation, 2_100);
        assert_eq!(gate.drive().unwrap().kind, DriveKind::Type);
        assert!(gate.drive().unwrap().pointer_known);
        assert_eq!((gate.drive().unwrap().x, gate.drive().unwrap().y), (300, 0));

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
    fn a_user_pause_survives_losing_the_driving_session() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        gate.control(Some(id), Control::Start).unwrap();
        gate.control(None, Control::Pause).unwrap();
        let generation = gate.generation();
        gate.clear_session(id);
        assert_eq!(gate.phase(), Phase::Paused);
        assert!(gate.session().is_none());
        assert!(gate.drive().is_none());
        assert_ne!(gate.generation(), generation);
        gate.clear_session(id);
        assert_eq!(gate.phase(), Phase::Paused);
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
            gate.generation(),
            50,
        );
        assert_eq!(gate.drive().unwrap().distance_px, 5);
        assert_eq!(gate.drive().unwrap().kind, DriveKind::Drag);
        gate.control(None, Control::Stop).unwrap();
        assert_eq!(gate.notice(), Notice::Upgrading);
        assert_eq!(Notice::parse("upgrading"), Some(Notice::Upgrading));
        assert_eq!(Notice::parse("nope"), None);
    }

    #[test]
    fn pause_blocks_every_session_and_stop_drops_a_stale_drive() {
        let mut gate = Gate::default();
        let driving = Uuid::new_v4();
        let other = Uuid::new_v4();
        let generation = gate.generation();
        gate.record_mouse(
            driving,
            &MouseOp::Move {
                x: 8,
                y: 9,
                clickable: false,
            },
            Some((8, 9)),
            generation,
            10,
        );
        gate.control(Some(driving), Control::Pause).unwrap();
        assert!(gate.blocks(other));
        gate.record_mouse(
            other,
            &MouseOp::Move {
                x: 1,
                y: 1,
                clickable: false,
            },
            Some((0, 0)),
            generation,
            20,
        );
        gate.record_type(other, Some((3, 4)), generation, 30);
        assert_eq!(gate.phase(), Phase::Paused);
        assert_eq!(gate.session(), Some(driving));
        assert_eq!(gate.drive().unwrap().kind, DriveKind::Move);

        gate.control(Some(driving), Control::Resume).unwrap();
        let seen = gate.generation();
        gate.control(None, Control::Stop).unwrap();
        assert_ne!(gate.generation(), seen);
        gate.record_mouse(
            other,
            &MouseOp::Click {
                x: 1,
                y: 1,
                button: Button::Left,
            },
            Some((1, 1)),
            seen,
            40,
        );
        gate.record_type(driving, Some((5, 6)), seen, 50);
        assert_eq!(gate.phase(), Phase::Off);
        assert!(gate.drive().is_none());
    }

    #[test]
    fn type_without_a_pointer_does_not_invent_the_origin() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        gate.record_type(id, None, gate.generation(), 10);
        let drive = gate.drive().unwrap();
        assert_eq!(drive.kind, DriveKind::Type);
        assert!(!drive.pointer_known);
        gate.record_type(id, Some((40, 70)), gate.generation(), 20);
        let drive = gate.drive().unwrap();
        assert!(drive.pointer_known);
        assert_eq!((drive.x, drive.y), (40, 70));
    }

    #[test]
    fn resume_and_start_drop_the_drive_sample() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        let generation = gate.generation();
        gate.record_mouse(
            id,
            &MouseOp::Move {
                x: 40,
                y: 50,
                clickable: false,
            },
            Some((0, 0)),
            generation,
            10,
        );
        gate.control(Some(id), Control::Pause).unwrap();
        assert!(gate.drive().is_some());
        gate.control(Some(id), Control::Resume).unwrap();
        assert_eq!(gate.phase(), Phase::Running);
        assert!(gate.drive().is_none());
        gate.record_type(id, None, generation, 20);
        let drive = gate.drive().unwrap();
        assert!(drive.pointer_known);
        assert_eq!((drive.x, drive.y), (40, 50));

        gate.control(Some(id), Control::Start).unwrap();
        assert_eq!(gate.phase(), Phase::Running);
        assert!(gate.drive().is_none());
        gate.record_type(id, None, generation, 30);
        assert_eq!((gate.drive().unwrap().x, gate.drive().unwrap().y), (40, 50));
    }

    #[test]
    fn resume_and_start_bump_a_sequence_the_drive_keeps() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        let generation = gate.generation();
        assert_eq!(gate.resume_seq(), 0);
        gate.record_mouse(
            id,
            &MouseOp::Move {
                x: 4,
                y: 5,
                clickable: false,
            },
            Some((0, 0)),
            generation,
            10,
        );
        assert_eq!(gate.drive().unwrap().resume_seq, 0);
        gate.control(Some(id), Control::Pause).unwrap();
        assert_eq!(gate.resume_seq(), 0, "pause does not start a new drive");
        gate.control(Some(id), Control::Resume).unwrap();
        assert_eq!(gate.resume_seq(), 1);
        assert!(gate.drive().is_none());
        gate.record_type(id, None, generation, 20);
        assert_eq!(gate.drive().unwrap().resume_seq, 1);
        gate.control(Some(id), Control::Resume).unwrap();
        assert_eq!(
            gate.resume_seq(),
            1,
            "resume while running is not a transition"
        );
        gate.control(Some(id), Control::Start).unwrap();
        assert_eq!(gate.resume_seq(), 2);
        assert!(gate.drive().is_none());
        gate.control(None, Control::Stop).unwrap();
        assert_eq!(gate.resume_seq(), 2, "stop must not rewind the sequence");
    }

    #[test]
    fn distance_survives_the_full_i32_span() {
        assert_eq!(distance(0, 0, 3, 4), 5);
        assert_eq!(distance(i32::MIN, 0, i32::MAX, 0), u32::MAX);
        let diagonal = distance(i32::MIN, i32::MIN, i32::MAX, i32::MAX);
        assert_eq!(diagonal, u32::MAX);
        assert!(lifts_user_pause(Control::Resume, Phase::Paused));
        assert!(lifts_user_pause(Control::Start, Phase::Paused));
        assert!(lifts_user_pause(Control::Stop, Phase::Paused));
        assert!(!lifts_user_pause(Control::Pause, Phase::Paused));
        assert!(!lifts_user_pause(Control::Start, Phase::Off));
        assert!(!lifts_user_pause(Control::Stop, Phase::Running));
    }

    #[test]
    fn an_injection_keeps_its_sequence_after_the_flag_drops() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        assert_eq!(gate.begin_inject(), 1);
        assert!(gate.injecting());
        gate.record_mouse(
            id,
            &MouseOp::Click {
                x: 4,
                y: 5,
                button: Button::Left,
            },
            Some((4, 5)),
            gate.generation(),
            10,
        );
        assert_eq!(gate.drive().unwrap().inject_seq, 1);
        gate.end_inject();
        assert!(!gate.injecting());
        assert_eq!(gate.inject_seq(), 1);
        assert_eq!(gate.last_settled_seq(), 1);
        gate.begin_inject();
        gate.end_inject();
        assert_eq!(gate.last_settled_seq(), 2);
        assert!(gate.drive().unwrap().inject_seq < gate.last_settled_seq());
        gate.begin_inject();
        gate.reset();
        assert!(!gate.injecting());
        assert_eq!(gate.last_settled_seq(), 2);
        assert_eq!(
            gate.inject_seq(),
            3,
            "reset must not reuse an in-flight sequence"
        );
    }
}
