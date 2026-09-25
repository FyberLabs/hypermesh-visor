use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::protocol::xproto::{ImageFormat, ImageOrder};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use crate::desktop::pixels::{encode_rgba_png, zpixmap_to_rgba};
use crate::desktop::{DesktopError, Frame};
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

pub fn mouse(display: Option<&str>, op: &MouseOp) -> Result<(), DesktopError> {
    let (conn, screen_num) = connect(display)?;
    ensure_xtest(&conn)?;
    let root = conn.setup().roots[screen_num].root;
    match *op {
        MouseOp::Move { x, y } => move_abs(&conn, root, x, y)?,
        MouseOp::Click { x, y, button } => {
            move_abs(&conn, root, x, y)?;
            button_event(&conn, root, button, true)?;
            button_event(&conn, root, button, false)?;
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
            move_abs(&conn, root, to_x, to_y)?;
            button_event(&conn, root, button, false)?;
        }
    }
    conn.flush().map_err(input_err)?;
    Ok(())
}

pub fn type_strokes(display: Option<&str>, strokes: &[Stroke]) -> Result<(), DesktopError> {
    let (conn, _) = connect(display)?;
    ensure_xtest(&conn)?;
    let map = load_map(&conn)?;
    let shift = find_keycode(&map, XK_SHIFT_L).map(|(code, _)| code);
    let mut planned = Vec::with_capacity(strokes.len());
    for stroke in strokes {
        let (keycode, needs_shift) = find_keycode(&map, stroke.keysym).ok_or_else(|| {
            DesktopError::Input("the session keymap has no keycode for a character".into())
        })?;
        planned.push((keycode, needs_shift, stroke.down));
    }
    for (keycode, needs_shift, down) in planned {
        if needs_shift {
            let shift = shift
                .ok_or_else(|| DesktopError::Input("the session keymap has no shift key".into()))?;
            if down {
                key_event(&conn, shift, true)?;
                key_event(&conn, keycode, true)?;
            } else {
                key_event(&conn, keycode, false)?;
                key_event(&conn, shift, false)?;
            }
        } else {
            key_event(&conn, keycode, down)?;
        }
    }
    conn.flush().map_err(input_err)?;
    Ok(())
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
}
