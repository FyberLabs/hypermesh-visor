//! X11 pointer replacement and real-versus-synthetic input.
//!
//! The bug cursor is an ARGB picture turned into a core cursor with the Render
//! extension. The click point is the hotspot from `hotspot.json`. Raw XInput2
//! events whose source is an XTEST device are the visor's own injected input
//! and are ignored. Other raw motion, button presses, and key presses are the
//! user.
//!
//! If XInput2 or the XTEST pointer cannot be identified, the companion falls
//! back to comparing the pointer with the last drive coordinate after the move
//! has settled. That fallback cannot see key presses. A missing XTEST keyboard
//! uses the no-keyboard-interrupt path, so raw keys are not treated as the
//! user. A type sample with no known pointer is left out of that comparison.
//! Wayland automation is off
//! until issue #17, so this detector is not asked to tell portal input from a
//! physical device.
//!
//! Client windows that set their own cursor would cover a cursor defined only
//! on the root. XFixes `ChangeCursorByName` replaces those cursor resources
//! and frees the originals, so the text caret could not be restored.
//! `XGrabPointer` would keep the bug sprite on top and would also take pointer
//! events away from the window being clicked. While the bug cursor is showing,
//! the system sprite is hidden and a click-through window follows the pointer.

use x11rb::connection::Connection;
use x11rb::protocol::render::{self, ConnectionExt as _, PictType};
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xinput::{self, ConnectionExt as _, DeviceType, XIEventMask};
use x11rb::protocol::xproto::{
    self, ClipOrdering, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, ImageFormat,
    ImageOrder, StackMode, WindowClass,
};
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
    xtest_pointer: Vec<u16>,
    xtest_keyboard: Vec<u16>,
    pointer_ready: bool,
    keyboard_ready: bool,
    pending_px: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum XTestClass {
    Pointer,
    Keyboard,
}

/// An XTEST device belongs to one class. A name is only a fallback when the
/// device type is neither pointer nor keyboard.
pub(crate) fn classify_xtest_device(name: &str, device_type: u16) -> Option<XTestClass> {
    let name = name.to_ascii_lowercase();
    if !name.contains("xtest") {
        return None;
    }
    let pointer = u16::from(DeviceType::SLAVE_POINTER);
    let master_pointer = u16::from(DeviceType::MASTER_POINTER);
    let keyboard = u16::from(DeviceType::SLAVE_KEYBOARD);
    let master_keyboard = u16::from(DeviceType::MASTER_KEYBOARD);
    if device_type == pointer || device_type == master_pointer {
        return Some(XTestClass::Pointer);
    }
    if device_type == keyboard || device_type == master_keyboard {
        return Some(XTestClass::Keyboard);
    }
    if name.contains("keyboard") {
        return Some(XTestClass::Keyboard);
    }
    if name.contains("pointer") {
        return Some(XTestClass::Pointer);
    }
    None
}

impl Watch {
    fn blank() -> Self {
        Self {
            xtest_pointer: Vec::new(),
            xtest_keyboard: Vec::new(),
            pointer_ready: false,
            keyboard_ready: false,
            pending_px: 0.0,
        }
    }

    pub fn attach(conn: &RustConnection, root: u32) -> Self {
        let mut watch = Self::blank();
        if conn
            .xinput_xi_query_version(2, 2)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_none()
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
            let name = String::from_utf8_lossy(&info.name);
            watch.consider(&name, u16::from(info.type_), info.deviceid);
        }
        let pointer = watch.accepts_raw_pointer_ids();
        let keyboard = watch.accepts_raw_keyboard_ids();
        if !pointer && !keyboard {
            return watch;
        }
        let mask = match (pointer, keyboard) {
            (true, true) => {
                XIEventMask::RAW_MOTION | XIEventMask::RAW_BUTTON_PRESS | XIEventMask::RAW_KEY_PRESS
            }
            (true, false) => XIEventMask::RAW_MOTION | XIEventMask::RAW_BUTTON_PRESS,
            (false, true) => XIEventMask::RAW_KEY_PRESS,
            (false, false) => return watch,
        };
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
        watch.finish_select(true);
        watch
    }

    fn consider(&mut self, name: &str, device_type: u16, device_id: u16) {
        match classify_xtest_device(name, device_type) {
            Some(XTestClass::Pointer) => self.xtest_pointer.push(device_id),
            Some(XTestClass::Keyboard) => self.xtest_keyboard.push(device_id),
            None => {}
        }
    }

    fn accepts_raw_pointer_ids(&self) -> bool {
        !self.xtest_pointer.is_empty()
    }

    fn accepts_raw_keyboard_ids(&self) -> bool {
        !self.xtest_keyboard.is_empty()
    }

    fn finish_select(&mut self, selected: bool) {
        if !selected {
            return;
        }
        self.pointer_ready = self.accepts_raw_pointer_ids();
        self.keyboard_ready = self.accepts_raw_keyboard_ids();
    }

    /// Motion and buttons can use XI2. A missing XTEST pointer keeps the
    /// drive-sample detector.
    pub fn accepts_raw_pointer(&self) -> bool {
        self.pointer_ready
    }

    /// Keys can be filtered. A missing XTEST keyboard is the fail-closed
    /// interrupt: raw keys are not user input.
    pub fn accepts_raw_keyboard(&self) -> bool {
        self.keyboard_ready
    }

    fn pointer_is_user(&self, device: u16, source: u16) -> bool {
        self.pointer_ready && !automation::is_synthetic(device, source, &self.xtest_pointer)
    }

    fn key_is_user(&self, device: u16, source: u16) -> bool {
        self.keyboard_ready && !automation::is_synthetic(device, source, &self.xtest_keyboard)
    }

    pub fn observe(&mut self, event: &Event) -> Option<LiveInput> {
        match event {
            Event::XinputRawMotion(raw) => {
                if !self.pointer_is_user(raw.deviceid, raw.sourceid) {
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
                if self.pointer_is_user(raw.deviceid, raw.sourceid) {
                    Some(LiveInput::Button)
                } else {
                    None
                }
            }
            Event::XinputRawKeyPress(raw) => {
                if self.key_is_user(raw.deviceid, raw.sourceid) {
                    Some(LiveInput::Key)
                } else {
                    None
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
    fp3232(value.integral, value.frac).abs()
}

/// XI2 `FP3232` is `integral + frac / 2^32`. A negative value is stored with
/// the integral rounded toward -inf and a positive fraction, so -0.25 is
/// integral -1 and fraction 0.75.
pub(crate) fn fp3232(integral: i32, frac: u32) -> f64 {
    f64::from(integral) + f64::from(frac) / 4_294_967_296.0
}

pub(crate) fn premultiply(px: [u8; 4]) -> [u8; 4] {
    let alpha = u32::from(px[3]);
    let scale = |channel: u8| ((u32::from(channel) * alpha + 127) / 255) as u8;
    [scale(px[0]), scale(px[1]), scale(px[2]), px[3]]
}

pub struct BugCursor {
    current: u32,
    root: u32,
    window: u32,
    overlay: u32,
    overlay_gc: u32,
    sprite_hidden: bool,
    hot: (i16, i16),
    /// True only after this process wrote the root window's cursor. The
    /// overlay path never does, so restore leaves a custom root cursor alone.
    root_cursor_set: bool,
    /// An unsupported extension is permanent. Further frames skip the overlay.
    overlay_disabled: bool,
    /// The last overlay failure was already logged.
    overlay_noted: bool,
    /// A cursor-creation error is permanent for this process.
    cursor_disabled: bool,
    /// The last cursor-creation failure was already logged.
    cursor_noted: bool,
    shown: Option<(CursorPose, usize)>,
    depth: u8,
    visual: u32,
    colormap: u32,
    red: u32,
    green: u32,
    blue: u32,
}

impl BugCursor {
    pub fn new(
        root: u32,
        window: u32,
        depth: u8,
        visual: u32,
        colormap: u32,
        red: u32,
        green: u32,
        blue: u32,
    ) -> Self {
        Self {
            current: 0,
            root,
            window,
            overlay: 0,
            overlay_gc: 0,
            sprite_hidden: false,
            hot: (0, 0),
            root_cursor_set: false,
            overlay_disabled: false,
            overlay_noted: false,
            cursor_disabled: false,
            cursor_noted: false,
            shown: None,
            depth,
            visual,
            colormap,
            red,
            green,
            blue,
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
        if self.cursor_disabled {
            return Ok(());
        }
        if self.shown == Some((pose, frame)) && self.current != 0 {
            return Ok(());
        }
        let (hot_x, hot_y) = automation::hotspot_for(CURSOR_SIZE);
        self.hot = (hot_x as i16, hot_y as i16);
        let cursor = match make_cursor(conn, self.root, image, hot_x, hot_y) {
            Ok(cursor) => cursor,
            Err(err) => {
                if cursor_give_up(&mut self.cursor_disabled, &mut self.cursor_noted) {
                    eprintln!("hypermesh-companion: cursor creation failed: {err}");
                }
                return Ok(());
            }
        };
        define(conn, self.window, cursor)?;
        let covering = self.cover_pointer(conn, image);
        if replaces_root_cursor(covering) {
            define(conn, self.root, cursor)?;
            self.root_cursor_set = true;
        }
        let previous = self.current;
        self.current = cursor;
        self.shown = Some((pose, frame));
        if previous != 0 {
            let _ = conn.free_cursor(previous);
        }
        Ok(())
    }

    pub fn follow(&mut self, conn: &RustConnection, x: i32, y: i32) {
        if self.overlay == 0 || !self.sprite_hidden {
            return;
        }
        let _ = conn.configure_window(
            self.overlay,
            &ConfigureWindowAux::new()
                .x(x - i32::from(self.hot.0))
                .y(y - i32::from(self.hot.1))
                .stack_mode(StackMode::ABOVE),
        );
    }

    pub fn restore(&mut self, conn: &RustConnection) {
        self.reveal_sprite(conn);
        self.unmap_overlay(conn);
        if self.root_cursor_set {
            let _ = define(conn, self.root, 0);
            self.root_cursor_set = false;
        }
        let _ = define(conn, self.window, 0);
        if self.current != 0 {
            let _ = conn.free_cursor(self.current);
            self.current = 0;
        }
        self.shown = None;
        let _ = conn.flush();
    }

    /// Hide the system sprite and paint the bug on a click-through window.
    /// False means the caller has to put the cursor on the root window.
    fn cover_pointer(&mut self, conn: &RustConnection, image: &draw::Rgba) -> bool {
        if self.depth != 32 || self.overlay_disabled {
            return false;
        }
        if let Err(err) = self.present_overlay(conn, image) {
            if !self.overlay_noted {
                if overlay_error_disables_retry(&err) {
                    eprintln!(
                        "hypermesh-companion: cursor overlay unavailable ({err}); the root cursor is used"
                    );
                } else {
                    eprintln!("hypermesh-companion: cursor window: {err}");
                }
                self.overlay_noted = true;
            }
            if overlay_error_disables_retry(&err) {
                self.overlay_disabled = true;
            }
            self.reveal_sprite(conn);
            return false;
        }
        self.overlay_noted = false;
        if self.conceal_sprite(conn) {
            return true;
        }
        self.unmap_overlay(conn);
        false
    }

    fn conceal_sprite(&mut self, conn: &RustConnection) -> bool {
        if self.sprite_hidden {
            return true;
        }
        if conn
            .xfixes_query_version(4, 0)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_none()
        {
            return false;
        }
        let Ok(cookie) = conn.xfixes_hide_cursor(self.root) else {
            return false;
        };
        if cookie.check().is_err() {
            return false;
        }
        self.sprite_hidden = true;
        true
    }

    fn reveal_sprite(&mut self, conn: &RustConnection) {
        if !self.sprite_hidden {
            return;
        }
        if let Ok(cookie) = conn.xfixes_show_cursor(self.root) {
            let _ = cookie.check();
        }
        self.sprite_hidden = false;
    }

    fn present_overlay(&mut self, conn: &RustConnection, image: &draw::Rgba) -> Result<(), String> {
        if self.overlay == 0 {
            let (overlay, gc) = open_overlay(
                conn,
                self.depth,
                self.root,
                self.visual,
                self.colormap,
                image,
            )?;
            self.overlay = overlay;
            self.overlay_gc = gc;
        }
        let lsb = conn.setup().image_byte_order == ImageOrder::LSB_FIRST;
        let data = pack_visual(image, self.red, self.green, self.blue, lsb);
        conn.put_image(
            ImageFormat::Z_PIXMAP,
            self.overlay,
            self.overlay_gc,
            image.width as u16,
            image.height as u16,
            0,
            0,
            0,
            self.depth,
            &data,
        )
        .map_err(|err| err.to_string())?;
        conn.map_window(self.overlay)
            .map_err(|err| err.to_string())?;
        Ok(())
    }

    fn unmap_overlay(&mut self, conn: &RustConnection) {
        if self.overlay != 0 {
            let _ = conn.unmap_window(self.overlay);
        }
    }
}

pub fn root_pointer(conn: &RustConnection, root: u32) -> Option<(i32, i32)> {
    root_pointer_state(conn, root).map(|(x, y, _)| (x, y))
}

/// Root pointer plus the `QueryPointer` button mask. Button1 is bit 8.
pub fn root_pointer_state(conn: &RustConnection, root: u32) -> Option<(i32, i32, u32)> {
    let reply = conn.query_pointer(root).ok()?.reply().ok()?;
    Some((
        i32::from(reply.root_x),
        i32::from(reply.root_y),
        u32::from(reply.mask),
    ))
}

/// The overlay covers the pointer, so the root window's cursor stays as the
/// user left it. X11 has no request that reads that cursor back. The fallback
/// path writes it and can only restore the default cursor on the way out.
pub(crate) fn replaces_root_cursor(overlay_covering: bool) -> bool {
    !overlay_covering
}

/// A missing Shape extension will fail the same way on every frame.
pub(crate) fn overlay_error_disables_retry(err: &str) -> bool {
    let err = err.to_ascii_lowercase();
    err.contains("extension") || err.contains("shape")
}

/// Create the click-through window. A failure after the window or GC request
/// was queued destroys that resource before the error is returned.
fn open_overlay(
    conn: &RustConnection,
    depth: u8,
    root: u32,
    visual: u32,
    colormap: u32,
    image: &draw::Rgba,
) -> Result<(u32, u32), String> {
    let overlay = conn.generate_id().map_err(|err| err.to_string())?;
    conn.create_window(
        depth,
        overlay,
        root,
        0,
        0,
        image.width as u16,
        image.height as u16,
        0,
        WindowClass::INPUT_OUTPUT,
        visual,
        &CreateWindowAux::new()
            .colormap(colormap)
            .border_pixel(0)
            .background_pixel(0)
            .override_redirect(1u32),
    )
    .map_err(|err| err.to_string())?;
    let gc = match conn.generate_id() {
        Ok(gc) => gc,
        Err(err) => {
            release_overlay(conn, overlay, None);
            return Err(err.to_string());
        }
    };
    if let Err(err) = conn.create_gc(gc, overlay, &xproto::CreateGCAux::new()) {
        release_overlay(conn, overlay, None);
        return Err(err.to_string());
    }
    let shaped = conn
        .shape_rectangles(
            shape::SO::SET,
            shape::SK::INPUT,
            ClipOrdering::UNSORTED,
            overlay,
            0,
            0,
            &[],
        )
        .map_err(|err| err.to_string())
        .and_then(|cookie| cookie.check().map_err(|err| err.to_string()));
    if let Err(err) = shaped {
        release_overlay(conn, overlay, Some(gc));
        return Err(err);
    }
    Ok((overlay, gc))
}

fn release_overlay(conn: &RustConnection, window: u32, gc: Option<u32>) {
    if let Some(gc) = gc {
        if let Ok(cookie) = conn.free_gc(gc) {
            let _ = cookie.check();
        }
    }
    if let Ok(cookie) = conn.destroy_window(window) {
        let _ = cookie.check();
    }
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

/// Pixmap, GC, and picture allocated for one cursor. Drop frees whichever
/// of them were created, including when `render_create_cursor` fails.
struct CursorScratch<'a> {
    conn: &'a RustConnection,
    pixmap: u32,
    gc: u32,
    picture: u32,
}

impl Drop for CursorScratch<'_> {
    fn drop(&mut self) {
        if self.picture != 0 {
            let _ = self.conn.render_free_picture(self.picture);
        }
        if self.gc != 0 {
            let _ = self.conn.free_gc(self.gc);
        }
        if self.pixmap != 0 {
            let _ = self.conn.free_pixmap(self.pixmap);
        }
    }
}

/// First failure disables further attempts and asks the caller to log once.
pub(crate) fn cursor_give_up(disabled: &mut bool, noted: &mut bool) -> bool {
    *disabled = true;
    if *noted {
        return false;
    }
    *noted = true;
    true
}

fn make_cursor(
    conn: &RustConnection,
    root: u32,
    image: &draw::Rgba,
    hot_x: u16,
    hot_y: u16,
) -> Result<u32, String> {
    let format = argb_format(conn)?;
    let mut scratch = CursorScratch {
        conn,
        pixmap: 0,
        gc: 0,
        picture: 0,
    };
    scratch.pixmap = conn.generate_id().map_err(|err| err.to_string())?;
    conn.create_pixmap(
        32,
        scratch.pixmap,
        root,
        image.width as u16,
        image.height as u16,
    )
    .map_err(|err| err.to_string())?;
    scratch.gc = conn.generate_id().map_err(|err| err.to_string())?;
    conn.create_gc(scratch.gc, scratch.pixmap, &xproto::CreateGCAux::new())
        .map_err(|err| err.to_string())?;
    let lsb = conn.setup().image_byte_order == ImageOrder::LSB_FIRST;
    let data = pack_argb(image, &format.direct, lsb);
    conn.put_image(
        ImageFormat::Z_PIXMAP,
        scratch.pixmap,
        scratch.gc,
        image.width as u16,
        image.height as u16,
        0,
        0,
        0,
        32,
        &data,
    )
    .map_err(|err| err.to_string())?;
    scratch.picture = conn.generate_id().map_err(|err| err.to_string())?;
    conn.render_create_picture(
        scratch.picture,
        scratch.pixmap,
        format.id,
        &render::CreatePictureAux::new(),
    )
    .map_err(|err| err.to_string())?;
    let cursor = conn.generate_id().map_err(|err| err.to_string())?;
    conn.render_create_cursor(cursor, scratch.picture, hot_x, hot_y)
        .map_err(|err| err.to_string())?
        .check()
        .map_err(|err| err.to_string())?;
    drop(scratch);
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
        let px = premultiply([px[0], px[1], px[2], px[3]]);
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

fn pack_visual(image: &draw::Rgba, red: u32, green: u32, blue: u32, lsb: bool) -> Vec<u8> {
    let shift = |mask: u32| mask.trailing_zeros();
    let used = red | green | blue;
    let alpha = (!used) & 0xffff_ffff;
    let mut data = Vec::with_capacity(image.rgba.len());
    for px in image.rgba.chunks_exact(4) {
        let px = premultiply([px[0], px[1], px[2], px[3]]);
        let mut pixel = 0u32;
        if red != 0 {
            pixel |= u32::from(px[0]) << shift(red);
        }
        if green != 0 {
            pixel |= u32::from(px[1]) << shift(green);
        }
        if blue != 0 {
            pixel |= u32::from(px[2]) << shift(blue);
        }
        if alpha != 0 {
            pixel |= u32::from(px[3]) << shift(alpha);
        }
        let bytes = if lsb {
            pixel.to_le_bytes()
        } else {
            pixel.to_be_bytes()
        };
        data.extend_from_slice(&bytes);
    }
    data
}

#[cfg(test)]
mod tests {
    use super::{
        cursor_give_up, fp3232, overlay_error_disables_retry, premultiply, replaces_root_cursor,
    };

    #[test]
    fn negative_fp3232_keeps_the_positive_fraction() {
        let frac = (0.75 * 4_294_967_296.0) as u32;
        let value = fp3232(-1, frac);
        assert!((value + 0.25).abs() < 1e-6, "{value}");
        assert!(value.abs() < 1.0);
        assert!((fp3232(1, 0) - 1.0).abs() < 1e-9);
        assert!((fp3232(-2, 0) + 2.0).abs() < 1e-9);
        assert!((fp3232(0, 0)).abs() < 1e-9);
    }

    #[test]
    fn a_cursor_creation_failure_logs_once_and_stops_retrying() {
        let mut disabled = false;
        let mut noted = false;
        assert!(cursor_give_up(&mut disabled, &mut noted));
        assert!(disabled);
        assert!(!cursor_give_up(&mut disabled, &mut noted));
        assert!(disabled);
    }

    #[test]
    fn overlay_leaves_the_root_cursor_alone() {
        assert!(!replaces_root_cursor(true));
        assert!(replaces_root_cursor(false));
    }

    #[test]
    fn an_unsupported_extension_disables_overlay_retries() {
        assert!(overlay_error_disables_retry("Unsupported extension"));
        assert!(overlay_error_disables_retry(
            "X11 error RequestError { error_code: 1, shape: true }"
        ));
        assert!(!overlay_error_disables_retry("connection reset"));
        assert!(!overlay_error_disables_retry(""));
    }

    #[test]
    fn xtest_pointer_and_keyboard_are_tracked_apart() {
        let pointer = u16::from(x11rb::protocol::xinput::DeviceType::SLAVE_POINTER);
        let keyboard = u16::from(x11rb::protocol::xinput::DeviceType::SLAVE_KEYBOARD);
        let mut watch = super::Watch::blank();
        watch.consider("Virtual core XTEST pointer", pointer, 6);
        watch.consider("Virtual core pointer", pointer, 2);
        watch.finish_select(true);
        assert!(watch.accepts_raw_pointer());
        assert!(!watch.accepts_raw_keyboard());
        assert!(!watch.key_is_user(9, 3));
        assert!(!watch.pointer_is_user(6, 2));
        assert!(watch.pointer_is_user(9, 3));

        let mut keys = super::Watch::blank();
        keys.consider("Virtual core XTEST keyboard", keyboard, 7);
        keys.finish_select(true);
        assert!(!keys.accepts_raw_pointer());
        assert!(keys.accepts_raw_keyboard());
        assert!(!keys.key_is_user(7, 3));
        assert!(keys.key_is_user(9, 3));
        assert!(!keys.pointer_is_user(9, 3));

        let mut both = super::Watch::blank();
        both.consider("Virtual core XTEST pointer", pointer, 6);
        both.consider("Virtual core XTEST keyboard", keyboard, 7);
        both.finish_select(false);
        assert!(!both.accepts_raw_pointer());
        assert!(!both.accepts_raw_keyboard());
        assert!(!both.key_is_user(9, 3));
    }

    #[test]
    fn cursor_colors_are_premultiplied_by_alpha() {
        assert_eq!(premultiply([255, 0, 0, 128]), [128, 0, 0, 128]);
        assert_eq!(premultiply([255, 255, 255, 0]), [0, 0, 0, 0]);
        assert_eq!(premultiply([10, 20, 30, 255]), [10, 20, 30, 255]);
    }
}
