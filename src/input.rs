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
}
