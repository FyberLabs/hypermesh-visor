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
/// X11 XTest motion uses a signed 16-bit coordinate.
pub const X11_COORD_MIN: i32 = i16::MIN as i32;
pub const X11_COORD_MAX: i32 = i16::MAX as i32;
/// Same limit the Wayland portal path uses before a pointer call.
pub const WAYLAND_COORD_LIMIT: i32 = 1_048_576;
/// Interpolated drag points. A full i16 span is about 1024 steps of 64 px.
/// Anything larger is refused. The steps themselves are not stored.
pub const DRAG_STEP_CAP: usize = 4_096;
/// Pointer events sent on one X connection before the pause latch is checked.
pub const POINTER_BATCH: usize = 16;

/// The span would walk more than [`DRAG_STEP_CAP`] points.
#[derive(Debug, PartialEq, Eq)]
pub struct DragLimit;

/// `x11` and the fixture use the XTest i16 range. `wayland` uses the portal limit.
pub fn pointer_axis_in_bounds(backend: &str, value: i32) -> bool {
    match backend {
        "wayland" => (-WAYLAND_COORD_LIMIT..=WAYLAND_COORD_LIMIT).contains(&value),
        _ => (X11_COORD_MIN..=X11_COORD_MAX).contains(&value),
    }
}

pub fn mouse_in_bounds(backend: &str, op: &MouseOp) -> bool {
    let axis = |value| pointer_axis_in_bounds(backend, value);
    match *op {
        MouseOp::Move { x, y, .. } | MouseOp::Click { x, y, .. } => axis(x) && axis(y),
        MouseOp::Drag {
            x, y, to_x, to_y, ..
        } => axis(x) && axis(y) && axis(to_x) && axis(to_y),
    }
}

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

/// How many points a drag would emit, including the end and excluding the
/// start. This does not build the points. A span over [`DRAG_STEP_CAP`] is
/// refused.
pub fn drag_point_count(x0: i32, y0: i32, x1: i32, y1: i32) -> Result<usize, DragLimit> {
    let dx = i64::from(x1) - i64::from(x0);
    let dy = i64::from(y1) - i64::from(y0);
    let dist = dx.abs().max(dy.abs());
    let steps = if dist <= i64::from(DRAG_STEP_PX) {
        1
    } else {
        (dist + i64::from(DRAG_STEP_PX) - 1) / i64::from(DRAG_STEP_PX)
    };
    if steps > DRAG_STEP_CAP as i64 {
        Err(DragLimit)
    } else {
        Ok(steps as usize)
    }
}

/// Points from `(x0, y0)` toward `(x1, y1)`, including the end and excluding
/// the start. A short drag is just the end, so it stays one mouse call.
/// Points are yielded one at a time. Nothing is stored up front.
pub fn drag_waypoints(x0: i32, y0: i32, x1: i32, y1: i32) -> Result<DragWaypoints, DragLimit> {
    let steps = drag_point_count(x0, y0, x1, y1)?;
    Ok(DragWaypoints {
        x0: i64::from(x0),
        y0: i64::from(y0),
        dx: i64::from(x1) - i64::from(x0),
        dy: i64::from(y1) - i64::from(y0),
        steps: steps as i64,
        next_i: 1,
    })
}

/// One drag point at a time. The struct holds the span, not the points.
#[derive(Clone, Debug)]
pub struct DragWaypoints {
    x0: i64,
    y0: i64,
    dx: i64,
    dy: i64,
    steps: i64,
    next_i: i64,
}

impl Iterator for DragWaypoints {
    type Item = (i32, i32);

    fn next(&mut self) -> Option<(i32, i32)> {
        if self.next_i > self.steps {
            return None;
        }
        let i = self.next_i;
        self.next_i += 1;
        let x = self.x0 + self.dx * i / self.steps;
        let y = self.y0 + self.dy * i / self.steps;
        Some((i32_coord(x), i32_coord(y)))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = (self.steps - self.next_i + 1).max(0) as usize;
        (left, Some(left))
    }
}

fn i32_coord(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value > 0 { i32::MAX } else { i32::MIN })
}

/// Move to the start, press, step along the path, and release.
/// Yields one [`PointerStep`] at a time and does not collect them.
pub fn drag_steps(
    x: i32,
    y: i32,
    to_x: i32,
    to_y: i32,
    button: Button,
) -> Result<DragSteps, DragLimit> {
    Ok(DragSteps {
        x,
        y,
        button,
        waypoints: drag_waypoints(x, y, to_x, to_y)?,
        phase: 0,
    })
}

/// One pointer step of a drag, produced as the caller asks for it.
#[derive(Clone, Debug)]
pub struct DragSteps {
    x: i32,
    y: i32,
    button: Button,
    waypoints: DragWaypoints,
    phase: u8,
}

impl Iterator for DragSteps {
    type Item = PointerStep;

    fn next(&mut self) -> Option<PointerStep> {
        match self.phase {
            0 => {
                self.phase = 1;
                Some(PointerStep::Move {
                    x: self.x,
                    y: self.y,
                })
            }
            1 => {
                self.phase = 2;
                Some(PointerStep::Button {
                    button: self.button,
                    down: true,
                })
            }
            2 => {
                if let Some((x, y)) = self.waypoints.next() {
                    Some(PointerStep::Move { x, y })
                } else {
                    self.phase = 3;
                    Some(PointerStep::Button {
                        button: self.button,
                        down: false,
                    })
                }
            }
            _ => None,
        }
    }
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
        assert_eq!(
            drag_waypoints(1, 2, 8, 9).unwrap().collect::<Vec<_>>(),
            vec![(8, 9)]
        );
        let far = drag_waypoints(0, 0, 130, 0).unwrap().collect::<Vec<_>>();
        assert!(far.len() > 1);
        assert!(far[0].0 > 0 && far[0].0 < 130);
        assert_eq!(far.last().copied(), Some((130, 0)));
        let steps = drag_steps(0, 0, 130, 0, Button::Left)
            .unwrap()
            .collect::<Vec<_>>();
        assert!(matches!(
            steps.first(),
            Some(PointerStep::Move { x: 0, y: 0 })
        ));
        assert!(matches!(
            steps.last(),
            Some(PointerStep::Button { down: false, .. })
        ));
    }

    #[test]
    fn an_extreme_drag_is_refused_without_building_points() {
        assert!(drag_point_count(i32::MIN, 0, i32::MAX, 0).is_err());
        assert!(drag_waypoints(i32::MIN, i32::MIN, i32::MAX, i32::MAX).is_err());
        assert!(drag_steps(i32::MIN, 0, i32::MAX, 0, Button::Left).is_err());
        assert!(pointer_axis_in_bounds("x11", 0));
        assert!(pointer_axis_in_bounds("fixture", X11_COORD_MAX));
        assert!(!pointer_axis_in_bounds("fixture", 40_000));
        assert!(!pointer_axis_in_bounds("x11", i32::MAX));
        assert!(pointer_axis_in_bounds("wayland", 40_000));
        assert!(pointer_axis_in_bounds("wayland", WAYLAND_COORD_LIMIT));
        assert!(!pointer_axis_in_bounds("wayland", WAYLAND_COORD_LIMIT + 1));
        let op = MouseOp::Drag {
            x: 0,
            y: 0,
            to_x: 40_000,
            to_y: 0,
            button: Button::Left,
        };
        assert!(!mouse_in_bounds("fixture", &op));
        assert!(mouse_in_bounds("wayland", &op));
    }
}
