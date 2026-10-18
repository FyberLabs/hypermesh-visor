//! Animated mascot frames from `assets/states/states.json`.
//!
//! Which state shows is chosen from signals the companion already has
//! (the visor poll, an automation phase, and how long the app has been idle).
//! An update is not one of those signals. See [`Signals::upgrading`].

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mascot {
    Idle,
    Automation,
    Upgrading,
    Disconnected,
    Sleeping,
}

impl Mascot {
    pub const ALL: [Mascot; 5] = [
        Mascot::Idle,
        Mascot::Automation,
        Mascot::Upgrading,
        Mascot::Disconnected,
        Mascot::Sleeping,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Automation => "automation",
            Self::Upgrading => "upgrading",
            Self::Disconnected => "disconnected",
            Self::Sleeping => "sleeping",
        }
    }
}

/// Why a state was chosen. `upgrading` is an external notice. Nothing in this
/// process sets it on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signals {
    pub connected: bool,
    /// An updater published this. The visor field `notice` or the file
    /// `companion-notice` containing `upgrading`.
    pub upgrading: bool,
    /// Visor is driving the desktop and the automation is not paused.
    pub automation_running: bool,
    /// Milliseconds since the companion or the open session was last used.
    pub unused_for_ms: u64,
    pub sleep_after_ms: u64,
}

/// Upgrading wins even when the poll fails, because an update may stop the
/// daemon. Disconnected wins over a stale automation. A paused automation is
/// not `automation_running`, so the mascot returns to idle (or sleeping).
pub fn choose(signals: &Signals) -> Mascot {
    if signals.upgrading {
        return Mascot::Upgrading;
    }
    if !signals.connected {
        return Mascot::Disconnected;
    }
    if signals.automation_running {
        return Mascot::Automation;
    }
    if signals.unused_for_ms >= signals.sleep_after_ms {
        return Mascot::Sleeping;
    }
    Mascot::Idle
}

/// Idle time is the sooner of local use and visor-session use.
/// No open session means only the companion's own last use counts.
pub fn unused_for_ms(local_idle_ms: u64, session_idle_ms: Option<u64>) -> u64 {
    match session_idle_ms {
        Some(session) => local_idle_ms.min(session),
        None => local_idle_ms,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reel {
    state: Mascot,
    frame: usize,
    acc_ms: u64,
}

impl Reel {
    pub fn new(state: Mascot) -> Self {
        Self {
            state,
            frame: 0,
            acc_ms: 0,
        }
    }

    pub fn frame(&self) -> usize {
        self.frame
    }

    pub fn state(&self) -> Mascot {
        self.state
    }

    pub fn set_state(&mut self, state: Mascot) {
        if self.state != state {
            self.state = state;
            self.frame = 0;
            self.acc_ms = 0;
        }
    }

    /// Advance by real elapsed time. `looped` comes from states.json.
    pub fn advance(&mut self, dt_ms: u64, ms_per_frame: u64, count: usize, looped: bool) {
        if count == 0 {
            self.frame = 0;
            return;
        }
        if self.frame >= count {
            self.frame = 0;
        }
        let step = ms_per_frame.max(1);
        self.acc_ms = self.acc_ms.saturating_add(dt_ms);
        while self.acc_ms >= step {
            self.acc_ms -= step;
            if self.frame + 1 >= count {
                if looped {
                    self.frame = 0;
                } else {
                    self.frame = count - 1;
                    self.acc_ms = 0;
                    break;
                }
            } else {
                self.frame += 1;
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Clip {
    pub ms_per_frame: u64,
    pub looped: bool,
    pub files: Vec<String>,
}

pub struct Catalog {
    clips: [Clip; 5],
}

impl Catalog {
    pub fn load() -> Self {
        let file: StatesFile =
            serde_json::from_str(include_str!("../assets/states/states.json")).expect("states.json");
        let clips = Mascot::ALL.map(|state| {
            let spec = file
                .states
                .get(state.as_str())
                .unwrap_or_else(|| panic!("states.json missing {}", state.as_str()));
            let files = spec
                .frames
                .get("400x520")
                .unwrap_or_else(|| panic!("{} missing 400x520", state.as_str()))
                .clone();
            if files.is_empty() {
                panic!("{} has no frames", state.as_str());
            }
            for name in &files {
                if state_png(name).is_none() {
                    panic!("missing embedded frame {name}");
                }
            }
            Clip {
                ms_per_frame: spec.ms_per_frame,
                looped: spec.looped,
                files,
            }
        });
        Self { clips }
    }

    pub fn clip(&self, state: Mascot) -> &Clip {
        &self.clips[state as usize]
    }
}

#[derive(Deserialize)]
struct StatesFile {
    states: BTreeMap<String, StateSpec>,
}

#[derive(Deserialize)]
struct StateSpec {
    ms_per_frame: u64,
    #[serde(rename = "loop")]
    looped: bool,
    frames: BTreeMap<String, Vec<String>>,
}

pub fn state_png(name: &str) -> Option<&'static [u8]> {
    Some(match name {
        "idle_0_400x520.png" => include_bytes!("../assets/states/idle_0_400x520.png"),
        "idle_1_400x520.png" => include_bytes!("../assets/states/idle_1_400x520.png"),
        "idle_2_400x520.png" => include_bytes!("../assets/states/idle_2_400x520.png"),
        "idle_3_400x520.png" => include_bytes!("../assets/states/idle_3_400x520.png"),
        "idle_4_400x520.png" => include_bytes!("../assets/states/idle_4_400x520.png"),
        "idle_5_400x520.png" => include_bytes!("../assets/states/idle_5_400x520.png"),
        "idle_6_400x520.png" => include_bytes!("../assets/states/idle_6_400x520.png"),
        "idle_7_400x520.png" => include_bytes!("../assets/states/idle_7_400x520.png"),
        "automation_0_400x520.png" => include_bytes!("../assets/states/automation_0_400x520.png"),
        "automation_1_400x520.png" => include_bytes!("../assets/states/automation_1_400x520.png"),
        "automation_2_400x520.png" => include_bytes!("../assets/states/automation_2_400x520.png"),
        "automation_3_400x520.png" => include_bytes!("../assets/states/automation_3_400x520.png"),
        "automation_4_400x520.png" => include_bytes!("../assets/states/automation_4_400x520.png"),
        "automation_5_400x520.png" => include_bytes!("../assets/states/automation_5_400x520.png"),
        "automation_6_400x520.png" => include_bytes!("../assets/states/automation_6_400x520.png"),
        "automation_7_400x520.png" => include_bytes!("../assets/states/automation_7_400x520.png"),
        "upgrading_0_400x520.png" => include_bytes!("../assets/states/upgrading_0_400x520.png"),
        "upgrading_1_400x520.png" => include_bytes!("../assets/states/upgrading_1_400x520.png"),
        "upgrading_2_400x520.png" => include_bytes!("../assets/states/upgrading_2_400x520.png"),
        "upgrading_3_400x520.png" => include_bytes!("../assets/states/upgrading_3_400x520.png"),
        "upgrading_4_400x520.png" => include_bytes!("../assets/states/upgrading_4_400x520.png"),
        "upgrading_5_400x520.png" => include_bytes!("../assets/states/upgrading_5_400x520.png"),
        "upgrading_6_400x520.png" => include_bytes!("../assets/states/upgrading_6_400x520.png"),
        "upgrading_7_400x520.png" => include_bytes!("../assets/states/upgrading_7_400x520.png"),
        "disconnected_0_400x520.png" => {
            include_bytes!("../assets/states/disconnected_0_400x520.png")
        }
        "disconnected_1_400x520.png" => {
            include_bytes!("../assets/states/disconnected_1_400x520.png")
        }
        "disconnected_2_400x520.png" => {
            include_bytes!("../assets/states/disconnected_2_400x520.png")
        }
        "disconnected_3_400x520.png" => {
            include_bytes!("../assets/states/disconnected_3_400x520.png")
        }
        "disconnected_4_400x520.png" => {
            include_bytes!("../assets/states/disconnected_4_400x520.png")
        }
        "disconnected_5_400x520.png" => {
            include_bytes!("../assets/states/disconnected_5_400x520.png")
        }
        "sleeping_0_400x520.png" => include_bytes!("../assets/states/sleeping_0_400x520.png"),
        "sleeping_1_400x520.png" => include_bytes!("../assets/states/sleeping_1_400x520.png"),
        "sleeping_2_400x520.png" => include_bytes!("../assets/states/sleeping_2_400x520.png"),
        "sleeping_3_400x520.png" => include_bytes!("../assets/states/sleeping_3_400x520.png"),
        "sleeping_4_400x520.png" => include_bytes!("../assets/states/sleeping_4_400x520.png"),
        "sleeping_5_400x520.png" => include_bytes!("../assets/states/sleeping_5_400x520.png"),
        "sleeping_6_400x520.png" => include_bytes!("../assets/states/sleeping_6_400x520.png"),
        "sleeping_7_400x520.png" => include_bytes!("../assets/states/sleeping_7_400x520.png"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signals(connected: bool, upgrading: bool, running: bool, unused: u64) -> Signals {
        Signals {
            connected,
            upgrading,
            automation_running: running,
            unused_for_ms: unused,
            sleep_after_ms: 900_000,
        }
    }

    #[test]
    fn catalog_uses_states_json_timing() {
        let cat = Catalog::load();
        let idle = cat.clip(Mascot::Idle);
        assert_eq!(idle.ms_per_frame, 150);
        assert_eq!(idle.files.len(), 8);
        assert!(idle.looped);
        assert_eq!(cat.clip(Mascot::Automation).ms_per_frame, 90);
        assert_eq!(cat.clip(Mascot::Automation).files.len(), 8);
        assert_eq!(cat.clip(Mascot::Upgrading).ms_per_frame, 130);
        assert_eq!(cat.clip(Mascot::Sleeping).ms_per_frame, 260);
        let gone = cat.clip(Mascot::Disconnected);
        assert_eq!(gone.files.len(), 6);
        assert_eq!(gone.ms_per_frame, 110);
        assert!(gone.looped);
        assert!(state_png(&idle.files[0]).is_some());
    }

    #[test]
    fn state_priority_follows_the_signals() {
        assert_eq!(choose(&signals(false, false, false, 0)), Mascot::Disconnected);
        assert_eq!(choose(&signals(false, true, true, 0)), Mascot::Upgrading);
        assert_eq!(choose(&signals(true, false, true, 0)), Mascot::Automation);
        assert_eq!(
            choose(&signals(true, false, false, 900_000)),
            Mascot::Sleeping
        );
        assert_eq!(choose(&signals(true, false, false, 899_999)), Mascot::Idle);
        assert_eq!(choose(&signals(true, false, false, 0)), Mascot::Idle);
    }

    #[test]
    fn unused_time_is_the_sooner_of_local_and_session() {
        assert_eq!(unused_for_ms(5_000, None), 5_000);
        assert_eq!(unused_for_ms(5_000, Some(100)), 100);
        assert_eq!(unused_for_ms(100, Some(5_000)), 100);
    }

    #[test]
    fn reel_holds_each_frame_and_loops() {
        let mut reel = Reel::new(Mascot::Idle);
        reel.advance(149, 150, 8, true);
        assert_eq!(reel.frame(), 0);
        reel.advance(1, 150, 8, true);
        assert_eq!(reel.frame(), 1);
        reel.advance(150 * 7, 150, 8, true);
        assert_eq!(reel.frame(), 0);
        reel.set_state(Mascot::Disconnected);
        assert_eq!(reel.frame(), 0);
        reel.advance(110 * 5, 110, 6, true);
        assert_eq!(reel.frame(), 5);
        reel.advance(110, 110, 6, false);
        assert_eq!(reel.frame(), 5);
    }
}
