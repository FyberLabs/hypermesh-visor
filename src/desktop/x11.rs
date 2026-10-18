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

/// Best-effort focused window WM_CLASS for MCP bindings.
pub fn focused_wm_class(display: Option<&str>) -> Result<Option<String>, DesktopError> {
    let (conn, screen_num) = connect(display)?;
    let root = conn.setup().roots[screen_num].root;
    let atom_active = intern(&conn, "_NET_ACTIVE_WINDOW")?;
    let atom_wm_class = intern(&conn, "WM_CLASS")?;
    let reply = conn
        .get_property(
            false,
            root,
            atom_active,
            x11rb::protocol::xproto::AtomEnum::WINDOW,
            0,
            1,
        )
        .map_err(unavailable)?
        .reply()
        .map_err(unavailable)?;
    if reply.value.len() < 4 {
        return Ok(None);
    }
    let window = u32::from_ne_bytes([
        reply.value[0],
        reply.value[1],
        reply.value[2],
        reply.value[3],
    ]);
    if window == 0 {
        return Ok(None);
    }
    let class = conn
        .get_property(
            false,
            window,
            atom_wm_class,
            x11rb::protocol::xproto::AtomEnum::STRING,
            0,
            256,
        )
        .map_err(unavailable)?
        .reply()
        .map_err(unavailable)?;
    let raw = String::from_utf8_lossy(&class.value);
    // WM_CLASS is instance\0class\0 — prefer the class token.
    let parts: Vec<&str> = raw.split('\0').filter(|s| !s.is_empty()).collect();
    let name = parts.last().copied().or_else(|| parts.first().copied());
    Ok(name.map(str::to_string))
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
