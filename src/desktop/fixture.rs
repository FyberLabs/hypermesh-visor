use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;

use base64::Engine;

use crate::desktop::{AudioRead, Desktop, DesktopError, Frame};
use crate::input::{Button, MouseOp, Stroke};

fn view_png() -> Vec<u8> {
    decode_fixture(include_str!("../../tests/fixtures/view.png.b64"))
}

fn listen_pcm() -> Vec<u8> {
    decode_fixture(include_str!("../../tests/fixtures/listen.pcm.b64"))
}

fn decode_fixture(encoded: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(encoded.trim().as_bytes())
        .expect("committed fixture is valid base64")
}

/// Desktop that never touches the logged-in session.
/// Mouse and type append the events the input path received.
pub struct FixtureDesktop {
    log: Mutex<BufWriter<File>>,
}

impl FixtureDesktop {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = File::create(path)?;
        Ok(Self {
            log: Mutex::new(BufWriter::new(file)),
        })
    }

    fn record(&self, value: serde_json::Value) -> Result<(), DesktopError> {
        let mut log = self.log.lock().unwrap_or_else(|poison| poison.into_inner());
        serde_json::to_writer(&mut *log, &value)
            .map_err(|err| DesktopError::Input(err.to_string()))?;
        log.write_all(b"\n")
            .map_err(|err| DesktopError::Input(err.to_string()))?;
        log.flush()
            .map_err(|err| DesktopError::Input(err.to_string()))?;
        Ok(())
    }
}

struct FixtureAudio {
    chunk: Option<Vec<u8>>,
}

impl AudioRead for FixtureAudio {
    fn read_chunk(&mut self) -> Result<Option<Vec<u8>>, DesktopError> {
        Ok(self.chunk.take())
    }
}

impl Desktop for FixtureDesktop {
    fn view(&self) -> Result<Frame, DesktopError> {
        Ok(Frame::png(view_png()))
    }

    fn open_audio(&self) -> Result<Box<dyn AudioRead>, DesktopError> {
        Ok(Box::new(FixtureAudio {
            chunk: Some(listen_pcm()),
        }))
    }

    fn mouse(&self, op: &MouseOp) -> Result<(), DesktopError> {
        self.record(mouse_event(op))
    }

    fn type_input(&self, strokes: &[Stroke]) -> Result<(), DesktopError> {
        self.record(serde_json::json!({
            "kind": "type",
            "strokes": strokes.iter().map(|stroke| serde_json::json!({
                "keysym": stroke.keysym,
                "down": stroke.down,
            })).collect::<Vec<_>>(),
        }))
    }
}

fn mouse_event(op: &MouseOp) -> serde_json::Value {
    match op {
        MouseOp::Move { x, y } => serde_json::json!({
            "kind": "mouse",
            "action": "move",
            "x": x,
            "y": y,
        }),
        MouseOp::Click { x, y, button } => serde_json::json!({
            "kind": "mouse",
            "action": "click",
            "x": x,
            "y": y,
            "button": button_name(*button),
        }),
        MouseOp::Drag {
            x,
            y,
            to_x,
            to_y,
            button,
        } => serde_json::json!({
            "kind": "mouse",
            "action": "drag",
            "x": x,
            "y": y,
            "to_x": to_x,
            "to_y": to_y,
            "button": button_name(*button),
        }),
    }
}

fn button_name(button: Button) -> &'static str {
    match button {
        Button::Left => "left",
        Button::Right => "right",
        Button::Middle => "middle",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_the_mouse_event_it_receives() {
        let path = std::env::temp_dir().join(format!(
            "hypermesh-visor-fixture-{}-{}.jsonl",
            std::process::id(),
            "mouse"
        ));
        let desktop = FixtureDesktop::open(&path).unwrap();
        desktop
            .mouse(&MouseOp::Click {
                x: 15,
                y: 80,
                button: Button::Right,
            })
            .unwrap();
        desktop
            .type_input(&[
                Stroke {
                    keysym: b'H' as u32,
                    down: true,
                },
                Stroke {
                    keysym: b'H' as u32,
                    down: false,
                },
            ])
            .unwrap();
        drop(desktop);
        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines[0]["action"], "click");
        assert_eq!(lines[0]["x"], 15);
        assert_eq!(lines[0]["y"], 80);
        assert_eq!(lines[0]["button"], "right");
        assert_eq!(lines[1]["kind"], "type");
        assert_eq!(lines[1]["strokes"][0]["keysym"], 72);
        assert_eq!(lines[1]["strokes"][0]["down"], true);
        assert_eq!(lines[1]["strokes"][1]["down"], false);
    }

    #[test]
    fn view_and_listen_are_the_committed_fixtures() {
        let path = std::env::temp_dir().join(format!(
            "hypermesh-visor-fixture-{}-{}.jsonl",
            std::process::id(),
            "media"
        ));
        let desktop = FixtureDesktop::open(&path).unwrap();
        let frame = desktop.view().unwrap();
        assert_eq!(frame.bytes, view_png());
        assert!(frame.bytes.starts_with(b"\x89PNG"));
        let mut audio = desktop.open_audio().unwrap();
        assert_eq!(
            audio.read_chunk().unwrap().as_deref(),
            Some(listen_pcm().as_slice())
        );
        assert!(audio.read_chunk().unwrap().is_none());
        drop(desktop);
        let _ = std::fs::remove_file(&path);
    }
}
