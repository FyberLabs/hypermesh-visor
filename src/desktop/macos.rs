//! Native macOS desktop bridge. The bundled helper uses public Apple APIs.
use super::*;
use crate::input::{Button, MouseOp, PointerStep, Stroke};
use base64::Engine;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub struct MacDesktop;
struct Bridge {
    child: Child,
    input: Option<ChildStdin>,
    output: ChildStdout,
}
fn unavailable(_: impl std::fmt::Display) -> DesktopError {
    DesktopError::Unavailable("native macOS desktop bridge unavailable".into())
}
impl Bridge {
    fn open() -> Result<Self, DesktopError> {
        let path = std::env::current_exe()
            .map_err(unavailable)?
            .with_file_name("hypermesh-native-desktop");
        let mut child = Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(unavailable)?;
        let input = child.stdin.take();
        let output = child.stdout.take().ok_or_else(|| unavailable("stdout"))?;
        let flags = unsafe { libc::fcntl(output.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(unavailable("nonblocking output"));
        }
        Ok(Self {
            child,
            input,
            output,
        })
    }
    fn call(&mut self, request: Value) -> Result<Value, DesktopError> {
        let input = self.input.as_mut().ok_or_else(|| unavailable("stdin"))?;
        serde_json::to_writer(&mut *input, &request).map_err(unavailable)?;
        input.write_all(b"\n").map_err(unavailable)?;
        input.flush().map_err(unavailable)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut reply = Vec::new();
        let mut buffer = [0u8; 65536];
        loop {
            if std::time::Instant::now() >= deadline {
                return Err(unavailable("reply timeout"));
            }
            match self.output.read(&mut buffer) {
                Ok(0) => return Err(unavailable("helper closed output")),
                Ok(n) => {
                    if reply.len() + n > 48 * 1024 * 1024 {
                        return Err(unavailable("reply size"));
                    }
                    reply.extend_from_slice(&buffer[..n]);
                    if reply.contains(&b'\n') {
                        if reply.last() != Some(&b'\n') {
                            return Err(unavailable("reply framing"));
                        }
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(unavailable(error)),
            }
        }
        let value: Value = serde_json::from_slice(&reply).map_err(unavailable)?;
        if value.get("error").is_some() {
            return Err(DesktopError::Unavailable(
                "macOS permission denied or native operation unavailable; check companion setup"
                    .into(),
            ));
        }
        value
            .get("result")
            .cloned()
            .ok_or_else(|| unavailable("result"))
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.input.take(); // EOF lets the native bridge release its held inputs.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < until {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn button(button: Button) -> u8 {
    match button {
        Button::Left => 0,
        Button::Right => 1,
        Button::Middle => 2,
    }
}
impl PointerPipe for Bridge {
    fn send(&mut self, steps: &[PointerStep]) -> Result<(), DesktopError> {
        let steps: Vec<Value> = steps
            .iter()
            .map(|step| match step {
                PointerStep::Move { x, y } => json!({"kind":"move","x":x,"y":y}),
                PointerStep::Scroll { direction } => json!({"kind":"scroll","direction":direction}),
                PointerStep::Button { button: b, down } => {
                    json!({"kind":"button","button":button(*b),"down":down})
                }
            })
            .collect();
        self.call(json!({"action":"pointer","steps":steps}))
            .map(|_| ())
    }
}
impl KeyboardPipe for Bridge {
    fn send(&mut self, strokes: &[Stroke]) -> Result<(), DesktopError> {
        let strokes: Vec<Value> = strokes
            .iter()
            .map(|s| json!({"keysym":s.keysym,"down":s.down}))
            .collect();
        self.call(json!({"action":"keyboard","strokes":strokes}))
            .map(|_| ())
    }
}
impl Desktop for MacDesktop {
    fn view(&self) -> Result<Frame, DesktopError> {
        let value = Bridge::open()?.call(json!({"action":"view"}))?;
        let png = value["png"].as_str().ok_or_else(|| unavailable("image"))?;
        Ok(Frame::png(
            base64::engine::general_purpose::STANDARD
                .decode(png)
                .map_err(unavailable)?,
        ))
    }
    fn open_audio(&self) -> Result<Box<dyn AudioRead>, DesktopError> {
        Err(DesktopError::AudioUnavailable(
            "macOS audio capture is not yet available".into(),
        ))
    }
    fn mouse(&self, op: &MouseOp) -> Result<(), DesktopError> {
        use PointerStep::*;
        let steps = match *op {
            MouseOp::Move { x, y, .. } => vec![Move { x, y }],
            MouseOp::Click { x, y, button } => vec![
                Move { x, y },
                Button { button, down: true },
                Button {
                    button,
                    down: false,
                },
            ],
            MouseOp::Drag {
                x,
                y,
                to_x,
                to_y,
                button,
            } => vec![
                Move { x, y },
                Button { button, down: true },
                Move { x: to_x, y: to_y },
                Button {
                    button,
                    down: false,
                },
            ],
            MouseOp::Scroll {
                x,
                y,
                delta_x,
                delta_y,
            } => crate::input::scroll_steps(x, y, delta_x, delta_y)
                .ok_or_else(|| unavailable("scroll"))?,
        };
        PointerPipe::send(&mut Bridge::open()?, &steps)
    }
    fn type_input(&self, strokes: &[Stroke]) -> Result<(), DesktopError> {
        KeyboardPipe::send(&mut Bridge::open()?, strokes)
    }
    fn pointer_steps(&self, steps: &[PointerStep]) -> Result<(), DesktopError> {
        PointerPipe::send(&mut Bridge::open()?, steps)
    }
    fn open_pointer(&self) -> Result<Box<dyn PointerPipe + '_>, DesktopError> {
        Ok(Box::new(Bridge::open()?))
    }
    fn open_keyboard(&self) -> Result<Box<dyn KeyboardPipe + '_>, DesktopError> {
        Ok(Box::new(Bridge::open()?))
    }
    fn root_size(&self) -> Result<Option<(u16, u16)>, DesktopError> {
        let info = Bridge::open()?.call(json!({"action":"info"}))?;
        let dimension = |name| {
            info[name]
                .as_u64()
                .and_then(|n| u16::try_from(n).ok())
                .filter(|n| *n > 0)
                .ok_or_else(|| unavailable("size"))
        };
        Ok(Some((dimension("width")?, dimension("height")?)))
    }
    fn pointer_position(&self) -> Result<Option<(i32, i32)>, DesktopError> {
        let info = Bridge::open()?.call(json!({"action":"info"}))?;
        let position = |name| {
            info[name]
                .as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .ok_or_else(|| unavailable("point"))
        };
        Ok(Some((position("x")?, position("y")?)))
    }
    fn focused_app(&self) -> Result<Option<crate::mcp::FocusedApp>, DesktopError> {
        let info = Bridge::open()?.call(json!({"action":"info"}))?;
        let executable = info["pid"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .and_then(super::focus::process_executable);
        if executable.is_none() {
            return Ok(None);
        }
        Ok(Some(crate::mcp::FocusedApp {
            wm_class: None,
            app_id: info["app_id"].as_str().map(str::to_string),
            executable,
        }))
    }
    fn backend_name(&self) -> &'static str {
        "macos"
    }
}
