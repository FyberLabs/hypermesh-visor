//! Desktop-driving phase for the companion.
//!
//! Mouse and type put the phase in `running`. There is no separate automation
//! runner. `start` and `stop` are the seam a runner can use without closing
//! the session. `clickable` on a move is reported by the caller. This process
//! does not discover clickable targets.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::input::MouseOp;

/// Shown when X11 cannot see a physical key press. Start, resume, mouse, and
/// type refuse with this sentence, the same way Wayland refuses automation.
pub const KEYBOARD_INTERRUPT: &str = "no keyboard interrupt available";

/// How long shutdown waits for an in-flight injection before it continues.
pub const SHUTDOWN_INJECTION_WAIT: Duration = Duration::from_secs(2);

/// The process exits if graceful shutdown is still running after this long,
/// including a desktop call stuck inside one request.
pub const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(5);

/// TCP `start` is refused with this sentence when no companion-socket poll
/// has landed inside the watchdog window.
pub const STALE_POLL: &str = "no recent companion poll";

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

/// Why [`Gate::apply`] refused a control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlReject {
    NeedsSession,
    StalePoll,
}

/// A control that arrived while an injection held the desktop. Replayed in
/// sequence order when the call unwinds, and only for the owner it named.
/// Start and resume are in this sequence so an older stop or pause cannot
/// erase them when the call ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Deferred {
    Stop,
    Pause,
    Close { session: Uuid, generation: u64 },
    KeyboardUnavailable,
    Start { session: Uuid, from_companion: bool },
    Resume { session: Uuid, from_companion: bool },
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

#[derive(Clone, Debug)]
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
    /// Monotonic id of the latest control. Deferred actions keep the value
    /// from when they were posted, so unwind replays them in order.
    control_seq: u64,
    /// Controls posted during an injection. Applied in order on unwind.
    deferred: Vec<(u64, Deferred)>,
    /// Cancellation generation. Published with the phase in one word.
    epoch: u64,
    /// Last instant the companion itself extended the watchdog: a socket
    /// poll, or the one grace from a socket transition into `running`.
    /// The first injection may arm it once. TCP start does not write it,
    /// and reset does not clear it.
    trusted_at: Option<Instant>,
    /// Last companion-socket poll, including one that arrived while the
    /// phase was off or paused. TCP start may enter `running` only while
    /// this instant is inside the watchdog window.
    last_poll: Option<Instant>,
    /// False after the companion reports that X11 cannot see key presses.
    keyboard_interrupt: bool,
    /// Lock-free view for injection threads. Bits 0–1 are the phase
    /// (`0` off, `1` running, `2` paused) and bits 2–63 are [`Self::epoch`].
    /// Only [`Gate`] methods store it.
    published: Arc<AtomicU64>,
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
            control_seq: 0,
            deferred: Vec::new(),
            epoch: 0,
            trusted_at: None,
            last_poll: None,
            keyboard_interrupt: true,
            published: Arc::new(AtomicU64::new(0)),
        }
    }
}

/// Phase packed into the low two bits of the published word.
pub fn pack_snap(epoch: u64, phase: Phase) -> u64 {
    let bits = match phase {
        Phase::Off => 0,
        Phase::Running => 1,
        Phase::Paused => 2,
    };
    (epoch << 2) | bits
}

pub fn unpack_snap(word: u64) -> (u64, Phase) {
    let phase = match word & 0b11 {
        1 => Phase::Running,
        2 => Phase::Paused,
        _ => Phase::Off,
    };
    (word >> 2, phase)
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

    #[allow(dead_code)]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn keyboard_interrupt(&self) -> bool {
        self.keyboard_interrupt
    }

    /// The companion reports that X11 cannot see key presses. `false` stays
    /// until the process exits. A repeated report while the gate is already
    /// paused does not bump the epoch again. An active phase pauses, and an
    /// in-flight injection is cancelled at its next chunk.
    pub fn set_keyboard_interrupt(&mut self, available: bool) {
        if available {
            return;
        }
        let fresh = self.keyboard_interrupt;
        self.keyboard_interrupt = false;
        if self.injecting {
            if fresh || self.phase != Phase::Paused {
                self.bump_epoch();
                self.phase = Phase::Paused;
                self.touch();
                self.defer(Deferred::KeyboardUnavailable);
                self.publish();
            }
            return;
        }
        if self.phase == Phase::Running {
            self.pause();
        }
    }

    /// Shared with request threads. Loading it does not take the gate lock.
    pub fn snapshot_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.published)
    }

    /// Mark an injection before the desktop call. The phase becomes `running`
    /// in the same step, unless a pause or a newer epoch already won.
    /// The sequence is the value the drive will store if this injection records.
    pub fn begin_inject(&mut self, session: Uuid, captured_epoch: u64) -> Result<u64, ()> {
        if self.epoch != captured_epoch || self.phase == Phase::Paused {
            return Err(());
        }
        self.inject_seq = self.inject_seq.saturating_add(1);
        self.injecting = true;
        if self.phase != Phase::Running {
            // One window for the first injection. A later TCP entry does not
            // move a clock the companion already recorded.
            if self.trusted_at.is_none() {
                self.trusted_at = Some(Instant::now());
            }
            self.phase = Phase::Running;
        }
        self.session = Some(session);
        self.publish();
        Ok(self.inject_seq)
    }

    /// The desktop call returned. A failure records no drive, so this sequence
    /// is settled on its own and the companion does not keep deferring.
    /// Controls posted during the call are applied in the order they arrived,
    /// and a close applies only to the session generation it named.
    pub fn end_inject(&mut self) {
        self.injecting = false;
        self.last_settled_seq = self.last_settled_seq.max(self.inject_seq);
        let deferred = std::mem::take(&mut self.deferred);
        for (_seq, action) in deferred {
            self.apply_deferred(action);
        }
        self.publish();
    }

    /// A pause covers the whole desktop. Every session injects into the same
    /// pointer, so another session must not move it until resume.
    #[allow(dead_code)]
    pub fn blocks(&self, _session: Uuid) -> bool {
        self.phase == Phase::Paused
    }

    /// The driving session went away. While an injection is in flight the
    /// phase stays put; [`Gate::end_inject`] applies the close to that session
    /// only.
    pub fn note_session_closed(&mut self, session: Uuid) {
        if self.session != Some(session) {
            return;
        }
        self.touch();
        if self.injecting {
            let generation = self.generation;
            self.defer(Deferred::Close {
                session,
                generation,
            });
            return;
        }
        self.bump_epoch();
        self.clear_session(session);
    }

    /// Called once the injection lock is free. A close that [`Gate::end_inject`]
    /// already applied is a no-op. A close queued here names the session it
    /// saw, so a newer session selected during the call is left alone.
    pub fn finish_closed_session(&mut self, session: Uuid) {
        if self.injecting {
            if self.session == Some(session) {
                self.touch();
                let generation = self.generation;
                self.defer(Deferred::Close {
                    session,
                    generation,
                });
            }
            return;
        }
        self.clear_session(session);
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
            self.publish();
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
        let epoch = self.epoch;
        let published = Arc::clone(&self.published);
        let keyboard_interrupt = self.keyboard_interrupt;
        let last_poll = self.last_poll;
        let trusted_at = self.trusted_at;
        let control_seq = self.control_seq;
        *self = Self {
            notice,
            generation,
            resume_seq,
            inject_seq,
            last_settled_seq,
            epoch,
            published,
            keyboard_interrupt,
            last_poll,
            trusted_at,
            control_seq,
            ..Self::default()
        };
        self.publish();
    }

    /// Epoch and `paused` land in one published word. A request cannot
    /// observe the new epoch with the old phase. A stop already queued for
    /// this injection stays queued: the newer control wins when the call ends.
    pub fn pause(&mut self) {
        self.bump_epoch();
        self.phase = Phase::Paused;
        self.touch();
        if self.injecting {
            self.defer(Deferred::Pause);
        }
        self.publish();
    }

    /// Cancel an in-flight injection, then let shutdown proceed. The phase
    /// stays until [`Gate::reset`] after the bounded wait.
    pub fn shutdown_cancel(&mut self) {
        self.bump_epoch();
        self.touch();
        if self.injecting {
            self.defer(Deferred::Stop);
        }
        self.publish();
    }

    /// Companion-socket poll. An already-expired gap pauses and this poll
    /// is not stored as a fresh heartbeat.
    pub fn note_poll(&mut self, now: Instant, timeout: Duration) -> bool {
        if self.phase == Phase::Running && self.gap_expired(now, timeout) {
            self.pause();
            return true;
        }
        self.last_poll = Some(now);
        self.trusted_at = Some(now);
        false
    }

    /// Watchdog tick. Does not record a poll and does not grant time.
    /// Freshness is the last trusted companion instant, not when this
    /// running stretch began.
    pub fn watchdog(&mut self, now: Instant, timeout: Duration) {
        if self.phase != Phase::Running {
            return;
        }
        if self.gap_expired(now, timeout) {
            self.pause();
        }
    }

    /// True when a companion-socket poll landed inside the watchdog window.
    pub fn poll_fresh(&self, now: Instant, timeout: Duration) -> bool {
        match self.last_poll {
            Some(at) => now.saturating_duration_since(at) < timeout,
            None => false,
        }
    }

    /// TCP mouse or type must not enter `running` once a recorded trust
    /// instant has expired. The first entry, with nothing recorded, is
    /// still allowed and arms one window.
    pub fn stale_reentry(&self, now: Instant, timeout: Duration) -> bool {
        if self.phase != Phase::Off {
            return false;
        }
        match self.trusted_at {
            Some(at) => now.saturating_duration_since(at) >= timeout,
            None => false,
        }
    }

    /// Companion-origin control. Grants the socket grace and does not
    /// require a prior poll. Unit tests use this path.
    #[cfg(test)]
    pub fn control(&mut self, session: Option<Uuid>, action: Control) -> Result<(), ()> {
        self.apply(
            session,
            action,
            true,
            Instant::now(),
            Duration::from_secs(3_600),
        )
        .map_err(|_| ())
    }

    /// `start` needs the session the companion is showing. The other actions
    /// apply to the drive already in progress. A TCP start or resume that
    /// would enter `running` needs a companion-socket poll inside `timeout`.
    pub fn apply(
        &mut self,
        session: Option<Uuid>,
        action: Control,
        from_companion: bool,
        now: Instant,
        timeout: Duration,
    ) -> Result<(), ControlReject> {
        match action {
            Control::Start => {
                let Some(id) = session else {
                    return Err(ControlReject::NeedsSession);
                };
                if self.phase != Phase::Running && !from_companion && !self.poll_fresh(now, timeout)
                {
                    return Err(ControlReject::StalePoll);
                }
                self.touch();
                // Join the deferred sequence while an injection is in flight.
                // Applying this start now would let an older stop or pause
                // replay over it. The newest control wins when the call ends.
                // This still does not wait on the injection lock.
                if self.injecting {
                    self.defer(Deferred::Start {
                        session: id,
                        from_companion,
                    });
                    return Ok(());
                }
                self.enter_running(id, from_companion);
                Ok(())
            }
            Control::Pause => {
                // A companion pause is a latch. It applies from running, off,
                // or any other phase, so a TCP stop that landed first cannot
                // leave the gate open for the next injection.
                self.pause();
                Ok(())
            }
            Control::Resume => {
                if self.phase != Phase::Paused {
                    self.touch();
                    return Ok(());
                }
                // The driving session may have been closed while paused.
                // The selected session becomes the owner. With none left,
                // the phase ends instead of running with no session.
                let Some(id) = session else {
                    self.touch();
                    self.reset();
                    return Ok(());
                };
                if !from_companion && !self.poll_fresh(now, timeout) {
                    return Err(ControlReject::StalePoll);
                }
                self.touch();
                // Same sequence as start. A pause already queued for this
                // injection must not replay over the resume.
                if self.injecting {
                    self.defer(Deferred::Resume {
                        session: id,
                        from_companion,
                    });
                    return Ok(());
                }
                self.enter_running(id, from_companion);
                Ok(())
            }
            Control::Stop => {
                self.stop();
                Ok(())
            }
        }
    }

    /// While an injection is in flight, stop only cancels it. The phase stays
    /// until [`Gate::end_inject`], so a later pause is still applied and a
    /// TCP stop cannot publish `off` underneath the call.
    pub fn stop(&mut self) {
        self.bump_epoch();
        self.touch();
        if self.injecting {
            self.defer(Deferred::Stop);
            self.publish();
            return;
        }
        self.reset();
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

    fn bump_epoch(&mut self) {
        self.epoch = self.epoch.saturating_add(1);
    }

    fn touch(&mut self) {
        self.control_seq = self.control_seq.saturating_add(1);
    }

    fn defer(&mut self, action: Deferred) {
        let seq = self.control_seq;
        self.deferred.push((seq, action));
    }

    fn apply_deferred(&mut self, action: Deferred) {
        match action {
            Deferred::Stop => {
                self.reset();
            }
            Deferred::Pause => {
                self.phase = Phase::Paused;
                self.publish();
            }
            Deferred::Close {
                session,
                generation,
            } => {
                if self.session == Some(session) && self.generation == generation {
                    self.clear_session(session);
                }
            }
            Deferred::KeyboardUnavailable => {
                self.keyboard_interrupt = false;
                self.phase = Phase::Paused;
                self.publish();
            }
            Deferred::Start {
                session,
                from_companion,
            }
            | Deferred::Resume {
                session,
                from_companion,
            } => {
                // A missing keyboard stays paused. Replaying a start or
                // resume must not lift that latch.
                if !self.keyboard_interrupt {
                    return;
                }
                self.enter_running(session, from_companion);
            }
        }
    }

    fn enter_running(&mut self, session: Uuid, grant_grace: bool) {
        let fresh = self.phase != Phase::Running;
        self.session = Some(session);
        // A sample from the previous drive would look like a user move
        // to the pointer fallback the moment this stretch is running.
        // The pointer is already away from a pre-pause sample. Leave the
        // last point so a later type still has somewhere to land, but do
        // not let the fallback compare against that target.
        self.drive = None;
        // One grace window when the companion socket itself enters running.
        // A start that is already running, and every TCP start, leave the
        // last trusted instant alone.
        if fresh && grant_grace {
            self.trusted_at = Some(Instant::now());
        }
        self.phase = Phase::Running;
        self.bump_resume();
        self.publish();
    }

    /// The last trusted companion instant. No instant means the window has
    /// already passed: a TCP start must not invent one.
    fn gap_expired(&self, now: Instant, timeout: Duration) -> bool {
        match self.trusted_at {
            Some(at) => now.saturating_duration_since(at) >= timeout,
            None => true,
        }
    }

    fn publish(&self) {
        self.published
            .store(pack_snap(self.epoch, self.phase), Ordering::SeqCst);
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
        if self.phase != Phase::Running && self.trusted_at.is_none() {
            self.trusted_at = Some(Instant::now());
        }
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
        self.publish();
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

/// Non-zero nonce for this visor process. A restart picks a new value, so
/// the companion can tell a fresh `resume_seq` of 0 from the process it paused.
pub fn fresh_instance() -> u64 {
    let mut bytes = [0u8; 8];
    let drawn = std::fs::File::open("/dev/urandom").and_then(|mut file| {
        use std::io::Read;
        file.read_exact(&mut bytes)?;
        Ok(u64::from_ne_bytes(bytes))
    });
    match drawn {
        Ok(value) if value != 0 => value,
        _ => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_nanos()).unwrap_or(1))
            .unwrap_or(1)
            .max(1),
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
    fn a_pause_after_stop_latches_and_start_does_not_run_until_it_is_lifted() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        let generation = gate.generation();
        gate.record_mouse(
            id,
            &MouseOp::Move {
                x: 3,
                y: 4,
                clickable: false,
            },
            Some((0, 0)),
            generation,
            10,
        );
        assert_eq!(gate.phase(), Phase::Running);
        gate.control(None, Control::Stop).unwrap();
        assert_eq!(gate.phase(), Phase::Off);
        gate.control(Some(id), Control::Pause).unwrap();
        assert_eq!(gate.phase(), Phase::Paused);
        assert!(gate.blocks(id));
        let paused_generation = gate.generation();
        gate.record_mouse(
            id,
            &MouseOp::Move {
                x: 9,
                y: 9,
                clickable: false,
            },
            Some((3, 4)),
            paused_generation,
            20,
        );
        assert!(gate.drive().is_none());
        assert_eq!(gate.phase(), Phase::Paused);
        // The gate action itself would start. TCP is refused before this runs
        // while the latch is set. A start that raced in before the pause
        // still loses when the pause arrives.
        gate.control(None, Control::Stop).unwrap();
        gate.control(Some(id), Control::Start).unwrap();
        assert_eq!(gate.phase(), Phase::Running);
        gate.control(Some(id), Control::Pause).unwrap();
        assert_eq!(gate.phase(), Phase::Paused);
        assert!(gate.blocks(id));
    }

    #[test]
    fn resume_after_the_driving_session_closed_takes_the_selected_session() {
        let mut gate = Gate::default();
        let driving = Uuid::new_v4();
        let other = Uuid::new_v4();
        gate.record_mouse(
            driving,
            &MouseOp::Move {
                x: 1,
                y: 1,
                clickable: false,
            },
            Some((0, 0)),
            gate.generation(),
            10,
        );
        gate.control(Some(driving), Control::Pause).unwrap();
        gate.clear_session(driving);
        assert_eq!(gate.phase(), Phase::Paused);
        assert!(gate.session().is_none());
        gate.control(Some(other), Control::Resume).unwrap();
        assert_eq!(gate.phase(), Phase::Running);
        assert_eq!(gate.session(), Some(other));
        assert!(gate.drive().is_none());

        gate.control(Some(other), Control::Pause).unwrap();
        gate.clear_session(other);
        gate.control(None, Control::Resume).unwrap();
        assert_eq!(gate.phase(), Phase::Off);
        assert!(gate.session().is_none());
    }

    #[test]
    fn an_injection_keeps_its_sequence_after_the_flag_drops() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        assert_eq!(gate.begin_inject(id, gate.epoch()).unwrap(), 1);
        assert!(gate.injecting());
        assert_eq!(gate.phase(), Phase::Running);
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
        gate.begin_inject(id, gate.epoch()).unwrap();
        gate.end_inject();
        assert_eq!(gate.last_settled_seq(), 2);
        assert!(gate.drive().unwrap().inject_seq < gate.last_settled_seq());
        gate.begin_inject(id, gate.epoch()).unwrap();
        gate.reset();
        assert!(!gate.injecting());
        assert_eq!(gate.last_settled_seq(), 2);
        assert_eq!(
            gate.inject_seq(),
            3,
            "reset must not reuse an in-flight sequence"
        );
    }

    #[test]
    fn begin_inject_publishes_running_before_any_drive() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        let handle = gate.snapshot_handle();
        let epoch = gate.epoch();
        assert_eq!(gate.begin_inject(id, epoch).unwrap(), 1);
        let (seen, phase) = unpack_snap(handle.load(Ordering::SeqCst));
        assert_eq!(phase, Phase::Running);
        assert_eq!(seen, epoch);
        assert!(gate.injecting());
        assert!(gate.drive().is_none());
        let stale = gate.epoch();
        gate.pause();
        assert!(gate.begin_inject(id, stale).is_err());
        assert_eq!(gate.phase(), Phase::Paused);
    }

    #[test]
    fn pause_publishes_the_epoch_and_paused_together() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        gate.control(Some(id), Control::Start).unwrap();
        let handle = gate.snapshot_handle();
        let before = unpack_snap(handle.load(Ordering::SeqCst)).0;
        gate.pause();
        let word = handle.load(Ordering::SeqCst);
        let (epoch, phase) = unpack_snap(word);
        assert_eq!(epoch, before + 1);
        assert_eq!(phase, Phase::Paused);
        assert_eq!(word, pack_snap(epoch, Phase::Paused));
        assert_eq!(gate.phase(), Phase::Paused);
    }

    #[test]
    fn a_stop_during_injection_waits_for_the_call_to_finish() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        let epoch = gate.epoch();
        gate.begin_inject(id, epoch).unwrap();
        gate.control(None, Control::Stop).unwrap();
        assert_eq!(gate.phase(), Phase::Running);
        assert!(gate.injecting());
        assert_ne!(gate.epoch(), epoch);
        assert!(gate.begin_inject(id, epoch).is_err());
        gate.end_inject();
        assert_eq!(gate.phase(), Phase::Off);
        assert!(!gate.injecting());
        assert!(gate.drive().is_none());
    }

    #[test]
    fn a_pause_during_injection_keeps_the_latch_over_a_pending_stop() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        gate.begin_inject(id, gate.epoch()).unwrap();
        gate.stop();
        gate.pause();
        gate.end_inject();
        assert_eq!(gate.phase(), Phase::Paused);
        assert!(gate.blocks(id));
    }

    #[test]
    fn a_late_poll_pauses_before_it_is_recorded() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        gate.control(Some(id), Control::Start).unwrap();
        let timeout = Duration::from_secs(2);
        let late = Instant::now() + Duration::from_secs(10);
        assert!(gate.note_poll(late, timeout));
        assert_eq!(gate.phase(), Phase::Paused);
        gate.control(Some(id), Control::Resume).unwrap();
        gate.watchdog(late, timeout);
        assert_eq!(
            gate.phase(),
            Phase::Paused,
            "the expired poll must not become the heartbeat of the new stretch"
        );
    }

    #[test]
    fn a_new_running_stretch_resets_the_watchdog_clock() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        gate.control(Some(id), Control::Start).unwrap();
        std::thread::sleep(Duration::from_millis(80));
        gate.control(None, Control::Stop).unwrap();
        gate.control(Some(id), Control::Start).unwrap();
        gate.watchdog(
            Instant::now() + Duration::from_millis(50),
            Duration::from_millis(100),
        );
        assert_eq!(
            gate.phase(),
            Phase::Running,
            "a stop then start must not inherit the previous stretch"
        );
    }

    #[test]
    fn repeated_starts_while_running_do_not_extend_the_watchdog() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        let timeout = Duration::from_millis(100);
        gate.control(Some(id), Control::Start).unwrap();
        let opened = Instant::now();
        std::thread::sleep(Duration::from_millis(60));
        gate.control(Some(id), Control::Start).unwrap();
        gate.control(Some(id), Control::Start).unwrap();
        assert_eq!(gate.resume_seq(), 3);
        assert!(gate.drive().is_none());
        gate.watchdog(opened + timeout, timeout);
        assert_eq!(
            gate.phase(),
            Phase::Paused,
            "a TCP start while running must not move the quiet-poll clock"
        );
    }

    #[test]
    fn a_companion_poll_extends_the_deadline_and_a_later_start_does_not() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        let timeout = Duration::from_millis(100);
        gate.control(Some(id), Control::Start).unwrap();
        std::thread::sleep(Duration::from_millis(40));
        let polled = Instant::now();
        assert!(!gate.note_poll(polled, timeout));
        std::thread::sleep(Duration::from_millis(40));
        gate.control(Some(id), Control::Start).unwrap();
        gate.watchdog(polled + Duration::from_millis(90), timeout);
        assert_eq!(gate.phase(), Phase::Running);
        gate.watchdog(polled + timeout, timeout);
        assert_eq!(
            gate.phase(),
            Phase::Paused,
            "only the companion poll extends the deadline"
        );
    }

    #[test]
    fn a_fresh_transition_gets_one_grace_window() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        let timeout = Duration::from_millis(100);
        assert!(!gate.note_poll(Instant::now(), timeout));
        std::thread::sleep(Duration::from_millis(80));
        gate.control(Some(id), Control::Start).unwrap();
        let opened = Instant::now();
        gate.watchdog(opened + Duration::from_millis(50), timeout);
        assert_eq!(
            gate.phase(),
            Phase::Running,
            "an old poll must not erase the grace window"
        );
        gate.watchdog(opened + timeout, timeout);
        assert_eq!(gate.phase(), Phase::Paused);
    }

    #[test]
    fn closing_the_session_during_injection_does_not_reset_until_the_call_ends() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        gate.begin_inject(id, gate.epoch()).unwrap();
        gate.note_session_closed(id);
        assert_eq!(gate.phase(), Phase::Running);
        assert!(gate.injecting());
        gate.end_inject();
        assert_eq!(gate.phase(), Phase::Off);
        assert!(!gate.injecting());
    }

    #[test]
    fn a_stop_after_an_inflight_pause_ends_off() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        gate.begin_inject(id, gate.epoch()).unwrap();
        gate.pause();
        gate.stop();
        assert_eq!(gate.phase(), Phase::Paused);
        gate.end_inject();
        assert_eq!(gate.phase(), Phase::Off);
        assert!(!gate.injecting());
        assert!(gate.drive().is_none());
    }

    #[test]
    fn a_close_during_injection_does_not_clear_the_session_selected_next() {
        let mut gate = Gate::default();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        gate.begin_inject(first, gate.epoch()).unwrap();
        gate.note_session_closed(first);
        gate.control(Some(second), Control::Start).unwrap();
        // The start is in the deferred sequence with the close. The session
        // stays the one this injection captured until the call unwinds.
        assert_eq!(gate.session(), Some(first));
        assert_eq!(gate.phase(), Phase::Running);
        gate.end_inject();
        assert_eq!(gate.phase(), Phase::Running);
        assert_eq!(gate.session(), Some(second));
        assert!(!gate.injecting());
    }

    #[test]
    fn a_start_during_injection_beats_an_older_stop() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        let epoch = gate.epoch();
        gate.begin_inject(id, epoch).unwrap();
        gate.control(None, Control::Stop).unwrap();
        assert_eq!(gate.phase(), Phase::Running);
        assert_ne!(gate.epoch(), epoch);
        gate.control(Some(id), Control::Start).unwrap();
        assert_eq!(
            gate.phase(),
            Phase::Running,
            "a deferred start waits for the call to finish"
        );
        let seq_before = gate.resume_seq();
        gate.end_inject();
        assert_eq!(gate.phase(), Phase::Running);
        assert_eq!(gate.session(), Some(id));
        assert!(!gate.injecting());
        assert!(gate.drive().is_none());
        assert_eq!(gate.resume_seq(), seq_before + 1);
    }

    #[test]
    fn a_resume_during_injection_beats_an_older_pause() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        gate.begin_inject(id, gate.epoch()).unwrap();
        gate.pause();
        assert_eq!(gate.phase(), Phase::Paused);
        gate.control(Some(id), Control::Resume).unwrap();
        assert_eq!(
            gate.phase(),
            Phase::Paused,
            "the latch stays until the call ends, so the injection cannot continue"
        );
        gate.end_inject();
        assert_eq!(gate.phase(), Phase::Running);
        assert_eq!(gate.session(), Some(id));
        assert!(!gate.injecting());
        assert!(gate.drive().is_none());
    }

    #[test]
    fn a_later_stop_still_beats_a_deferred_start() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        gate.begin_inject(id, gate.epoch()).unwrap();
        gate.control(Some(id), Control::Start).unwrap();
        gate.control(None, Control::Stop).unwrap();
        gate.end_inject();
        assert_eq!(gate.phase(), Phase::Off);
        assert!(gate.session().is_none());
        assert!(!gate.injecting());
    }

    #[test]
    fn a_missing_keyboard_beats_a_deferred_start() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        gate.begin_inject(id, gate.epoch()).unwrap();
        gate.control(None, Control::Stop).unwrap();
        gate.control(Some(id), Control::Start).unwrap();
        gate.set_keyboard_interrupt(false);
        assert!(!gate.keyboard_interrupt());
        gate.end_inject();
        assert_eq!(gate.phase(), Phase::Paused);
        assert!(!gate.keyboard_interrupt());
        assert!(gate.begin_inject(id, gate.epoch()).is_err());
    }

    #[test]
    fn a_tcp_stop_start_never_refreshes_the_watchdog() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        let timeout = Duration::from_millis(100);
        let opened = Instant::now();
        assert_eq!(
            gate.apply(Some(id), Control::Start, false, opened, timeout),
            Err(ControlReject::StalePoll)
        );
        gate.begin_inject(id, gate.epoch()).unwrap();
        gate.end_inject();
        assert_eq!(gate.phase(), Phase::Running);
        gate.apply(None, Control::Stop, false, opened, timeout)
            .unwrap();
        assert_eq!(gate.phase(), Phase::Off);
        assert_eq!(
            gate.apply(
                Some(id),
                Control::Start,
                false,
                opened + Duration::from_millis(20),
                timeout
            ),
            Err(ControlReject::StalePoll)
        );
        let late = opened + timeout + Duration::from_millis(1);
        assert!(gate.stale_reentry(late, timeout));
        assert_eq!(
            gate.apply(Some(id), Control::Start, false, late, timeout),
            Err(ControlReject::StalePoll)
        );
        assert_eq!(gate.phase(), Phase::Off);
    }

    #[test]
    fn a_companion_poll_allows_tcp_start_without_moving_the_deadline() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        let timeout = Duration::from_millis(100);
        let polled = Instant::now();
        assert!(!gate.note_poll(polled, timeout));
        gate.apply(Some(id), Control::Start, false, polled, timeout)
            .unwrap();
        assert_eq!(gate.phase(), Phase::Running);
        gate.apply(None, Control::Stop, false, polled, timeout)
            .unwrap();
        gate.apply(
            Some(id),
            Control::Start,
            false,
            polled + Duration::from_millis(40),
            timeout,
        )
        .unwrap();
        gate.watchdog(polled + Duration::from_millis(90), timeout);
        assert_eq!(gate.phase(), Phase::Running);
        gate.watchdog(polled + timeout, timeout);
        assert_eq!(
            gate.phase(),
            Phase::Paused,
            "a TCP stop and start must keep the poll that allowed the start"
        );
    }

    #[test]
    fn a_missing_keyboard_pauses_a_live_injection() {
        let mut gate = Gate::default();
        let id = Uuid::new_v4();
        let epoch = gate.epoch();
        gate.begin_inject(id, epoch).unwrap();
        gate.set_keyboard_interrupt(false);
        assert!(!gate.keyboard_interrupt());
        assert_eq!(gate.phase(), Phase::Paused);
        assert_ne!(gate.epoch(), epoch);
        gate.set_keyboard_interrupt(false);
        assert_eq!(gate.epoch(), epoch + 1, "a repeat must not bump again");
        gate.end_inject();
        assert_eq!(gate.phase(), Phase::Paused);
        assert!(gate.begin_inject(id, gate.epoch()).is_err());
    }
}
