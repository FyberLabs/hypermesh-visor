use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

impl Default for Button {
    fn default() -> Self {
        Self::Left
    }
}

impl Button {
    fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Middle => "middle",
        }
    }
}

/// What the renter feed may show for a mouse verb. Coordinates only.
pub fn mouse_activity(op: &MouseOp) -> String {
    match op {
        MouseOp::Move { x, y, .. } => format!("move {x},{y}"),
        MouseOp::Click { x, y, button } => format!("click {x},{y} {}", button.as_str()),
        MouseOp::Drag {
            x,
            y,
            to_x,
            to_y,
            button,
        } => format!("drag {x},{y} to {to_x},{to_y} {}", button.as_str()),
    }
}

/// Named keys from a type verb. The secret value is not on this body.
pub fn keys_activity(body: &TypeBody) -> Option<String> {
    if body.keys.is_empty() {
        return None;
    }
    let mut names = Vec::new();
    for key in &body.keys {
        let name = key.key.trim();
        if name.is_empty() || name.len() > 32 || !name.chars().all(|ch| ch.is_ascii_graphic()) {
            continue;
        }
        names.push(name.to_string());
        if names.len() == 8 {
            break;
        }
    }
    if names.is_empty() {
        None
    } else {
        Some(format!("keys {}", names.join(" ")))
    }
}

impl<'de> Deserialize<'de> for Button {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.to_ascii_lowercase().as_str() {
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "middle" => Ok(Self::Middle),
            other => Err(serde::de::Error::custom(format!("unknown button {other}"))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum MouseOp {
    Move {
        x: i32,
        y: i32,
        /// The caller knows the pointer is over something that can be clicked.
        /// The visor does not discover that itself.
        #[serde(default)]
        clickable: bool,
    },
    Click {
        x: i32,
        y: i32,
        #[serde(default)]
        button: Button,
    },
    Drag {
        x: i32,
        y: i32,
        to_x: i32,
        to_y: i32,
        #[serde(default)]
        button: Button,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stroke {
    pub keysym: u32,
    pub down: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyAction {
    Tap,
    Down,
    Up,
}

impl Default for KeyAction {
    fn default() -> Self {
        Self::Tap
    }
}

#[derive(Deserialize)]
pub struct KeyBody {
    pub key: String,
    #[serde(default)]
    pub action: KeyAction,
}

#[derive(Deserialize)]
pub struct TypeBody {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub keys: Vec<KeyBody>,
    #[serde(default)]
    pub secret: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PlanError {
    Empty,
    SecretNotText,
    UnsupportedChar,
    UnknownKey(String),
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "nothing to type"),
            Self::SecretNotText => write!(f, "secret is not text"),
            Self::UnsupportedChar => write!(f, "unsupported character"),
            Self::UnknownKey(name) => write!(f, "unknown key \"{name}\""),
        }
    }
}

impl std::error::Error for PlanError {}

/// Keystrokes sent in one desktop call before the pause latch is checked again.
pub const TYPE_CHUNK_KEYSTROKES: usize = 32;
/// One `type` request may not plan more keystrokes than this.
pub const TYPE_MAX_KEYSTROKES: usize = 4_096;
/// A drag longer than this, in Chebyshev pixels, is sent as separate steps.
pub const DRAG_STEP_PX: i32 = 64;

/// One injected pointer event. A drag is a run of these so a pause can land
/// between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerStep {
    Move { x: i32, y: i32 },
    Button { button: Button, down: bool },
}

/// How many strokes belong to the next keystroke. A press and its matching
/// release stay together. A lone down or up is its own keystroke.
pub fn keystroke_width(strokes: &[Stroke]) -> usize {
    match strokes {
        [down, up, ..] if down.down && !up.down && down.keysym == up.keysym => 2,
        [_, ..] => 1,
        [] => 0,
    }
}

pub fn keystroke_count(strokes: &[Stroke]) -> usize {
    let mut count = 0;
    let mut index = 0;
    while index < strokes.len() {
        index += keystroke_width(&strokes[index..]);
        count += 1;
    }
    count
}

/// The prefix of `strokes` that contains at most `limit` keystrokes.
pub fn take_keystrokes(strokes: &[Stroke], limit: usize) -> &[Stroke] {
    let mut count = 0;
    let mut index = 0;
    while index < strokes.len() && count < limit {
        index += keystroke_width(&strokes[index..]);
        count += 1;
    }
    &strokes[..index]
}

/// Points from `(x0, y0)` toward `(x1, y1)`, including the end and excluding
/// the start. A short drag is just the end, so it stays one mouse call.
pub fn drag_waypoints(x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<(i32, i32)> {
    let dx = i64::from(x1) - i64::from(x0);
    let dy = i64::from(y1) - i64::from(y0);
    let dist = dx.abs().max(dy.abs());
    if dist <= i64::from(DRAG_STEP_PX) {
        return vec![(x1, y1)];
    }
    let steps = (dist + i64::from(DRAG_STEP_PX) - 1) / i64::from(DRAG_STEP_PX);
    let mut points = Vec::with_capacity(steps as usize);
    for i in 1..=steps {
        let x = i64::from(x0) + dx * i / steps;
        let y = i64::from(y0) + dy * i / steps;
        points.push((i32_coord(x), i32_coord(y)));
    }
    points
}

fn i32_coord(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value > 0 { i32::MAX } else { i32::MIN })
}

/// Move to the start, press, step along the path, and release.
pub fn drag_steps(x: i32, y: i32, to_x: i32, to_y: i32, button: Button) -> Vec<PointerStep> {
    let mut steps = vec![
        PointerStep::Move { x, y },
        PointerStep::Button { button, down: true },
    ];
    for (px, py) in drag_waypoints(x, y, to_x, to_y) {
        steps.push(PointerStep::Move { x: px, y: py });
    }
    steps.push(PointerStep::Button {
        button,
        down: false,
    });
    steps
}

pub fn plan_type(body: &TypeBody, secret: Option<&[u8]>) -> Result<Vec<Stroke>, PlanError> {
    let has_text = body.text.as_ref().is_some_and(|text| !text.is_empty());
    let has_keys = !body.keys.is_empty();
    if !has_text && !has_keys && secret.is_none() {
        return Err(PlanError::Empty);
    }
    let mut strokes = Vec::new();
    if let Some(text) = &body.text {
        strokes.extend(text_strokes(text)?);
    }
    if let Some(bytes) = secret {
        let text = std::str::from_utf8(bytes).map_err(|_| PlanError::SecretNotText)?;
        strokes.extend(text_strokes(text)?);
    }
    for key in &body.keys {
        strokes.extend(named_strokes(&key.key, key.action)?);
    }
    if strokes.is_empty() {
        return Err(PlanError::Empty);
    }
    Ok(strokes)
}

fn text_strokes(text: &str) -> Result<Vec<Stroke>, PlanError> {
    let mut strokes = Vec::new();
    for ch in text.chars() {
        let keysym = keysym_for_char(ch)?;
        strokes.push(Stroke { keysym, down: true });
        strokes.push(Stroke {
            keysym,
            down: false,
        });
    }
    Ok(strokes)
}

fn named_strokes(name: &str, action: KeyAction) -> Result<Vec<Stroke>, PlanError> {
    let keysym = keysym_for_name(name)?;
    match action {
        KeyAction::Tap => Ok(vec![
            Stroke { keysym, down: true },
            Stroke {
                keysym,
                down: false,
            },
        ]),
        KeyAction::Down => Ok(vec![Stroke { keysym, down: true }]),
        KeyAction::Up => Ok(vec![Stroke {
            keysym,
            down: false,
        }]),
    }
}

fn keysym_for_char(ch: char) -> Result<u32, PlanError> {
    match ch {
        '\n' | '\r' => Ok(0xff0d),
        '\t' => Ok(0xff09),
        ch if (ch as u32) <= 0xff => Ok(ch as u32),
        _ => Err(PlanError::UnsupportedChar),
    }
}

pub fn keysym_for_name(name: &str) -> Result<u32, PlanError> {
    if name.chars().count() == 1 {
        let ch = name.chars().next().unwrap();
        if ch.is_ascii() && !ch.is_control() {
            return keysym_for_char(ch);
        }
    }
    let keysym = match name.to_ascii_lowercase().as_str() {
        "return" | "enter" => 0xff0d,
        "tab" => 0xff09,
        "backspace" => 0xff08,
        "escape" | "esc" => 0xff1b,
        "space" => 0x0020,
        "left" => 0xff51,
        "up" => 0xff52,
        "right" => 0xff53,
        "down" => 0xff54,
        "delete" => 0xffff,
        "home" => 0xff50,
        "end" => 0xff57,
        "pageup" | "page_up" => 0xff55,
        "pagedown" | "page_down" => 0xff56,
        "shift" | "shift_l" => 0xffe1,
        "control" | "ctrl" | "control_l" => 0xffe3,
        "alt" | "alt_l" => 0xffe9,
        "super" | "super_l" => 0xffeb,
        other => function_key(other).ok_or_else(|| PlanError::UnknownKey(name.to_string()))?,
    };
    Ok(keysym)
}

fn function_key(name: &str) -> Option<u32> {
    let rest = name.strip_prefix('f')?;
    let number: u32 = rest.parse().ok()?;
    if (1..=12).contains(&number) {
        Some(0xffbe + number - 1)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_named_keys_become_strokes() {
        let body = TypeBody {
            text: Some("A\n".into()),
            keys: vec![KeyBody {
                key: "Tab".into(),
                action: KeyAction::Tap,
            }],
            secret: None,
        };
        let strokes = plan_type(&body, None).unwrap();
        assert_eq!(
            strokes,
            vec![
                Stroke {
                    keysym: b'A' as u32,
                    down: true
                },
                Stroke {
                    keysym: b'A' as u32,
                    down: false
                },
                Stroke {
                    keysym: 0xff0d,
                    down: true
                },
                Stroke {
                    keysym: 0xff0d,
                    down: false
                },
                Stroke {
                    keysym: 0xff09,
                    down: true
                },
                Stroke {
                    keysym: 0xff09,
                    down: false
                },
            ]
        );
    }

    #[test]
    fn secret_bytes_are_planned_and_bad_text_is_refused() {
        let body = TypeBody {
            text: None,
            keys: Vec::new(),
            secret: Some("password".into()),
        };
        let strokes = plan_type(&body, Some(b"ab")).unwrap();
        assert_eq!(strokes.len(), 4);
        assert_eq!(
            plan_type(&body, Some(&[0xff, 0xfe])),
            Err(PlanError::SecretNotText)
        );
        let emoji = TypeBody {
            text: Some("hi\u{1F600}".into()),
            keys: Vec::new(),
            secret: None,
        };
        assert_eq!(plan_type(&emoji, None), Err(PlanError::UnsupportedChar));
        let empty = TypeBody {
            text: None,
            keys: Vec::new(),
            secret: None,
        };
        assert_eq!(plan_type(&empty, None), Err(PlanError::Empty));
    }

    #[test]
    fn keystrokes_stay_paired_and_long_drags_step() {
        let strokes = plan_type(
            &TypeBody {
                text: Some("ab".into()),
                keys: vec![KeyBody {
                    key: "shift".into(),
                    action: KeyAction::Down,
                }],
                secret: None,
            },
            None,
        )
        .unwrap();
        assert_eq!(keystroke_count(&strokes), 3);
        assert_eq!(take_keystrokes(&strokes, 2).len(), 4);
        assert_eq!(take_keystrokes(&strokes, 3).len(), strokes.len());
        assert_eq!(drag_waypoints(1, 2, 8, 9), vec![(8, 9)]);
        let far = drag_waypoints(0, 0, 130, 0);
        assert!(far.len() > 1);
        assert!(far[0].0 > 0 && far[0].0 < 130);
        assert_eq!(far.last().copied(), Some((130, 0)));
        let steps = drag_steps(0, 0, 130, 0, Button::Left);
        assert!(matches!(
            steps.first(),
            Some(PointerStep::Move { x: 0, y: 0 })
        ));
        assert!(matches!(
            steps.last(),
            Some(PointerStep::Button { down: false, .. })
        ));
    }
}
