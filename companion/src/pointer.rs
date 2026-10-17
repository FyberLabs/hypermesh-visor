//! X11 pointer replacement and real-versus-synthetic input.
//!
//! The bug cursor is an ARGB picture turned into a core cursor with the Render
//! extension. The click point is the hotspot from `hotspot.json`. Raw XInput2
//! events whose source is an XTEST device are the visor's own injected input
//! and are ignored. Other raw motion, button presses, and key presses are the
//! user.
//!
//! If XInput2 or the XTEST devices cannot be identified, the companion falls
//! back to comparing the pointer with the last drive coordinate after the move
//! has settled. That fallback cannot see key presses.

use x11rb::connection::Connection;
use x11rb::protocol::render::{self, ConnectionExt as _, PictType};
use x11rb::protocol::xinput::{self, ConnectionExt as _, XIEventMask};
use x11rb::protocol::xproto::{self, ConnectionExt as _, ImageFormat, ImageOrder};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use crate::automation::{self, CursorPose};
use crate::draw;

const CURSOR_SIZE: u32 = 32;

pub struct Art {
    frames: Vec<(&'static str, draw::Rgba)>,
}

impl Art {
    pub fn load() -> Self {
        let names = [
            "pointer", "crawl", "fly", "reach", "click1", "click2", "type1", "type2", "busy1",
            "busy2", "busy3", "busy4",
        ];
        let frames = names
            .into_iter()
            .map(|name| {
                let bytes = cursor_png(name).unwrap_or_else(|| panic!("missing cursor {name}"));
                (name, draw::decode_rgba(bytes))
            })
            .collect();
        Self { frames }
    }

    pub fn frame(&self, name: &str) -> &draw::Rgba {
        self.frames
            .iter()
            .find(|(id, _)| *id == name)
            .map(|(_, image)| image)
            .unwrap_or_else(|| &self.frames[0].1)
    }
}

fn cursor_png(name: &str) -> Option<&'static [u8]> {
    Some(match name {
        "pointer" => include_bytes!("../assets/cursor/pointer_32.png"),
        "crawl" => include_bytes!("../assets/cursor/crawl_32.png"),
        "fly" => include_bytes!("../assets/cursor/fly_32.png"),
        "reach" => include_bytes!("../assets/cursor/reach_32.png"),
        "click1" => include_bytes!("../assets/cursor/click1_32.png"),
        "click2" => include_bytes!("../assets/cursor/click2_32.png"),
        "type1" => include_bytes!("../assets/cursor/type1_32.png"),
        "type2" => include_bytes!("../assets/cursor/type2_32.png"),
        "busy1" => include_bytes!("../assets/cursor/busy1_32.png"),
        "busy2" => include_bytes!("../assets/cursor/busy2_32.png"),
        "busy3" => include_bytes!("../assets/cursor/busy3_32.png"),
        "busy4" => include_bytes!("../assets/cursor/busy4_32.png"),
        _ => return None,
    })
}

pub struct Watch {
    pub xtest: Vec<u16>,
    pub ready: bool,
    pending_px: f64,
}

impl Watch {
    pub fn attach(conn: &RustConnection, root: u32) -> Self {
        let mut watch = Self {
            xtest: Vec::new(),
            ready: false,
            pending_px: 0.0,
        };
        if conn.xinput_xi_query_version(2, 2).ok().and_then(|cookie| cookie.reply().ok()).is_none()
        {
            return watch;
        }
        let Ok(cookie) = conn.xinput_xi_query_device(0u16) else {
            return watch;
        };
        let Ok(reply) = cookie.reply() else {
            return watch;
        };
        for info in &reply.infos {
            let name = String::from_utf8_lossy(&info.name).to_ascii_lowercase();
            if name.contains("xtest") {
                watch.xtest.push(info.deviceid);
            }
        }
        let mask = XIEventMask::RAW_MOTION | XIEventMask::RAW_BUTTON_PRESS | XIEventMask::RAW_KEY_PRESS;
        let select = xinput::EventMask {
            deviceid: 0,
            mask: vec![mask],
        };
        let selected = conn.xinput_xi_select_events(root, &[select]);
        let Ok(cookie) = selected else {
            return watch;
        };
        if cookie.check().is_err() {
            return watch;
        }
        watch.ready = !watch.xtest.is_empty();
        watch
    }

    pub fn observe(&mut self, event: &Event) -> Option<LiveInput> {
        if !self.ready {
            return None;
        }
        match event {
            Event::XinputRawMotion(raw) => {
                if automation::is_synthetic(raw.deviceid, raw.sourceid, &self.xtest) {
                    return None;
                }
                if raw.axisvalues_raw.is_empty() {
                    return Some(LiveInput::Motion);
                }
                self.pending_px += raw
                    .axisvalues_raw
                    .iter()
                    .take(2)
                    .map(|value| fp_abs(*value))
                    .sum::<f64>();
                if self.pending_px >= 1.0 {
                    self.pending_px = 0.0;
                    Some(LiveInput::Motion)
                } else {
                    None
                }
            }
            Event::XinputRawButtonPress(raw) => {
                if automation::is_synthetic(raw.deviceid, raw.sourceid, &self.xtest) {
                    None
                } else {
                    Some(LiveInput::Button)
                }
            }
            Event::XinputRawKeyPress(raw) => {
                if automation::is_synthetic(raw.deviceid, raw.sourceid, &self.xtest) {
                    None
                } else {
                    Some(LiveInput::Key)
                }
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveInput {
    Motion,
    Button,
    Key,
}

fn fp_abs(value: xinput::Fp3232) -> f64 {
    let frac = f64::from(value.frac) / 4_294_967_296.0;
    let signed = f64::from(value.integral) + if value.integral < 0 { -frac } else { frac };
    signed.abs()
}

pub struct BugCursor {
    current: u32,
    root: u32,
    window: u32,
    shown: Option<(CursorPose, usize)>,
}

impl BugCursor {
    pub fn new(root: u32, window: u32) -> Self {
        Self {
            current: 0,
            root,
            window,
            shown: None,
        }
    }

    pub fn shown(&self) -> Option<(CursorPose, usize)> {
        self.shown
    }

    pub fn apply(
        &mut self,
        conn: &RustConnection,
        image: &draw::Rgba,
        pose: CursorPose,
        frame: usize,
    ) -> Result<(), String> {
        if self.shown == Some((pose, frame)) && self.current != 0 {
            return Ok(());
        }
        let (hot_x, hot_y) = automation::hotspot_for(CURSOR_SIZE);
        let cursor = make_cursor(conn, self.root, image, hot_x, hot_y)?;
        define(conn, self.root, cursor)?;
        define(conn, self.window, cursor)?;
        let previous = self.current;
        self.current = cursor;
        self.shown = Some((pose, frame));
        if previous != 0 {
            let _ = conn.free_cursor(previous);
        }
        Ok(())
    }

    pub fn restore(&mut self, conn: &RustConnection) {
        let _ = define(conn, self.root, 0);
        let _ = define(conn, self.window, 0);
        if self.current != 0 {
            let _ = conn.free_cursor(self.current);
            self.current = 0;
        }
        self.shown = None;
        let _ = conn.flush();
    }
}

pub fn root_pointer(conn: &RustConnection, root: u32) -> Option<(i32, i32)> {
    let reply = conn.query_pointer(root).ok()?.reply().ok()?;
    Some((i32::from(reply.root_x), i32::from(reply.root_y)))
}

fn define(conn: &RustConnection, window: u32, cursor: u32) -> Result<(), String> {
    conn.change_window_attributes(
        window,
        &xproto::ChangeWindowAttributesAux::new().cursor(cursor),
    )
    .map_err(|err| err.to_string())?
    .check()
    .map_err(|err| err.to_string())
}

fn make_cursor(
    conn: &RustConnection,
    root: u32,
    image: &draw::Rgba,
    hot_x: u16,
    hot_y: u16,
) -> Result<u32, String> {
    let format = argb_format(conn)?;
    let pixmap = conn.generate_id().map_err(|err| err.to_string())?;
    conn.create_pixmap(32, pixmap, root, image.width as u16, image.height as u16)
        .map_err(|err| err.to_string())?;
    let gc = conn.generate_id().map_err(|err| err.to_string())?;
    conn.create_gc(gc, pixmap, &xproto::CreateGCAux::new())
        .map_err(|err| err.to_string())?;
    let lsb = conn.setup().image_byte_order == ImageOrder::LSB_FIRST;
    let data = pack_argb(image, &format.direct, lsb);
    conn.put_image(
        ImageFormat::Z_PIXMAP,
        pixmap,
        gc,
        image.width as u16,
        image.height as u16,
        0,
        0,
        0,
        32,
        &data,
    )
    .map_err(|err| err.to_string())?;
    let picture = conn.generate_id().map_err(|err| err.to_string())?;
    conn.render_create_picture(picture, pixmap, format.id, &render::CreatePictureAux::new())
        .map_err(|err| err.to_string())?;
    let cursor = conn.generate_id().map_err(|err| err.to_string())?;
    conn.render_create_cursor(cursor, picture, hot_x, hot_y)
        .map_err(|err| err.to_string())?
        .check()
        .map_err(|err| err.to_string())?;
    let _ = conn.render_free_picture(picture);
    let _ = conn.free_gc(gc);
    let _ = conn.free_pixmap(pixmap);
    Ok(cursor)
}

fn argb_format(conn: &RustConnection) -> Result<render::Pictforminfo, String> {
    let reply = conn
        .render_query_pict_formats()
        .map_err(|err| err.to_string())?
        .reply()
        .map_err(|err| err.to_string())?;
    reply
        .formats
        .into_iter()
        .find(|format| {
            format.type_ == PictType::DIRECT
                && format.depth == 32
                && format.direct.red_mask == 0xff
                && format.direct.green_mask == 0xff
                && format.direct.blue_mask == 0xff
                && format.direct.alpha_mask == 0xff
        })
        .ok_or_else(|| "render has no argb format".to_string())
}

fn pack_argb(image: &draw::Rgba, format: &render::Directformat, lsb: bool) -> Vec<u8> {
    let mut data = Vec::with_capacity(image.rgba.len());
    for px in image.rgba.chunks_exact(4) {
        let mut pixel = 0u32;
        pixel |= u32::from(px[0]) << format.red_shift;
        pixel |= u32::from(px[1]) << format.green_shift;
        pixel |= u32::from(px[2]) << format.blue_shift;
        pixel |= u32::from(px[3]) << format.alpha_shift;
        let bytes = if lsb {
            pixel.to_le_bytes()
        } else {
            pixel.to_be_bytes()
        };
        data.extend_from_slice(&bytes);
    }
    data
}
