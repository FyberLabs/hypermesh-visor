//! Pause and resume while the visor is driving the desktop.
//!
//! The clock is injected so the 2 minute wait can be tested without sleeping.
//! Synthetic pointer and key events are filtered before they reach [`Machine`].
//!
//! There is no separate automation runner. The visor enters `running` when
//! `mouse` or `type` is called, or when a caller posts `start`. `stop` is the
//! seam that ends it without closing the session. `reach` is shown only when
//! that drive sample says the target is clickable. Nothing here invents that.

use serde_json::Value;

/// Default wait with no user input before a paused automation continues.
pub const DEFAULT_RESUME_MS: u64 = 120_000;

/// A move at least this long is fly.
pub const FLY_DISTANCE_PX: u32 = 256;
/// A move at least this fast is fly.
pub const FLY_SPEED_PX_PER_SEC: u32 = 1_200;
pub const MOVE_FRESH_MS: u64 = 200;
pub const CLICK_FRESH_MS: u64 = 200;
pub const TYPE_FRESH_MS: u64 = 400;
pub const CURSOR_FRAME_MS: u64 = 100;
/// Pointer-fallback: ignore a drive sample until it is older than this.
pub const FALLBACK_SETTLE_MS: u64 = 150;
/// Pointer-fallback: a root pointer farther than this from the drive target
/// is a user move. Manhattan, matching the window check.
pub const FALLBACK_DISTANCE_PX: i32 = 12;
/// QueryPointer button bits: Button1 at bit 8 through Button5 at bit 12.
const FALLBACK_BUTTONS: u32 = 0b1_1111 << 8;
/// How long a published injection may suppress fallback checks. A hung
/// `mouse` or `type` must not keep a physical move from pausing.
pub const INJECT_HOLD_MS: u64 = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Off,
    Running,
    Paused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisorPhase {
    Off,
    Running,
    Paused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    Pause,
    Resume,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    phase: Phase,
    last_input_ms: u64,
    resume_after_ms: u64,
    asking: bool,
    /// A local resume is waiting for the visor to leave `paused`. A poll that
    /// still says paused must not undo it.
    holding_resume: bool,
    /// Resume sequence on the latest visor snapshot.
    last_resume_seq: u64,
    /// Resume sequence when this pause began. A later `running` snapshot
    /// follows only when its sequence is newer, so a poll that skips the
    /// `paused` state still counts, and a poll from before the pause does not.
    pause_mark: u64,
}

impl Machine {
    pub fn new(resume_after_ms: u64) -> Self {
        Self {
            phase: Phase::Off,
            last_input_ms: 0,
            resume_after_ms: resume_after_ms.max(1),
            asking: false,
            holding_resume: false,
            last_resume_seq: 0,
            pause_mark: 0,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn asking(&self) -> bool {
        self.asking
    }

    pub fn resume_after_ms(&self) -> u64 {
        self.resume_after_ms
    }

    /// What the visor last reported. A local pause follows a `running` report
    /// only when that report's resume sequence is newer than the sequence at
    /// the pause. The companion does not have to see the `paused` poll in
    /// between. The same sequence is the report from before the pause.
    pub fn see_visor(&mut self, visor: VisorPhase, now_ms: u64, resume_seq: u64) -> Effect {
        self.last_resume_seq = resume_seq;
        match visor {
            VisorPhase::Off => {
                self.phase = Phase::Off;
                self.asking = false;
                self.holding_resume = false;
                Effect::None
            }
            VisorPhase::Running => {
                self.holding_resume = false;
                let follow = self.phase == Phase::Off
                    || (self.phase == Phase::Paused && resume_seq > self.pause_mark);
                if follow {
                    self.phase = Phase::Running;
                    self.asking = false;
                }
                Effect::None
            }
            VisorPhase::Paused => {
                // A local resume is in flight. A paused snapshot from that
                // sequence, or an older one, has not seen the resume. A newer
                // sequence was paused again after the resume landed.
                let stale = self.holding_resume && resume_seq <= self.pause_mark;
                if stale {
                    Effect::None
                } else {
                    let entered = self.phase != Phase::Paused;
                    if entered {
                        self.phase = Phase::Paused;
                        self.last_input_ms = now_ms;
                        self.asking = false;
                    }
                    self.holding_resume = false;
                    if entered || resume_seq > self.pause_mark {
                        self.pause_mark = resume_seq;
                    }
                    Effect::None
                }
            }
        }
    }

    /// Real mouse movement, button press, or key press. Synthetic input must
    /// not be passed in.
    pub fn user_input(&mut self, now_ms: u64) -> Effect {
        match self.phase {
            Phase::Running => {
                self.phase = Phase::Paused;
                self.last_input_ms = now_ms;
                self.asking = false;
                self.holding_resume = false;
                self.pause_mark = self.last_resume_seq;
                Effect::Pause
            }
            Phase::Paused => {
                self.last_input_ms = now_ms;
                Effect::None
            }
            Phase::Off => Effect::None,
        }
    }

    /// Click on the bug while already paused. The click that caused the pause
    /// must not call this.
    pub fn ask(&mut self, now_ms: u64) {
        if self.phase == Phase::Paused {
            self.asking = true;
            self.last_input_ms = now_ms;
        }
    }

    pub fn continue_now(&mut self, _now_ms: u64) -> Effect {
        if self.phase == Phase::Paused && self.asking {
            self.phase = Phase::Running;
            self.asking = false;
            self.holding_resume = true;
            return Effect::Resume;
        }
        Effect::None
    }

    pub fn dismiss(&mut self, now_ms: u64) {
        if self.asking {
            self.asking = false;
            self.last_input_ms = now_ms;
        }
    }

    /// The visor could not be polled. A local pause stays paused so the
    /// unsent pause can be posted again when the visor returns.
    pub fn note_disconnect(&mut self) {
        if self.phase == Phase::Paused {
            return;
        }
        self.phase = Phase::Off;
        self.asking = false;
        self.holding_resume = false;
    }

    /// Resume when the wait has elapsed with no user input.
    pub fn poll(&mut self, now_ms: u64) -> Effect {
        if self.phase != Phase::Paused {
            return Effect::None;
        }
        if now_ms.saturating_sub(self.last_input_ms) >= self.resume_after_ms {
            self.phase = Phase::Running;
            self.asking = false;
            self.holding_resume = true;
            return Effect::Resume;
        }
        Effect::None
    }
}

/// Sequence a drive must be newer than before the pointer fallback trusts it.
///
/// `prior` is the resume sequence from before the snapshot in hand. `seen` is
/// the sequence on that snapshot. A server resume snapshot already carries the
/// bumped sequence, and so does a drive recorded between the resume and this
/// poll. The cutoff stays at the older of the two so that drive still counts.
pub fn resume_cutoff(prior: u64, seen: u64) -> u64 {
    prior.min(seen)
}

/// A drive counts when no resume has set a cutoff, or it was recorded on a
/// later resume sequence than that cutoff.
pub fn drive_counts(drive_resume_seq: u64, after_resume: Option<u64>) -> bool {
    match after_resume {
        None => true,
        Some(seq) => drive_resume_seq > seq,
    }
}

/// The pointer-fallback detector. A sample from the resume sequence at or
/// before the cutoff is the drive that was on screen when the automation
/// paused; comparing the pointer with it pauses again on the next frame.
pub fn fallback_displaced(
    pointer_known: bool,
    drive_resume_seq: u64,
    after_resume: Option<u64>,
    age_ms: u64,
    distance: i32,
) -> bool {
    pointer_known
        && drive_counts(drive_resume_seq, after_resume)
        && age_ms > FALLBACK_SETTLE_MS
        && distance > FALLBACK_DISTANCE_PX
}

/// Newly pressed buttons in a `QueryPointer` mask (bits 8 through 12).
pub fn newly_pressed(previous: u32, mask: u32) -> u32 {
    (mask & !previous) & FALLBACK_BUTTONS
}

/// Later polls of the same drive sample required before a pointer mismatch
/// counts as the user. The poll already in flight can still be the previous
/// drive after the automation's pointer has moved.
pub const FALLBACK_CONFIRM_EPOCHS: u8 = 2;

/// The drive sample a mismatch was first seen against, and how many newer
/// polls have shown that same sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FallbackHold {
    pub at_ms: u64,
    pub last_epoch: u64,
    pub later_epochs: u8,
}

/// Displacement against the snapshot in hand is not enough. Pause only once
/// later polls still carry that same drive and the pointer is still off it.
/// A new `at_ms` starts the wait over, which is what a just-recorded move does.
pub fn consider_displacement(
    displaced: bool,
    drive_at_ms: u64,
    epoch: u64,
    hold: Option<FallbackHold>,
) -> (bool, Option<FallbackHold>) {
    if !displaced {
        return (false, None);
    }
    let mut hold = match hold {
        Some(hold) if hold.at_ms == drive_at_ms => hold,
        _ => FallbackHold {
            at_ms: drive_at_ms,
            last_epoch: epoch,
            later_epochs: 0,
        },
    };
    if epoch > hold.last_epoch {
        hold.later_epochs = hold.later_epochs.saturating_add(1);
        hold.last_epoch = epoch;
    }
    let pause = hold.later_epochs >= FALLBACK_CONFIRM_EPOCHS;
    (pause, Some(hold))
}

/// A newly pressed button is a physical press.
///
/// The visor publishes a click or drag only after the press and the release
/// have both finished, so a button that is down once that sample is visible
/// is not the automation's button.
pub fn fallback_button_press(previous: u32, mask: u32) -> bool {
    newly_pressed(previous, mask) != 0
}

/// While paused, the pointer fallback has no fresh drive target. Movement
/// or a button change from the sample taken at pause time is still the user.
pub fn fallback_paused_activity(
    base_x: i32,
    base_y: i32,
    base_mask: u32,
    x: i32,
    y: i32,
    mask: u32,
) -> bool {
    let distance = (x - base_x).saturating_abs() + (y - base_y).saturating_abs();
    distance > FALLBACK_DISTANCE_PX || (mask ^ base_mask) & FALLBACK_BUTTONS != 0
}

/// One drive sample the pointer fallback can see. `at_ms` is the visor's
/// clock. The companion's own clock is [`FallbackInput::now_ms`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FallbackDrive {
    pub x: i32,
    pub y: i32,
    pub at_ms: u64,
    pub pointer_known: bool,
    pub resume_seq: u64,
    pub inject_seq: u64,
}

/// One pointer sample plus the snapshot fields the fallback needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FallbackInput {
    pub phase: Phase,
    pub x: i32,
    pub y: i32,
    pub mask: u32,
    /// Milliseconds since the companion started. Not the drive's unix time.
    pub now_ms: u64,
    /// Wall clock used only to age a drive sample.
    pub wall_ms: u64,
    pub epoch: u64,
    pub injecting: bool,
    pub inject_seq: u64,
    pub drive: Option<FallbackDrive>,
    pub after_resume: Option<u64>,
}

/// Baseline, button mask, and injection hold for the pointer fallback.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FallbackTrack {
    baseline: Option<(i32, i32, u32)>,
    button_mask: u32,
    displacement_hold: Option<FallbackHold>,
    baseline_ready: bool,
    defer_seq: Option<u64>,
    defer_since_ms: u64,
    /// Highest injection sequence that has matched a drive or timed out.
    /// A later `start` that drops the drive must not defer this sequence again.
    settled_inject: u64,
}

/// Pointer-fallback cases.
///
/// | Case | Result |
/// | --- | --- |
/// | Start, or resume, with no usable drive | The first sample is the baseline. A later move over 12 px or a button change pauses. |
/// | Type sample with `pointer_known` false | The same baseline. The type point is not a target. |
/// | Click or drag in flight | Buttons and displacement wait while `injecting` is set, or until `drive.inject_seq` matches, for at most [`INJECT_HOLD_MS`]. The pointer is adopted and does not pause. |
/// | Click or drag after that drive is visible | Displacement against the drive, confirmed on two later polls. A button pauses only when newly pressed. |
/// | Injection still unmarked after 2 s | Classification resumes. The position already adopted is not an immediate pause. |
/// | TCP `start` clears the drive | The baseline still sees a later physical move. |
/// | Re-pause | [`Machine::see_visor`] accepts a paused snapshot whose `resume_seq` is newer than the local pause. |
/// | Same or older paused snapshot during a local resume | Ignored. |
/// | Backend becomes Wayland | The window disables raw events before it drains them, then this fallback runs. |
/// | Backend becomes X11 | Raw events are enabled before the drain, and this track is cleared. |
/// | Reconnect | The same `resume_seq` does not undo a local pause. A newer running snapshot follows. |
/// | Missed poll | The epoch does not bump, so a displacement does not confirm. |
///
/// A drive is usable when its pointer is known and [`drive_counts`]. While an
/// injection is outstanding, the sample is copied into the baseline and no
/// pause is reported, so the visor's own move does not trip the baseline.
pub fn classify_fallback(input: &FallbackInput, track: &mut FallbackTrack) -> bool {
    if input.phase == Phase::Off {
        *track = FallbackTrack::default();
        return false;
    }
    if injection_deferred(input, track) {
        track.baseline = Some((input.x, input.y, input.mask));
        track.button_mask = input.mask;
        track.displacement_hold = None;
        track.baseline_ready = true;
        return false;
    }
    if input.phase == Phase::Paused {
        track.displacement_hold = None;
        return baseline_moved(input, track);
    }
    let usable = input.drive.as_ref().is_some_and(|drive| {
        drive.pointer_known && drive_counts(drive.resume_seq, input.after_resume)
    });
    let Some(drive) = input.drive.filter(|_| usable) else {
        track.displacement_hold = None;
        return baseline_moved(input, track);
    };
    let age = input.wall_ms.saturating_sub(drive.at_ms);
    let distance = pointer_distance(input.x, input.y, drive.x, drive.y);
    let displaced = fallback_displaced(true, drive.resume_seq, input.after_resume, age, distance);
    let (pause_move, hold) =
        consider_displacement(displaced, drive.at_ms, input.epoch, track.displacement_hold);
    track.displacement_hold = hold;
    let pressed = fallback_button_press(track.button_mask, input.mask);
    track.button_mask = input.mask;
    track.baseline = Some((input.x, input.y, input.mask));
    track.baseline_ready = true;
    pause_move || pressed
}

fn pointer_distance(x0: i32, y0: i32, x1: i32, y1: i32) -> i32 {
    let span = x0.abs_diff(x1).saturating_add(y0.abs_diff(y1));
    span.min(i32::MAX as u32) as i32
}

fn baseline_moved(input: &FallbackInput, track: &mut FallbackTrack) -> bool {
    let Some((x, y, mask)) = track.baseline.filter(|_| track.baseline_ready) else {
        track.baseline = Some((input.x, input.y, input.mask));
        track.button_mask = input.mask;
        track.baseline_ready = true;
        return false;
    };
    let moved = fallback_paused_activity(x, y, mask, input.x, input.y, input.mask);
    if moved {
        track.baseline = Some((input.x, input.y, input.mask));
    }
    track.button_mask = input.mask;
    moved
}

/// An injection is outstanding when the visor says one is running, or its
/// sequence has not appeared on a drive yet. Sequence 0 means none has started.
fn injection_outstanding(input: &FallbackInput) -> Option<u64> {
    if input.injecting {
        return Some(input.inject_seq);
    }
    if input.inject_seq == 0 {
        return None;
    }
    let shown = input
        .drive
        .as_ref()
        .is_some_and(|drive| drive.inject_seq == input.inject_seq);
    if shown {
        None
    } else {
        Some(input.inject_seq)
    }
}

fn injection_deferred(input: &FallbackInput, track: &mut FallbackTrack) -> bool {
    let Some(seq) = injection_outstanding(input) else {
        // The drive already shows this sequence, including when the poll
        // missed the `injecting` flag. A later start that drops the drive
        // must not treat the same sequence as in flight again.
        if input.inject_seq > 0 {
            track.settled_inject = track.settled_inject.max(input.inject_seq);
        }
        track.defer_seq = None;
        return false;
    };
    if seq <= track.settled_inject {
        return false;
    }
    if track.defer_seq != Some(seq) {
        track.defer_seq = Some(seq);
        track.defer_since_ms = input.now_ms;
    }
    if input.now_ms.saturating_sub(track.defer_since_ms) >= INJECT_HOLD_MS {
        track.settled_inject = track.settled_inject.max(seq);
        track.defer_seq = None;
        return false;
    }
    true
}

/// XTEST device ids are synthetic. A real device is anything else.
pub fn is_synthetic(device_id: u16, source_id: u16, xtest: &[u16]) -> bool {
    !xtest.is_empty() && (xtest.contains(&device_id) || xtest.contains(&source_id))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorPose {
    System,
    Pointer,
    Crawl,
    Fly,
    Reach,
    Click,
    Type,
    Busy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionKind {
    None,
    Move,
    Click,
    Drag,
    Type,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Motion {
    pub kind: MotionKind,
    pub age_ms: u64,
    pub distance_px: u32,
    pub elapsed_ms: u32,
    pub clickable: bool,
}

pub fn cursor_pose(phase: Phase, opt_in: bool, motion: Option<Motion>) -> CursorPose {
    if phase == Phase::Paused || (phase == Phase::Off && !opt_in) {
        return CursorPose::System;
    }
    let Some(motion) = motion else {
        return if phase == Phase::Running {
            CursorPose::Busy
        } else {
            CursorPose::Pointer
        };
    };
    if motion.kind == MotionKind::Click && motion.age_ms < CLICK_FRESH_MS {
        return CursorPose::Click;
    }
    if motion.kind == MotionKind::Type && motion.age_ms < TYPE_FRESH_MS {
        return CursorPose::Type;
    }
    if matches!(motion.kind, MotionKind::Move | MotionKind::Drag) && motion.age_ms < MOVE_FRESH_MS {
        return if flies(&motion) {
            CursorPose::Fly
        } else {
            CursorPose::Crawl
        };
    }
    if motion.clickable && matches!(motion.kind, MotionKind::Move | MotionKind::Drag) {
        return CursorPose::Reach;
    }
    if phase == Phase::Running {
        CursorPose::Busy
    } else {
        CursorPose::Pointer
    }
}

fn flies(motion: &Motion) -> bool {
    if motion.distance_px >= FLY_DISTANCE_PX {
        return true;
    }
    if motion.elapsed_ms == 0 {
        return false;
    }
    let speed = u64::from(motion.distance_px) * 1000 / u64::from(motion.elapsed_ms);
    speed >= u64::from(FLY_SPEED_PX_PER_SEC)
}

pub fn cursor_frame(pose: CursorPose, age_ms: u64) -> usize {
    let step = CURSOR_FRAME_MS.max(1);
    match pose {
        CursorPose::Click => ((age_ms / step) as usize).min(1),
        CursorPose::Type => (age_ms / step) as usize % 2,
        CursorPose::Busy => (age_ms / step) as usize % 4,
        _ => 0,
    }
}

pub fn cursor_frame_name(pose: CursorPose, frame: usize) -> &'static str {
    match pose {
        CursorPose::System | CursorPose::Pointer => "pointer",
        CursorPose::Crawl => "crawl",
        CursorPose::Fly => "fly",
        CursorPose::Reach => "reach",
        CursorPose::Click => ["click1", "click2"][frame.min(1)],
        CursorPose::Type => ["type1", "type2"][frame.min(1)],
        CursorPose::Busy => ["busy1", "busy2", "busy3", "busy4"][frame % 4],
    }
}

/// Antenna tip. Halves round to even, matching hotspot.json.
pub fn hotspot_for(size: u32) -> (u16, u16) {
    let x = round_half_even(41.0 * f64::from(size) / 128.0);
    let y = round_half_even(20.0 * f64::from(size) / 128.0);
    (x.max(0) as u16, y.max(0) as u16)
}

fn round_half_even(value: f64) -> i32 {
    let floor = value.floor();
    let diff = value - floor;
    if (diff - 0.5).abs() < 1e-9 {
        let even = (floor as i64) % 2 == 0;
        if even {
            floor as i32
        } else {
            floor as i32 + 1
        }
    } else {
        value.round() as i32
    }
}

/// Consecutive failed visor polls before the companion shows disconnected.
pub const DISCONNECT_MISSES: u32 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link<T> {
    misses: u32,
    last: T,
}

impl<T: Clone> Link<T> {
    pub fn new(initial: T) -> Self {
        Self {
            misses: DISCONNECT_MISSES,
            last: initial,
        }
    }

    pub fn current(&self) -> &T {
        &self.last
    }

    pub fn disconnected(&self) -> bool {
        self.misses >= DISCONNECT_MISSES
    }

    pub fn ok(&mut self, value: T) {
        self.misses = 0;
        self.last = value;
    }

    pub fn miss(&mut self, down: T) {
        self.misses = self.misses.saturating_add(1);
        if self.misses >= DISCONNECT_MISSES {
            self.last = down;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn motion(kind: MotionKind, age: u64, distance: u32, elapsed: u32, clickable: bool) -> Motion {
        Motion {
            kind,
            age_ms: age,
            distance_px: distance,
            elapsed_ms: elapsed,
            clickable,
        }
    }

    #[test]
    fn user_input_pauses_and_continue_resumes() {
        let mut machine = Machine::new(DEFAULT_RESUME_MS);
        assert_eq!(machine.see_visor(VisorPhase::Running, 0, 0), Effect::None);
        assert_eq!(machine.phase(), Phase::Running);
        assert_eq!(machine.user_input(1_000), Effect::Pause);
        assert_eq!(machine.phase(), Phase::Paused);
        assert!(!machine.asking());
        machine.ask(1_500);
        assert!(machine.asking());
        assert_eq!(machine.continue_now(1_600), Effect::Resume);
        assert_eq!(machine.phase(), Phase::Running);
        assert!(!machine.asking());
    }

    #[test]
    fn continue_without_the_prompt_does_nothing() {
        let mut machine = Machine::new(DEFAULT_RESUME_MS);
        machine.see_visor(VisorPhase::Running, 0, 0);
        machine.user_input(10);
        assert_eq!(machine.continue_now(20), Effect::None);
        assert_eq!(machine.phase(), Phase::Paused);
    }

    #[test]
    fn timeout_resumes_after_two_minutes_without_input() {
        let mut machine = Machine::new(DEFAULT_RESUME_MS);
        machine.see_visor(VisorPhase::Running, 0, 0);
        assert_eq!(machine.user_input(5_000), Effect::Pause);
        assert_eq!(machine.poll(5_000 + DEFAULT_RESUME_MS - 1), Effect::None);
        assert_eq!(machine.phase(), Phase::Paused);
        assert_eq!(machine.poll(5_000 + DEFAULT_RESUME_MS), Effect::Resume);
        assert_eq!(machine.phase(), Phase::Running);
    }

    #[test]
    fn input_during_the_wait_keeps_it_paused() {
        let mut machine = Machine::new(DEFAULT_RESUME_MS);
        machine.see_visor(VisorPhase::Running, 0, 0);
        machine.user_input(0);
        machine.ask(1_000);
        assert_eq!(machine.user_input(60_000), Effect::None);
        assert!(machine.asking());
        assert_eq!(machine.poll(60_000 + DEFAULT_RESUME_MS - 1), Effect::None);
        assert_eq!(machine.phase(), Phase::Paused);
        assert_eq!(machine.poll(60_000 + DEFAULT_RESUME_MS), Effect::Resume);
        assert!(!machine.asking());
    }

    #[test]
    fn dismiss_stays_paused_and_a_stopped_visor_clears_it() {
        let mut machine = Machine::new(1_000);
        machine.see_visor(VisorPhase::Running, 0, 0);
        machine.user_input(0);
        machine.ask(10);
        machine.dismiss(20);
        assert!(!machine.asking());
        assert_eq!(machine.phase(), Phase::Paused);
        assert_eq!(machine.poll(20 + 999), Effect::None);
        machine.see_visor(VisorPhase::Off, 30, 0);
        assert_eq!(machine.phase(), Phase::Off);
        assert_eq!(machine.poll(10_000), Effect::None);
    }

    #[test]
    fn stale_running_report_does_not_undo_a_local_pause() {
        let mut machine = Machine::new(DEFAULT_RESUME_MS);
        machine.see_visor(VisorPhase::Running, 0, 0);
        machine.user_input(100);
        machine.see_visor(VisorPhase::Running, 200, 0);
        assert_eq!(machine.phase(), Phase::Paused);
    }

    #[test]
    fn a_newer_resume_sequence_follows_a_skipped_pause() {
        let mut machine = Machine::new(1_000);
        machine.see_visor(VisorPhase::Running, 0, 3);
        machine.user_input(10);
        machine.see_visor(VisorPhase::Running, 20, 3);
        assert_eq!(machine.phase(), Phase::Paused);
        machine.see_visor(VisorPhase::Running, 30, 4);
        assert_eq!(machine.phase(), Phase::Running);

        machine.user_input(40);
        machine.see_visor(VisorPhase::Paused, 50, 4);
        machine.note_disconnect();
        assert_eq!(machine.phase(), Phase::Paused);
        machine.see_visor(VisorPhase::Running, 60, 4);
        assert_eq!(machine.phase(), Phase::Paused);
        machine.see_visor(VisorPhase::Running, 70, 5);
        assert_eq!(machine.phase(), Phase::Running);
    }

    #[test]
    fn pause_is_retried_after_a_disconnect() {
        let mut machine = Machine::new(1_000);
        machine.see_visor(VisorPhase::Running, 0, 0);
        assert_eq!(machine.user_input(10), Effect::Pause);
        machine.note_disconnect();
        assert_eq!(machine.phase(), Phase::Paused);
        machine.see_visor(VisorPhase::Running, 20, 0);
        assert_eq!(machine.phase(), Phase::Paused);
        machine.see_visor(VisorPhase::Off, 30, 0);
        assert_eq!(machine.phase(), Phase::Off);
    }

    #[test]
    fn disconnect_without_a_pause_clears_a_running_automation() {
        let mut machine = Machine::new(1_000);
        machine.see_visor(VisorPhase::Running, 0, 0);
        machine.note_disconnect();
        assert_eq!(machine.phase(), Phase::Off);
        machine.see_visor(VisorPhase::Running, 10, 0);
        assert_eq!(machine.phase(), Phase::Running);
    }

    #[test]
    fn stale_paused_report_does_not_undo_a_local_resume() {
        let mut machine = Machine::new(1_000);
        machine.see_visor(VisorPhase::Running, 0, 0);
        machine.user_input(10);
        machine.ask(20);
        assert_eq!(machine.continue_now(30), Effect::Resume);
        machine.see_visor(VisorPhase::Paused, 40, 0);
        assert_eq!(machine.phase(), Phase::Running);
        machine.see_visor(VisorPhase::Running, 50, 0);
        machine.see_visor(VisorPhase::Paused, 60, 0);
        assert_eq!(machine.phase(), Phase::Paused);
        assert_eq!(machine.poll(60 + 1_000), Effect::Resume);
        machine.see_visor(VisorPhase::Off, 2_000, 0);
        assert_eq!(machine.phase(), Phase::Off);
    }

    #[test]
    fn paused_fallback_tracks_later_pointer_and_button_changes() {
        let button = 1 << 8;
        assert!(!fallback_paused_activity(10, 10, 0, 10, 10, 0));
        assert!(!fallback_paused_activity(10, 10, 0, 22, 10, 0));
        assert!(fallback_paused_activity(10, 10, 0, 23, 10, 0));
        assert!(fallback_paused_activity(10, 10, 0, 10, 10, button));
        assert!(!fallback_paused_activity(10, 10, button, 10, 10, button));
        assert!(fallback_paused_activity(10, 10, button, 10, 10, 0));
        assert!(!fallback_paused_activity(0, 0, 0, 0, 0, 1));
    }

    #[test]
    fn fallback_ignores_a_drive_from_before_resume() {
        let cutoff = Some(resume_cutoff(3, 4));
        assert_eq!(cutoff, Some(3));
        assert!(!drive_counts(3, cutoff));
        assert!(!drive_counts(2, cutoff));
        assert!(drive_counts(4, cutoff));
        assert!(drive_counts(0, None));
        assert!(!fallback_displaced(true, 3, cutoff, 500, 80));
        assert!(!fallback_displaced(true, 2, cutoff, 500, 80));
        assert!(fallback_displaced(true, 4, cutoff, 151, 13));
        assert!(!fallback_displaced(true, 4, cutoff, 150, 13));
        assert!(!fallback_displaced(true, 4, cutoff, 400, 12));
        assert!(!fallback_displaced(false, 4, cutoff, 400, 80));
        assert!(fallback_displaced(true, 0, None, 151, 13));
    }

    #[test]
    fn displacement_waits_for_a_later_poll_of_the_same_drive() {
        let (pause, hold) = consider_displacement(true, 10, 5, None);
        assert!(!pause);
        let (pause, hold) = consider_displacement(true, 10, 5, hold);
        assert!(!pause, "the same snapshot is not a new poll");
        let (pause, hold) = consider_displacement(true, 10, 6, hold);
        assert!(!pause, "one later poll can still be the pre-move drive");
        let (pause, hold) = consider_displacement(true, 10, 7, hold);
        assert!(pause);
        let (pause, hold) = consider_displacement(true, 80, 8, hold);
        assert!(!pause, "a newly recorded drive starts the wait over");
        let (pause, cleared) = consider_displacement(false, 80, 9, hold);
        assert!(!pause);
        assert!(cleared.is_none());
    }

    #[test]
    fn fallback_button_press_counts_a_click_on_a_fresh_drive() {
        let button1 = 1 << 8;
        assert!(fallback_button_press(0, button1));
        assert!(!fallback_button_press(button1, button1));
        assert!(fallback_button_press(0, 1 << 12));
        assert!(!fallback_button_press(0, 1 << 0));
        assert_eq!(newly_pressed(0, button1 | 1), button1);
    }

    #[test]
    fn synthetic_devices_are_not_user_input() {
        let xtest = [7u16, 8];
        assert!(is_synthetic(7, 2, &xtest));
        assert!(is_synthetic(2, 8, &xtest));
        assert!(!is_synthetic(3, 4, &xtest));
        assert!(!is_synthetic(7, 7, &[]));
    }

    #[test]
    fn cursor_matches_the_drive_and_hides_when_paused() {
        let crawl = motion(MotionKind::Move, 50, 40, 200, false);
        assert_eq!(
            cursor_pose(Phase::Running, false, Some(crawl)),
            CursorPose::Crawl
        );
        let far = motion(MotionKind::Move, 50, FLY_DISTANCE_PX, 5_000, false);
        assert_eq!(
            cursor_pose(Phase::Running, false, Some(far)),
            CursorPose::Fly
        );
        let fast = motion(MotionKind::Drag, 10, 100, 50, false);
        assert_eq!(
            cursor_pose(Phase::Running, false, Some(fast)),
            CursorPose::Fly
        );
        let hover = motion(MotionKind::Move, MOVE_FRESH_MS, 10, 100, true);
        assert_eq!(
            cursor_pose(Phase::Running, false, Some(hover)),
            CursorPose::Reach
        );
        let click = motion(MotionKind::Click, 50, 0, 0, false);
        assert_eq!(
            cursor_pose(Phase::Running, false, Some(click)),
            CursorPose::Click
        );
        let typed = motion(MotionKind::Type, 100, 0, 0, false);
        assert_eq!(
            cursor_pose(Phase::Running, false, Some(typed)),
            CursorPose::Type
        );
        assert_eq!(cursor_pose(Phase::Running, false, None), CursorPose::Busy);
        assert_eq!(
            cursor_pose(Phase::Paused, true, Some(crawl)),
            CursorPose::System
        );
        assert_eq!(cursor_pose(Phase::Off, false, None), CursorPose::System);
        assert_eq!(cursor_pose(Phase::Off, true, None), CursorPose::Pointer);
    }

    #[test]
    fn click_plays_once_and_busy_loops() {
        assert_eq!(cursor_frame(CursorPose::Click, 0), 0);
        assert_eq!(cursor_frame(CursorPose::Click, 100), 1);
        assert_eq!(cursor_frame(CursorPose::Click, 250), 1);
        assert_eq!(cursor_frame_name(CursorPose::Click, 0), "click1");
        assert_eq!(cursor_frame_name(CursorPose::Click, 1), "click2");
        assert_eq!(cursor_frame(CursorPose::Busy, 300), 3);
        assert_eq!(cursor_frame(CursorPose::Busy, 400), 0);
        assert_eq!(cursor_frame_name(CursorPose::Fly, 0), "fly");
    }

    #[test]
    fn hotspot_matches_the_file_and_rounds_half_to_even() {
        let file: Value =
            serde_json::from_str(include_str!("../assets/cursor/hotspot.json")).expect("hotspot");
        for size in [32u32, 64, 128] {
            let pair = file["hotspot"][size.to_string()].as_array().unwrap();
            assert_eq!(
                hotspot_for(size),
                (
                    pair[0].as_u64().unwrap() as u16,
                    pair[1].as_u64().unwrap() as u16
                )
            );
        }
        assert_eq!(hotspot_for(64), (20, 10));
        let cursor: Value =
            serde_json::from_str(include_str!("../assets/cursor/cursor.json")).expect("cursor");
        assert_eq!(cursor["frame_ms"].as_u64(), Some(CURSOR_FRAME_MS));
    }

    #[test]
    fn link_waits_for_three_misses() {
        let mut link = Link::new("down");
        assert!(link.disconnected());
        link.ok("up");
        assert!(!link.disconnected());
        link.miss("down");
        link.miss("down");
        assert_eq!(link.current(), &"up");
        link.miss("down");
        assert!(link.disconnected());
        assert_eq!(link.current(), &"down");
    }

    fn running_at(x: i32, y: i32, mask: u32, now_ms: u64) -> FallbackInput {
        FallbackInput {
            phase: Phase::Running,
            x,
            y,
            mask,
            now_ms,
            wall_ms: 10_000,
            epoch: 1,
            injecting: false,
            inject_seq: 0,
            drive: None,
            after_resume: None,
        }
    }

    #[test]
    fn a_newer_paused_snapshot_after_resume_is_a_re_pause() {
        let mut machine = Machine::new(1_000);
        machine.see_visor(VisorPhase::Running, 0, 3);
        machine.user_input(10);
        machine.ask(20);
        assert_eq!(machine.continue_now(30), Effect::Resume);
        machine.see_visor(VisorPhase::Paused, 40, 3);
        assert_eq!(machine.phase(), Phase::Running);
        machine.see_visor(VisorPhase::Paused, 50, 4);
        assert_eq!(machine.phase(), Phase::Paused);
        machine.see_visor(VisorPhase::Running, 60, 4);
        assert_eq!(machine.phase(), Phase::Paused);
        machine.see_visor(VisorPhase::Running, 70, 5);
        assert_eq!(machine.phase(), Phase::Running);

        machine.user_input(80);
        assert_eq!(machine.poll(80 + 1_000), Effect::Resume);
        machine.see_visor(VisorPhase::Paused, 90, 5);
        assert_eq!(machine.phase(), Phase::Running);
        machine.see_visor(VisorPhase::Paused, 100, 6);
        assert_eq!(machine.phase(), Phase::Paused);
    }

    #[test]
    fn fallback_cases_cover_start_resume_type_click_and_a_cleared_drive() {
        let button = 1 << 8;
        let mut track = FallbackTrack::default();
        let mut sample = running_at(0, 0, 0, 0);
        assert!(!classify_fallback(&sample, &mut track));
        sample.x = 12;
        assert!(!classify_fallback(&sample, &mut track));
        sample.x = 13;
        assert!(classify_fallback(&sample, &mut track));

        track = FallbackTrack::default();
        sample = running_at(0, 0, 0, 0);
        assert!(!classify_fallback(&sample, &mut track));
        sample.mask = button;
        assert!(classify_fallback(&sample, &mut track));

        track = FallbackTrack::default();
        sample = running_at(4, 4, 0, 0);
        sample.inject_seq = 1;
        sample.drive = Some(FallbackDrive {
            x: 4,
            y: 4,
            at_ms: 9_000,
            pointer_known: true,
            resume_seq: 2,
            inject_seq: 1,
        });
        sample.wall_ms = 9_200;
        assert!(!classify_fallback(&sample, &mut track));
        sample.drive = None;
        sample.inject_seq = 1;
        assert!(!classify_fallback(&sample, &mut track));
        sample.x = 30;
        assert!(classify_fallback(&sample, &mut track));

        track = FallbackTrack::default();
        sample = running_at(8, 8, 0, 0);
        sample.injecting = true;
        sample.inject_seq = 3;
        assert!(!classify_fallback(&sample, &mut track));
        sample.drive = Some(FallbackDrive {
            x: 0,
            y: 0,
            at_ms: 1,
            pointer_known: false,
            resume_seq: 1,
            inject_seq: 3,
        });
        sample.injecting = false;
        assert!(!classify_fallback(&sample, &mut track));
        sample.x = 40;
        assert!(classify_fallback(&sample, &mut track));

        track = FallbackTrack::default();
        sample = running_at(10, 10, 0, 0);
        sample.after_resume = Some(4);
        sample.drive = Some(FallbackDrive {
            x: 200,
            y: 200,
            at_ms: 1,
            pointer_known: true,
            resume_seq: 4,
            inject_seq: 0,
        });
        assert!(!classify_fallback(&sample, &mut track));
        assert!(!classify_fallback(&sample, &mut track));

        track = FallbackTrack::default();
        sample = running_at(0, 0, 0, 0);
        assert!(!classify_fallback(&sample, &mut track));
        sample.phase = Phase::Off;
        assert!(!classify_fallback(&sample, &mut track));
        sample.phase = Phase::Running;
        sample.x = 80;
        assert!(!classify_fallback(&sample, &mut track));
    }

    #[test]
    fn fallback_defers_an_in_flight_click_until_the_drive_or_the_timeout() {
        let button = 1 << 8;
        let mut track = FallbackTrack::default();
        let mut sample = running_at(20, 20, button, 0);
        sample.injecting = true;
        sample.inject_seq = 1;
        assert!(!classify_fallback(&sample, &mut track));
        sample.x = 80;
        sample.now_ms = 100;
        assert!(!classify_fallback(&sample, &mut track));

        sample.injecting = false;
        sample.mask = 0;
        sample.x = 20;
        sample.drive = Some(FallbackDrive {
            x: 20,
            y: 20,
            at_ms: 9_000,
            pointer_known: true,
            resume_seq: 1,
            inject_seq: 1,
        });
        sample.epoch = 1;
        assert!(!classify_fallback(&sample, &mut track));

        sample.injecting = true;
        sample.inject_seq = 2;
        sample.mask = button;
        sample.x = 90;
        sample.drive = Some(FallbackDrive {
            x: 20,
            y: 20,
            at_ms: 9_000,
            pointer_known: true,
            resume_seq: 1,
            inject_seq: 1,
        });
        assert!(!classify_fallback(&sample, &mut track));

        let mut hung = running_at(0, 0, 0, 0);
        hung.injecting = true;
        hung.inject_seq = 7;
        assert!(!classify_fallback(&hung, &mut track));
        hung.x = 50;
        hung.now_ms = 10;
        assert!(!classify_fallback(&hung, &mut track));
        hung.now_ms = 10 + INJECT_HOLD_MS;
        assert!(!classify_fallback(&hung, &mut track));
        hung.x = 70;
        hung.now_ms += 30;
        assert!(classify_fallback(&hung, &mut track));
    }

    #[test]
    fn fallback_confirms_displacement_only_on_later_polls() {
        let mut track = FallbackTrack::default();
        let mut sample = running_at(0, 0, 0, 0);
        sample.drive = Some(FallbackDrive {
            x: 0,
            y: 0,
            at_ms: 1_000,
            pointer_known: true,
            resume_seq: 1,
            inject_seq: 1,
        });
        sample.inject_seq = 1;
        sample.wall_ms = 2_000;
        sample.x = 40;
        sample.epoch = 5;
        assert!(!classify_fallback(&sample, &mut track));
        assert!(!classify_fallback(&sample, &mut track));
        sample.epoch = 6;
        assert!(!classify_fallback(&sample, &mut track));
        sample.epoch = 7;
        assert!(classify_fallback(&sample, &mut track));
    }
}
