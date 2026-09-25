mod audio;
pub(crate) mod pixels;
mod wayland;
mod x11;

use std::sync::Mutex;

use crate::input::{MouseOp, Stroke};

pub const AUDIO_FORMAT: &str = "s16le;rate=48000;channels=1";
pub const AUDIO_RATE: u32 = 48_000;
pub const AUDIO_CHANNELS: u8 = 1;

pub struct Frame {
    pub mime: String,
    pub bytes: Vec<u8>,
}

impl Frame {
    pub fn png(bytes: Vec<u8>) -> Self {
        Self {
            mime: "image/png".into(),
            bytes,
        }
    }
}

#[derive(Debug)]
pub enum DesktopError {
    RootRefused,
    Unavailable(String),
    AudioUnavailable(String),
    Input(String),
}

impl std::fmt::Display for DesktopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RootRefused => write!(f, "refusing to run as root"),
            Self::Unavailable(message) | Self::AudioUnavailable(message) | Self::Input(message) => {
                write!(f, "{message}")
            }
        }
    }
}

impl std::error::Error for DesktopError {}

pub trait AudioRead: Send {
    fn read_chunk(&mut self) -> Result<Option<Vec<u8>>, DesktopError>;
}

/// What view, watch, listen, mouse, and type call.
/// Version one is Linux. Later desktops implement the same trait.
pub trait Desktop: Send + Sync {
    fn view(&self) -> Result<Frame, DesktopError>;
    fn open_audio(&self) -> Result<Box<dyn AudioRead>, DesktopError>;
    fn mouse(&self, op: &MouseOp) -> Result<(), DesktopError>;
    fn type_input(&self, strokes: &[Stroke]) -> Result<(), DesktopError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendKind {
    X11,
    Wayland,
}

#[derive(Clone, Debug)]
pub struct SessionEnv {
    pub display: Option<String>,
    pub wayland_display: Option<String>,
    pub session_type: Option<String>,
    pub euid: u32,
}

impl SessionEnv {
    pub fn from_process() -> Self {
        Self {
            display: nonempty_env("DISPLAY"),
            wayland_display: nonempty_env("WAYLAND_DISPLAY"),
            session_type: nonempty_env("XDG_SESSION_TYPE"),
            euid: current_euid(),
        }
    }
}

fn nonempty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

pub fn current_euid() -> u32 {
    // geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

pub fn choose_backend(env: &SessionEnv) -> Result<BackendKind, DesktopError> {
    if env.euid == 0 {
        return Err(DesktopError::RootRefused);
    }
    let session = env
        .session_type
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    let wayland = present(&env.wayland_display);
    let x11 = present(&env.display);
    match session.as_str() {
        "wayland" if wayland => Ok(BackendKind::Wayland),
        "x11" if x11 => Ok(BackendKind::X11),
        "wayland" => Err(DesktopError::Unavailable(
            "wayland session has no WAYLAND_DISPLAY".into(),
        )),
        "x11" => Err(DesktopError::Unavailable(
            "x11 session has no DISPLAY".into(),
        )),
        _ if wayland => Ok(BackendKind::Wayland),
        _ if x11 => Ok(BackendKind::X11),
        _ => Err(DesktopError::Unavailable("no desktop session".into())),
    }
}

fn present(value: &Option<String>) -> bool {
    value.as_ref().is_some_and(|item| !item.is_empty())
}

pub struct LinuxDesktop {
    env: SessionEnv,
    wayland_input: Mutex<Option<wayland::WaylandInput>>,
}

impl LinuxDesktop {
    pub fn new(env: SessionEnv) -> Self {
        Self {
            env,
            wayland_input: Mutex::new(None),
        }
    }

    pub fn from_process() -> Self {
        Self::new(SessionEnv::from_process())
    }
}

impl Desktop for LinuxDesktop {
    fn view(&self) -> Result<Frame, DesktopError> {
        match choose_backend(&self.env)? {
            BackendKind::X11 => x11::capture(self.env.display.as_deref()),
            BackendKind::Wayland => wayland::capture(),
        }
    }

    fn open_audio(&self) -> Result<Box<dyn AudioRead>, DesktopError> {
        let _ = choose_backend(&self.env)?;
        Ok(Box::new(audio::open()?))
    }

    fn mouse(&self, op: &MouseOp) -> Result<(), DesktopError> {
        match choose_backend(&self.env)? {
            BackendKind::X11 => x11::mouse(self.env.display.as_deref(), op),
            BackendKind::Wayland => self.with_wayland(|input| input.mouse(op)),
        }
    }

    fn type_input(&self, strokes: &[Stroke]) -> Result<(), DesktopError> {
        match choose_backend(&self.env)? {
            BackendKind::X11 => x11::type_strokes(self.env.display.as_deref(), strokes),
            BackendKind::Wayland => self.with_wayland(|input| input.type_strokes(strokes)),
        }
    }
}

impl LinuxDesktop {
    fn with_wayland<T>(
        &self,
        op: impl FnOnce(&wayland::WaylandInput) -> Result<T, DesktopError>,
    ) -> Result<T, DesktopError> {
        let mut guard = lock(&self.wayland_input);
        if guard.is_none() {
            *guard = Some(wayland::WaylandInput::start()?);
        }
        op(guard.as_ref().expect("wayland input"))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(
        session: Option<&str>,
        display: Option<&str>,
        wayland: Option<&str>,
        euid: u32,
    ) -> SessionEnv {
        SessionEnv {
            display: display.map(str::to_string),
            wayland_display: wayland.map(str::to_string),
            session_type: session.map(str::to_string),
            euid,
        }
    }

    #[test]
    fn root_is_refused() {
        let err = choose_backend(&env(Some("x11"), Some(":0"), None, 0)).unwrap_err();
        assert!(matches!(err, DesktopError::RootRefused));
        assert_eq!(err.to_string(), "refusing to run as root");
    }

    #[test]
    fn selects_the_logged_in_session() {
        assert_eq!(
            choose_backend(&env(Some("x11"), Some(":0"), Some("wayland-0"), 1000)).unwrap(),
            BackendKind::X11
        );
        assert_eq!(
            choose_backend(&env(Some("wayland"), Some(":0"), Some("wayland-0"), 1000)).unwrap(),
            BackendKind::Wayland
        );
        assert_eq!(
            choose_backend(&env(None, Some(":0"), Some("wayland-0"), 1000)).unwrap(),
            BackendKind::Wayland
        );
        assert_eq!(
            choose_backend(&env(None, Some(":0"), None, 1000)).unwrap(),
            BackendKind::X11
        );
        assert_eq!(
            choose_backend(&env(None, None, None, 1000))
                .unwrap_err()
                .to_string(),
            "no desktop session"
        );
        assert_eq!(
            choose_backend(&env(Some("x11"), None, Some("wayland-0"), 1000))
                .unwrap_err()
                .to_string(),
            "x11 session has no DISPLAY"
        );
    }
}
