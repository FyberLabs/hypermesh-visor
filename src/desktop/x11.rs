use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::protocol::xproto::{ImageFormat, ImageOrder};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use crate::desktop::pixels::{encode_rgba_png, zpixmap_to_rgba};
use crate::desktop::{DesktopError, Frame, KeyboardPipe, PointerPipe};
use crate::input::{Button, MouseOp, Stroke};

const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
const BUTTON_PRESS: u8 = 4;
const BUTTON_RELEASE: u8 = 5;
const MOTION_NOTIFY: u8 = 6;
const XK_SHIFT_L: u32 = 0xffe1;

pub(crate) struct KeyMap {
    min: u8,
    per: usize,
    keysyms: Vec<u32>,
}

pub(crate) fn find_keycode(map: &KeyMap, keysym: u32) -> Option<(u8, bool)> {
    if map.per == 0 || keysym == 0 {
        return None;
    }
    for (index, chunk) in map.keysyms.chunks(map.per).enumerate() {
        if chunk.first().copied() == Some(keysym) {
            return Some((map.min.wrapping_add(index as u8), false));
        }
        if chunk.get(1).copied() == Some(keysym) {
            return Some((map.min.wrapping_add(index as u8), true));
        }
    }
    None
}

/// X11 properties are routing hints, not authenticated application identity.
pub fn focused_app(display: Option<&str>) -> Result<Option<crate::mcp::FocusedApp>, DesktopError> {
    use x11rb::protocol::xproto::AtomEnum;
    let (conn, screen_num) = connect(display)?;
    let root = conn.setup().roots[screen_num].root;
    let active = intern(&conn, "_NET_ACTIVE_WINDOW")?;
    let get = |window, atom, kind, length| {
        conn.get_property(false, window, atom, kind, 0, length)
            .map_err(unavailable)?
            .reply()
            .map_err(unavailable)
    };
    let Some(window) = property_u32(&get(root, active, AtomEnum::WINDOW, 1)?, AtomEnum::WINDOW)
    else {
        return Ok(None);
    };
    let class = get(window, intern(&conn, "WM_CLASS")?, AtomEnum::STRING, 256)?;
    let wm_class = property_text(&class).and_then(|raw| {
        raw.split('\0')
            .filter(|s| !s.trim().is_empty())
            .last()
            .map(str::to_string)
    });
    // A remote X client can report a PID in another machine's namespace. Never
    // resolve it locally without both a local display and an exact host match.
    let executable = if local_display(display) {
        let pid = get(window, intern(&conn, "_NET_WM_PID")?, AtomEnum::CARDINAL, 1).ok();
        let machine = get(
            window,
            intern(&conn, "WM_CLIENT_MACHINE")?,
            AtomEnum::STRING,
            256,
        )
        .ok();
        let hostname = local_hostname();
        local_executable_hint(
            pid.as_ref(),
            machine.as_ref(),
            hostname.as_deref(),
            super::focus::process_executable,
        )
    } else {
        None
    };
    // Do not publish an identity assembled while the active window changed.
    if property_u32(&get(root, active, AtomEnum::WINDOW, 1)?, AtomEnum::WINDOW) != Some(window) {
        return Ok(None);
    }
    let app = crate::mcp::FocusedApp {
        wm_class,
        app_id: None,
        executable,
    };
    Ok((!app.is_empty()).then_some(app))
}

fn property_u32(
    reply: &x11rb::protocol::xproto::GetPropertyReply,
    kind: x11rb::protocol::xproto::AtomEnum,
) -> Option<u32> {
    if reply.type_ != u32::from(kind)
        || reply.format != 32
        || reply.bytes_after != 0
        || reply.value.len() != 4
    {
        return None;
    }
    let value = u32::from_ne_bytes(reply.value.as_slice().try_into().ok()?);
    (value > 0).then_some(value)
}

fn property_text(reply: &x11rb::protocol::xproto::GetPropertyReply) -> Option<&str> {
    if reply.type_ != u32::from(x11rb::protocol::xproto::AtomEnum::STRING)
        || reply.format != 8
        || reply.bytes_after != 0
    {
        return None;
    }
    std::str::from_utf8(&reply.value).ok()
}

fn local_executable_hint(
    pid: Option<&x11rb::protocol::xproto::GetPropertyReply>,
    machine: Option<&x11rb::protocol::xproto::GetPropertyReply>,
    hostname: Option<&str>,
    resolve: impl FnOnce(u32) -> Option<String>,
) -> Option<String> {
    let machine = property_text(machine?)?;
    let machine = machine.strip_suffix('\0').unwrap_or(machine);
    let hostname = hostname.filter(|s| !s.is_empty())?;
    if machine != hostname {
        return None;
    }
    resolve(property_u32(
        pid?,
        x11rb::protocol::xproto::AtomEnum::CARDINAL,
    )?)
}

fn local_display(display: Option<&str>) -> bool {
    let env = std::env::var("DISPLAY").ok();
    let Some(display) = display.or(env.as_deref()) else {
        return false;
    };
    // Conservative Unix-socket forms only; no TCP, SSH forwarding, or DNS.
    let Some(number) = display
        .strip_prefix(':')
        .or_else(|| display.strip_prefix("unix:"))
    else {
        return false;
    };
    !number.is_empty()
        && number.split('.').count() <= 2
        && number
            .split('.')
            .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
}

fn local_hostname() -> Option<String> {
    let mut bytes = [0u8; 256];
    if unsafe { libc::gethostname(bytes.as_mut_ptr().cast(), bytes.len()) } != 0 {
        return None;
    }
    let end = bytes.iter().position(|b| *b == 0)?;
    std::str::from_utf8(&bytes[..end])
        .ok()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn intern(
    conn: &RustConnection,
    name: &str,
) -> Result<x11rb::protocol::xproto::Atom, DesktopError> {
    Ok(conn
        .intern_atom(false, name.as_bytes())
        .map_err(unavailable)?
        .reply()
        .map_err(unavailable)?
        .atom)
}

pub fn capture(display: Option<&str>) -> Result<Frame, DesktopError> {
    let (conn, screen_num) = connect(display)?;
    let screen = &conn.setup().roots[screen_num];
    let width = screen.width_in_pixels;
    let height = screen.height_in_pixels;
    if width == 0 || height == 0 {
        return Err(DesktopError::Unavailable("x11 screen is empty".into()));
    }
    let reply = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            screen.root,
            0,
            0,
            width,
            height,
            !0u32,
        )
        .map_err(unavailable)?
        .reply()
        .map_err(unavailable)?;
    let (red, green, blue) = visual_masks(screen, screen.root_visual)?;
    let bpp = bytes_per_pixel(conn.setup(), reply.depth)?;
    let lsb = conn.setup().image_byte_order == ImageOrder::LSB_FIRST;
    let rgba = zpixmap_to_rgba(
        &reply.data,
        width as u32,
        height as u32,
        bpp,
        lsb,
        red,
        green,
        blue,
    )?;
    let bytes = encode_rgba_png(width as u32, height as u32, &rgba)?;
    Ok(Frame::png(bytes))
}

pub fn pointer(display: Option<&str>) -> Result<(i32, i32), DesktopError> {
    let (conn, screen_num) = connect(display)?;
    let root = conn.setup().roots[screen_num].root;
    let reply = conn
        .query_pointer(root)
        .map_err(unavailable)?
        .reply()
        .map_err(unavailable)?;
    Ok((i32::from(reply.root_x), i32::from(reply.root_y)))
}

pub fn mouse(display: Option<&str>, op: &MouseOp) -> Result<(), DesktopError> {
    let (conn, screen_num) = connect(display)?;
    ensure_xtest(&conn)?;
    let root = conn.setup().roots[screen_num].root;
    let pressed = match *op {
        MouseOp::Scroll {
            x,
            y,
            delta_x,
            delta_y,
        } => {
            let steps = crate::input::scroll_steps(x, y, delta_x, delta_y).ok_or_else(|| {
                DesktopError::Input("scroll must contain 1 to 128 total ticks".into())
            })?;
            return pointer_steps(display, &steps);
        }
        MouseOp::Move { x, y, .. } => {
            move_abs(&conn, root, x, y)?;
            None
        }
        MouseOp::Click { x, y, button } => {
            move_abs(&conn, root, x, y)?;
            button_event(&conn, root, button, true)?;
            if let Err(err) = button_event(&conn, root, button, false) {
                release_pressed(&conn, root, button);
                return Err(err);
            }
            Some(button)
        }
        MouseOp::Drag {
            x,
            y,
            to_x,
            to_y,
            button,
        } => {
            move_abs(&conn, root, x, y)?;
            button_event(&conn, root, button, true)?;
            let moved = move_abs(&conn, root, to_x, to_y)
                .and_then(|_| button_event(&conn, root, button, false));
            if let Err(err) = moved {
                release_pressed(&conn, root, button);
                return Err(err);
            }
            Some(button)
        }
    };
    if let Err(err) = conn.flush().map_err(input_err) {
        if let Some(button) = pressed {
            release_pressed(&conn, root, button);
        }
        return Err(err);
    }
    Ok(())
}

/// The press was accepted. Queue a release even when the matching up already
/// failed, then flush. A second release of a button that is up is harmless.
fn release_pressed(conn: &RustConnection, root: u32, button: Button) {
    let _ = button_event(conn, root, button, false);
    let _ = conn.flush().map_err(input_err);
}

pub fn pointer_steps(
    display: Option<&str>,
    steps: &[crate::input::PointerStep],
) -> Result<(), DesktopError> {
    XPointer::open(display)?.send(steps)
}

/// One X connection and one XTEST setup for every batch of a drag.
pub struct XPointer {
    conn: RustConnection,
    root: u32,
}

impl XPointer {
    pub fn open(display: Option<&str>) -> Result<Self, DesktopError> {
        let (conn, screen_num) = connect(display)?;
        ensure_xtest(&conn)?;
        let root = conn.setup().roots[screen_num].root;
        Ok(Self { conn, root })
    }
}

impl PointerPipe for XPointer {
    fn send(&mut self, steps: &[crate::input::PointerStep]) -> Result<(), DesktopError> {
        for (index, step) in steps.iter().enumerate() {
            if let Err(err) = apply_step(&self.conn, self.root, *step) {
                for button in unreleased_buttons(&steps[..index]).into_iter().rev() {
                    let _ = button_event(&self.conn, self.root, button, false);
                }
                let _ = self.conn.flush().map_err(input_err);
                return Err(err);
            }
        }
        self.conn.flush().map_err(input_err)?;
        Ok(())
    }
}

fn apply_step(
    conn: &RustConnection,
    root: u32,
    step: crate::input::PointerStep,
) -> Result<(), DesktopError> {
    match step {
        crate::input::PointerStep::Move { x, y } => move_abs(conn, root, x, y),
        crate::input::PointerStep::Scroll { direction } => wheel_tick(conn, root, direction),
        crate::input::PointerStep::Button { button, down } => {
            button_event(conn, root, button, down)
        }
    }
}

/// Buttons pressed in `steps` that this slice did not release.
pub(crate) fn unreleased_buttons(steps: &[crate::input::PointerStep]) -> Vec<Button> {
    let mut held = Vec::new();
    for step in steps {
        let crate::input::PointerStep::Button { button, down } = *step else {
            continue;
        };
        if down {
            held.push(button);
            continue;
        }
        if let Some(index) = held.iter().rposition(|pressed| *pressed == button) {
            held.remove(index);
        }
    }
    held
}

pub fn root_size(display: Option<&str>) -> Result<(u16, u16), DesktopError> {
    let (conn, screen_num) = connect(display)?;
    let screen = &conn.setup().roots[screen_num];
    Ok((screen.width_in_pixels, screen.height_in_pixels))
}

/// One X connection and one keymap for every chunk of a type request.
pub struct XKeyboard {
    conn: RustConnection,
    map: KeyMap,
    shift: Option<u8>,
}

impl XKeyboard {
    pub fn open(display: Option<&str>) -> Result<Self, DesktopError> {
        let (conn, _) = connect(display)?;
        ensure_xtest(&conn)?;
        let map = load_map(&conn)?;
        let shift = find_keycode(&map, XK_SHIFT_L).map(|(code, _)| code);
        Ok(Self { conn, map, shift })
    }
}

impl KeyboardPipe for XKeyboard {
    fn presses(&self, strokes: &[Stroke]) -> Vec<u32> {
        planned_presses(&self.map, strokes)
    }

    fn send(&mut self, strokes: &[Stroke]) -> Result<(), DesktopError> {
        let mut planned = Vec::with_capacity(strokes.len());
        for stroke in strokes {
            let (keycode, needs_shift) =
                find_keycode(&self.map, stroke.keysym).ok_or_else(|| {
                    DesktopError::Input("the session keymap has no keycode for a character".into())
                })?;
            planned.push((keycode, needs_shift, stroke.down));
        }
        let events = expand_key_events(&planned, self.shift)
            .map_err(|_| DesktopError::Input("the session keymap has no shift key".into()))?;
        let mut sent = Vec::with_capacity(events.len());
        for event in events {
            if let Err(err) = key_event(&self.conn, event.keycode, event.down) {
                self.release_keycodes(&unreleased_keycodes(&sent));
                return Err(err);
            }
            sent.push(event);
        }
        if let Err(err) = self.conn.flush().map_err(input_err) {
            self.release_keycodes(&unreleased_keycodes(&sent));
            return Err(err);
        }
        Ok(())
    }
}

impl XKeyboard {
    fn release_keycodes(&mut self, codes: &[u8]) {
        for code in codes.iter().rev() {
            let _ = key_event(&self.conn, *code, false);
        }
        let _ = self.conn.flush().map_err(input_err);
    }
}

/// Downs this chunk will press, with Shift before a keysym that needs it.
pub(crate) fn planned_presses(map: &KeyMap, strokes: &[Stroke]) -> Vec<u32> {
    let mut downs = Vec::new();
    for stroke in strokes {
        if !stroke.down {
            continue;
        }
        let needs_shift = find_keycode(map, stroke.keysym)
            .map(|(_, shift)| shift)
            .unwrap_or(false);
        if needs_shift {
            downs.push(XK_SHIFT_L);
        }
        downs.push(stroke.keysym);
    }
    downs
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KeyEvent {
    pub keycode: u8,
    pub down: bool,
}

/// `planned` entries are `(keycode, needs_shift, down)`.
pub(crate) fn expand_key_events(
    planned: &[(u8, bool, bool)],
    shift: Option<u8>,
) -> Result<Vec<KeyEvent>, ()> {
    let mut events = Vec::new();
    for &(keycode, needs_shift, down) in planned {
        if needs_shift {
            let shift = shift.ok_or(())?;
            if down {
                events.push(KeyEvent {
                    keycode: shift,
                    down: true,
                });
                events.push(KeyEvent {
                    keycode,
                    down: true,
                });
            } else {
                events.push(KeyEvent {
                    keycode,
                    down: false,
                });
                events.push(KeyEvent {
                    keycode: shift,
                    down: false,
                });
            }
        } else {
            events.push(KeyEvent { keycode, down });
        }
    }
    Ok(events)
}

/// Keycodes pressed in `events` that this slice did not release.
pub(crate) fn unreleased_keycodes(events: &[KeyEvent]) -> Vec<u8> {
    let mut held = Vec::new();
    for event in events {
        if event.down {
            held.push(event.keycode);
            continue;
        }
        if let Some(index) = held.iter().rposition(|code| *code == event.keycode) {
            held.remove(index);
        }
    }
    held
}

pub fn type_strokes(display: Option<&str>, strokes: &[Stroke]) -> Result<(), DesktopError> {
    let mut keyboard = XKeyboard::open(display)?;
    KeyboardPipe::send(&mut keyboard, strokes)
}

fn connect(display: Option<&str>) -> Result<(RustConnection, usize), DesktopError> {
    x11rb::connect(display).map_err(unavailable)
}

fn ensure_xtest(conn: &RustConnection) -> Result<(), DesktopError> {
    let reply = conn
        .query_extension(b"XTEST")
        .map_err(input_err)?
        .reply()
        .map_err(input_err)?;
    if !reply.present {
        return Err(DesktopError::Input("XTEST extension is not present".into()));
    }
    Ok(())
}

fn load_map(conn: &RustConnection) -> Result<KeyMap, DesktopError> {
    let setup = conn.setup();
    let min = setup.min_keycode;
    let count = setup.max_keycode - min + 1;
    let reply = conn
        .get_keyboard_mapping(min, count)
        .map_err(input_err)?
        .reply()
        .map_err(input_err)?;
    Ok(KeyMap {
        min,
        per: reply.keysyms_per_keycode as usize,
        keysyms: reply.keysyms,
    })
}

fn move_abs(conn: &RustConnection, root: u32, x: i32, y: i32) -> Result<(), DesktopError> {
    fake(conn, MOTION_NOTIFY, 0, root, i16_coord(x)?, i16_coord(y)?)
}

// Each tick is a complete press/release before the next cancellation boundary.
// Always attempt the release, even if the press failed after reaching Xorg.
fn wheel_tick(
    conn: &RustConnection,
    root: u32,
    direction: crate::input::ScrollDirection,
) -> Result<(), DesktopError> {
    use crate::input::ScrollDirection::*;
    let detail = match direction {
        Up => 4,
        Down => 5,
        Left => 6,
        Right => 7,
    };
    let pressed = fake(conn, BUTTON_PRESS, detail, root, 0, 0);
    let released = fake(conn, BUTTON_RELEASE, detail, root, 0, 0);
    if pressed.is_err() || released.is_err() {
        let _ = fake(conn, BUTTON_RELEASE, detail, root, 0, 0);
        let _ = conn.flush();
    }
    pressed.and(released)
}

fn button_event(
    conn: &RustConnection,
    root: u32,
    button: Button,
    down: bool,
) -> Result<(), DesktopError> {
    let kind = if down { BUTTON_PRESS } else { BUTTON_RELEASE };
    fake(conn, kind, x11_button(button), root, 0, 0)
}

fn key_event(conn: &RustConnection, keycode: u8, down: bool) -> Result<(), DesktopError> {
    let kind = if down { KEY_PRESS } else { KEY_RELEASE };
    fake(conn, kind, keycode, x11rb::NONE, 0, 0)
}

fn fake(
    conn: &RustConnection,
    kind: u8,
    detail: u8,
    root: u32,
    x: i16,
    y: i16,
) -> Result<(), DesktopError> {
    conn.xtest_fake_input(kind, detail, 0, root, x, y, 0)
        .map_err(input_err)?
        .check()
        .map_err(input_err)
}

fn x11_button(button: Button) -> u8 {
    match button {
        Button::Left => 1,
        Button::Middle => 2,
        Button::Right => 3,
    }
}

fn i16_coord(value: i32) -> Result<i16, DesktopError> {
    i16::try_from(value)
        .map_err(|_| DesktopError::Input("pointer coordinate is out of range".into()))
}

fn visual_masks(
    screen: &x11rb::protocol::xproto::Screen,
    visual: u32,
) -> Result<(u32, u32, u32), DesktopError> {
    for depth in &screen.allowed_depths {
        for entry in &depth.visuals {
            if entry.visual_id == visual {
                return Ok((entry.red_mask, entry.green_mask, entry.blue_mask));
            }
        }
    }
    Err(DesktopError::Unavailable(
        "x11 visual masks are missing".into(),
    ))
}

fn bytes_per_pixel(
    setup: &x11rb::protocol::xproto::Setup,
    depth: u8,
) -> Result<usize, DesktopError> {
    setup
        .pixmap_formats
        .iter()
        .find(|format| format.depth == depth)
        .map(|format| (format.bits_per_pixel / 8) as usize)
        .filter(|bpp| *bpp > 0)
        .ok_or_else(|| DesktopError::Unavailable(format!("no pixmap format for depth {depth}")))
}

fn unavailable(err: impl std::fmt::Display) -> DesktopError {
    DesktopError::Unavailable(err.to_string())
}

fn input_err(err: impl std::fmt::Display) -> DesktopError {
    DesktopError::Input(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn property(
        kind: x11rb::protocol::xproto::AtomEnum,
        format: u8,
        value: Vec<u8>,
    ) -> x11rb::protocol::xproto::GetPropertyReply {
        x11rb::protocol::xproto::GetPropertyReply {
            format,
            sequence: 0,
            length: 0,
            type_: kind.into(),
            bytes_after: 0,
            value_len: (value.len() * 8 / format as usize) as u32,
            value,
        }
    }

    #[test]
    fn x11_executable_requires_local_host_and_valid_pid_properties() {
        use x11rb::protocol::xproto::AtomEnum;
        let pid = property(AtomEnum::CARDINAL, 32, 42u32.to_ne_bytes().to_vec());
        let machine = property(AtomEnum::STRING, 8, b"workstation\0".to_vec());
        assert_eq!(
            local_executable_hint(Some(&pid), Some(&machine), Some("workstation"), |p| {
                assert_eq!(p, 42);
                Some("/opt/editor".into())
            })
            .as_deref(),
            Some("/opt/editor")
        );
        for host in [None, Some(""), Some("remote"), Some("workstation.example")] {
            assert!(
                local_executable_hint(Some(&pid), Some(&machine), host, |_| panic!(
                    "foreign host probed"
                ))
                .is_none()
            );
        }
        assert!(
            local_executable_hint(None, Some(&machine), Some("workstation"), |_| panic!(
                "missing pid"
            ))
            .is_none()
        );
        assert!(
            local_executable_hint(Some(&pid), None, Some("workstation"), |_| panic!(
                "missing machine"
            ))
            .is_none()
        );
        assert!(
            local_executable_hint(Some(&pid), Some(&machine), Some("workstation"), |_| None)
                .is_none()
        );
        for invalid in [
            property(AtomEnum::STRING, 32, 42u32.to_ne_bytes().to_vec()),
            property(AtomEnum::CARDINAL, 8, vec![42]),
            property(AtomEnum::CARDINAL, 32, vec![0; 4]),
            property(AtomEnum::CARDINAL, 32, vec![1; 8]),
        ] {
            assert!(local_executable_hint(
                Some(&invalid),
                Some(&machine),
                Some("workstation"),
                |_| panic!("invalid pid")
            )
            .is_none());
        }
        let mut truncated = pid.clone();
        truncated.bytes_after = 4;
        assert!(property_u32(&truncated, AtomEnum::CARDINAL).is_none());
        let mut truncated = machine.clone();
        truncated.bytes_after = 1;
        assert!(property_text(&truncated).is_none());
        assert!(property_text(&property(AtomEnum::STRING, 8, vec![255])).is_none());
        assert!(property_text(&property(AtomEnum::CARDINAL, 32, vec![0; 4])).is_none());
    }

    #[test]
    fn executable_hints_never_resolve_tcp_or_forwarded_displays() {
        for display in [":0", ":12.1", "unix:0"] {
            assert!(local_display(Some(display)));
        }
        for display in [
            "localhost:10.0",
            "127.0.0.1:0",
            "remote:0",
            "tcp/host:0",
            ":",
            ":0.bad",
            ":0.1.2",
        ] {
            assert!(!local_display(Some(display)));
        }
    }

    #[test]
    fn x11_process_restart_resolves_the_new_pid_not_a_cached_path() {
        use x11rb::protocol::xproto::AtomEnum;
        let machine = property(AtomEnum::STRING, 8, b"local".to_vec());
        for (pid, path) in [(42u32, "/opt/old"), (43u32, "/opt/new")] {
            let prop = property(AtomEnum::CARDINAL, 32, pid.to_ne_bytes().to_vec());
            assert_eq!(
                local_executable_hint(Some(&prop), Some(&machine), Some("local"), |p| {
                    assert_eq!(p, pid);
                    Some(path.into())
                })
                .as_deref(),
                Some(path)
            );
        }
    }

    #[test]
    fn finds_shifted_and_plain_keycodes() {
        let map = KeyMap {
            min: 38,
            per: 2,
            keysyms: vec![0x61, 0x41, 0x20, 0x20],
        };
        assert_eq!(find_keycode(&map, 0x41), Some((38, true)));
        assert_eq!(find_keycode(&map, 0x20), Some((39, false)));
        assert_eq!(find_keycode(&map, 0x62), None);
    }

    #[test]
    fn a_failed_batch_remembers_the_button_it_pressed() {
        use crate::input::PointerStep;
        let steps = [
            PointerStep::Move { x: 1, y: 2 },
            PointerStep::Button {
                button: Button::Left,
                down: true,
            },
            PointerStep::Move { x: 3, y: 2 },
        ];
        assert_eq!(unreleased_buttons(&steps), vec![Button::Left]);
        let mut released = steps.to_vec();
        released.push(PointerStep::Button {
            button: Button::Left,
            down: false,
        });
        assert!(unreleased_buttons(&released).is_empty());
    }

    #[test]
    fn a_shifted_press_keeps_shift_until_the_matching_up() {
        let map = KeyMap {
            min: 38,
            per: 2,
            keysyms: vec![0x61, 0x41],
        };
        let strokes = [
            Stroke {
                keysym: 0x41,
                down: true,
            },
            Stroke {
                keysym: 0x41,
                down: false,
            },
        ];
        assert_eq!(planned_presses(&map, &strokes), vec![XK_SHIFT_L, 0x41]);
        let events = expand_key_events(&[(38, true, true), (38, true, false)], Some(50)).unwrap();
        assert_eq!(
            unreleased_keycodes(&events[..1]),
            vec![50],
            "shift stays down when the key press has not been sent"
        );
        assert_eq!(unreleased_keycodes(&events[..2]), vec![50, 38]);
        assert!(unreleased_keycodes(&events).is_empty());
        assert!(expand_key_events(&[(38, true, true)], None).is_err());
    }
}
