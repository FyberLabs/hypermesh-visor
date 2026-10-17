//! The desktop bug. Frames come from `assets/states`. When a session is open
//! the purpose and current verb are drawn under the bug.

use std::collections::HashMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use crate::auth::Pose;
use crate::mascot::{self, Mascot};

const TARGET_WIDTH: u32 = 400;
/// `place` keeps 48 px under the window. This leaves that, plus a small gap,
/// so a capped menu still sits on the screen.
pub const VIEW_MARGIN: u32 = 64;
/// One menu row (48 px card plus the 4 px gap). The wheel scrolls by this.
pub const MENU_SCROLL_STEP: i32 = 52;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    ToggleMenu,
    OpenCli,
    Login,
    OpenBilling,
    OpenRent,
    OpenDashboard,
    ToggleEyes,
    ToggleBugCursor,
    McpProfiles,
    McpDoctor,
    ContinueAutomation,
    DismissAutomation,
}

#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub action: Action,
}

impl Hit {
    pub fn contains(self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }
}

pub struct Scene {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub hits: Vec<Hit>,
}

struct Image {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub struct Sprites {
    frames: HashMap<(Mascot, usize), Image>,
    idle_eyes: Vec<Vec<Rect>>,
    catalog: mascot::Catalog,
}

impl Sprites {
    pub fn load() -> Self {
        let catalog = mascot::Catalog::load();
        let mut frames = HashMap::new();
        let mut idle_eyes = Vec::new();
        for state in Mascot::ALL {
            let clip = catalog.clip(state);
            for (index, name) in clip.files.iter().enumerate() {
                let bytes = mascot::state_png(name).unwrap_or_else(|| panic!("missing {name}"));
                let image = decode_png(bytes);
                if state == Mascot::Idle {
                    idle_eyes.push(find_eyes(&image));
                }
                frames.insert((state, index), image);
            }
        }
        Self {
            frames,
            idle_eyes,
            catalog,
        }
    }

    pub fn frame(&self, state: Mascot, index: usize) -> &Image {
        let count = self.catalog.clip(state).files.len();
        let index = if count == 0 { 0 } else { index % count };
        self.frames
            .get(&(state, index))
            .unwrap_or_else(|| &self.frames[&(state, 0)])
    }

    pub fn timing(&self, state: Mascot) -> (u64, usize, bool) {
        let clip = self.catalog.clip(state);
        (clip.ms_per_frame, clip.files.len(), clip.looped)
    }
}

pub fn render(
    sprites: &Sprites,
    pose: &Pose,
    mascot: Mascot,
    frame: usize,
    menu: bool,
    ask_continue: bool,
    status: Option<&str>,
    eyes_follow: bool,
    bug_cursor: bool,
    pointer: Option<(i32, i32)>,
    max_height: u32,
    scroll: i32,
) -> Scene {
    let base = sprites.frame(mascot, frame);
    let mut bug = base.clone_image();
    let bug_top = 0i32;
    // The illustrated frames already blink and bob. Pupils still track when
    // the idle frame has the two eye shapes and a session is open.
    if eyes_follow && mascot == Mascot::Idle {
        if let (Pose::Active { .. }, Some((px, py))) = (pose, pointer) {
            let eyes = sprites
                .idle_eyes
                .get(frame)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            track_eyes(&mut bug, eyes, (px, py - bug_top));
        }
    }
    let width = bug.w.max(TARGET_WIDTH);
    let mut blocks: Vec<Block> = Vec::new();
    let mut y = bug.h as i32;
    if let Pose::Active {
        purpose,
        verb,
        mode,
    } = pose
    {
        // Prefer-MCP shows `mcp:<id>` on the mode line; mouse/type stay the verb fallback.
        let mode_line = mode.as_deref().or(verb.as_deref()).unwrap_or("—");
        let lines = [
            format!("purpose: {}", sanitize(purpose)),
            format!("verb: {}", sanitize(mode_line)),
        ];
        let card = text_card(width, 3, &lines, [7, 25, 61, 255], [255, 255, 255, 255]);
        blocks.push(Block {
            y,
            image: card,
            hits: Vec::new(),
        });
        y += blocks.last().unwrap().image.h as i32 + 8;
    }
    if let Some(status) = status.filter(|line| !line.is_empty()) {
        let card = text_card(
            width,
            2,
            &[sanitize(status)],
            [12, 18, 28, 230],
            [220, 228, 220, 255],
        );
        blocks.push(Block {
            y,
            image: card,
            hits: Vec::new(),
        });
        y += blocks.last().unwrap().image.h as i32 + 8;
    }
    if ask_continue {
        let rows = [
            ("Continue the automation?", None),
            ("Continue", Some(Action::ContinueAutomation)),
            ("Not now", Some(Action::DismissAutomation)),
        ];
        for (label, action) in rows {
            let row = text_card(
                width,
                3,
                &[label.to_string()],
                [16, 32, 36, 245],
                [236, 244, 236, 255],
            );
            let h = row.h as i32;
            let hits = action
                .map(|action| Hit {
                    x: 0,
                    y,
                    w: width as i32,
                    h,
                    action,
                })
                .into_iter()
                .collect();
            blocks.push(Block {
                y,
                image: row,
                hits,
            });
            y += h + 4;
        }
    } else if menu {
        let eyes_label = if eyes_follow {
            "Eyes follow: on"
        } else {
            "Eyes follow: off"
        };
        let bug_label = if bug_cursor {
            "Bug cursor: on"
        } else {
            "Bug cursor: off"
        };
        let rows = [
            ("Open CLI", Action::OpenCli),
            ("MCP profiles", Action::McpProfiles),
            ("MCP doctor", Action::McpDoctor),
            ("Login", Action::Login),
            ("Billing", Action::OpenBilling),
            ("Rent", Action::OpenRent),
            ("Dashboards", Action::OpenDashboard),
            (eyes_label, Action::ToggleEyes),
            (bug_label, Action::ToggleBugCursor),
        ];
        for (label, action) in rows {
            let row = text_card(
                width,
                3,
                &[label.to_string()],
                [16, 32, 36, 245],
                [236, 244, 236, 255],
            );
            let h = row.h as i32;
            blocks.push(Block {
                y,
                image: row,
                hits: vec![Hit {
                    x: 0,
                    y,
                    w: width as i32,
                    h,
                    action,
                }],
            });
            y += h + 4;
        }
    }
    let bug_h = bug.h as i32;
    let natural = y.max(bug_h).max(1);
    let limit = i32::try_from(max_height.max(1)).unwrap_or(i32::MAX);
    let height = if natural <= limit {
        natural
    } else {
        limit.max(1)
    };
    let scroll = scroll.clamp(0, (natural - height).max(0));
    // The bug stays pinned. Rows scroll in the space under it, and a value
    // past the end clamps so the last row lands fully inside the window.
    let clip_top = if bug_h >= height { 0 } else { bug_h };
    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    blit(&mut rgba, width, &bug, 0, bug_top, 0);
    let mut hits = vec![Hit {
        x: 0,
        y: 0,
        w: width as i32,
        h: bug_h.min(height),
        action: Action::ToggleMenu,
    }];
    for block in &blocks {
        let drawn = block.y - scroll;
        blit(&mut rgba, width, &block.image, 0, drawn, clip_top);
        for hit in &block.hits {
            if let Some(hit) = clip_hit(*hit, scroll, clip_top, height) {
                hits.push(hit);
            }
        }
    }
    // Menu rows sit above the bug in the hit list so a click on a row wins.
    hits.sort_by_key(|hit| match hit.action {
        Action::ToggleMenu => 1,
        _ => 0,
    });
    Scene {
        width,
        height: height as u32,
        rgba,
        hits,
    }
}

fn clip_hit(mut hit: Hit, scroll: i32, clip_top: i32, height: i32) -> Option<Hit> {
    hit.y -= scroll;
    if hit.y < clip_top {
        hit.h -= clip_top - hit.y;
        hit.y = clip_top;
    }
    if hit.y + hit.h > height {
        hit.h = height - hit.y;
    }
    if hit.h <= 0 {
        None
    } else {
        Some(hit)
    }
}

struct Block {
    y: i32,
    image: Image,
    hits: Vec<Hit>,
}

pub fn dump_frames(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let sprites = Sprites::load();
    let idle = render(
        &sprites,
        &Pose::Idle,
        Mascot::Idle,
        0,
        false,
        false,
        None,
        false,
        false,
        None,
        u32::MAX,
        0,
    );
    let active = render(
        &sprites,
        &Pose::Active {
            purpose: "review the desktop".into(),
            verb: Some("view".into()),
            mode: Some("view".into()),
        },
        Mascot::Automation,
        0,
        false,
        false,
        None,
        false,
        false,
        None,
        u32::MAX,
        0,
    );
    write_png(&dir.join("visor-bug-idle.png"), &idle)?;
    write_png(&dir.join("visor-bug-active.png"), &active)?;
    Ok(())
}

pub fn decode_rgba(bytes: &[u8]) -> Rgba {
    let image = decode_png(bytes);
    Rgba {
        width: image.w,
        height: image.h,
        rgba: image.rgba,
    }
}

fn track_eyes(image: &mut Image, eyes: &[Rect], pointer: (i32, i32)) {
    for eye in eyes {
        let blob = pupil_blob(image, *eye);
        if blob.len() < 40 {
            continue;
        }
        let mut cx = 0i32;
        let mut cy = 0i32;
        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;
        for &(x, y) in &blob {
            cx += x;
            cy += y;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
        cx /= blob.len() as i32;
        cy /= blob.len() as i32;
        let max_shift = (eye.w.min(eye.h) / 6).clamp(4, 28);
        let (mut sx, mut sy) = clamp_offset(pointer.0 - cx, pointer.1 - cy, max_shift);
        if min_x + sx < eye.x {
            sx = eye.x - min_x;
        }
        if max_x + sx >= eye.x + eye.w {
            sx = eye.x + eye.w - 1 - max_x;
        }
        if min_y + sy < eye.y {
            sy = eye.y - min_y;
        }
        if max_y + sy >= eye.y + eye.h {
            sy = eye.y + eye.h - 1 - max_y;
        }
        if sx == 0 && sy == 0 {
            continue;
        }
        let white = sclera_color(image, *eye);
        let colors: Vec<[u8; 4]> = blob
            .iter()
            .map(|&(x, y)| {
                let i = (y as u32 * image.w + x as u32) as usize * 4;
                [
                    image.rgba[i],
                    image.rgba[i + 1],
                    image.rgba[i + 2],
                    image.rgba[i + 3],
                ]
            })
            .collect();
        for &(x, y) in &blob {
            put_px(image, x, y, white);
        }
        for (index, &(x, y)) in blob.iter().enumerate() {
            let nx = x + sx;
            let ny = y + sy;
            if nx < eye.x || ny < eye.y || nx >= eye.x + eye.w || ny >= eye.y + eye.h {
                continue;
            }
            put_px(image, nx, ny, colors[index]);
        }
    }
}

fn clamp_offset(dx: i32, dy: i32, max_shift: i32) -> (i32, i32) {
    let dist = ((dx * dx + dy * dy) as f32).sqrt();
    if dist <= max_shift as f32 || dist == 0.0 {
        return (dx, dy);
    }
    let scale = max_shift as f32 / dist;
    (
        (dx as f32 * scale).round() as i32,
        (dy as f32 * scale).round() as i32,
    )
}

fn pupil_blob(image: &Image, eye: Rect) -> Vec<(i32, i32)> {
    let start = nearest_dark(image, eye);
    let Some((sx, sy)) = start else {
        return Vec::new();
    };
    let mut stack = vec![(sx, sy)];
    let mut seen = vec![false; image.w as usize * image.h as usize];
    seen[(sy as u32 * image.w + sx as u32) as usize] = true;
    let mut blob = Vec::new();
    let limit = (eye.w * eye.h * 7) / 10;
    while let Some((x, y)) = stack.pop() {
        blob.push((x, y));
        if blob.len() as i32 > limit {
            return Vec::new();
        }
        for (nx, ny) in [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)] {
            if nx < eye.x || ny < eye.y || nx >= eye.x + eye.w || ny >= eye.y + eye.h {
                continue;
            }
            let ni = (ny as u32 * image.w + nx as u32) as usize;
            if seen[ni] || !dark_pixel(image, nx, ny) {
                continue;
            }
            seen[ni] = true;
            stack.push((nx, ny));
        }
    }
    blob
}

fn nearest_dark(image: &Image, eye: Rect) -> Option<(i32, i32)> {
    let cx = eye.x + eye.w / 2;
    let cy = eye.y + eye.h / 2;
    if dark_pixel(image, cx, cy) {
        return Some((cx, cy));
    }
    let radius = eye.w.max(eye.h);
    for ring in 1..radius {
        for dy in -ring..=ring {
            for dx in -ring..=ring {
                if dx.abs() != ring && dy.abs() != ring {
                    continue;
                }
                let x = cx + dx;
                let y = cy + dy;
                if x < eye.x || y < eye.y || x >= eye.x + eye.w || y >= eye.y + eye.h {
                    continue;
                }
                if dark_pixel(image, x, y) {
                    return Some((x, y));
                }
            }
        }
    }
    None
}

fn dark_pixel(image: &Image, x: i32, y: i32) -> bool {
    if x < 0 || y < 0 || x >= image.w as i32 || y >= image.h as i32 {
        return false;
    }
    let i = (y as u32 * image.w + x as u32) as usize * 4;
    let (r, g, b, a) = (
        image.rgba[i],
        image.rgba[i + 1],
        image.rgba[i + 2],
        image.rgba[i + 3],
    );
    a > 200 && r < 50 && g < 50 && b < 60
}

fn sclera_color(image: &Image, eye: Rect) -> [u8; 4] {
    let mut count = 0u32;
    let mut sum = [0u32; 3];
    for y in eye.y..eye.y + eye.h {
        for x in eye.x..eye.x + eye.w {
            if !eye_pixel(image, x, y) {
                continue;
            }
            let i = (y as u32 * image.w + x as u32) as usize * 4;
            sum[0] += image.rgba[i] as u32;
            sum[1] += image.rgba[i + 1] as u32;
            sum[2] += image.rgba[i + 2] as u32;
            count += 1;
        }
    }
    if count == 0 {
        return [252, 252, 252, 255];
    }
    [
        (sum[0] / count) as u8,
        (sum[1] / count) as u8,
        (sum[2] / count) as u8,
        255,
    ]
}

fn put_px(image: &mut Image, x: i32, y: i32, color: [u8; 4]) {
    if x < 0 || y < 0 || x >= image.w as i32 || y >= image.h as i32 {
        return;
    }
    let i = (y as u32 * image.w + x as u32) as usize * 4;
    image.rgba[i..i + 4].copy_from_slice(&color);
}

fn sanitize(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars().take(72) {
        if ch.is_control() {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    if value.chars().count() > 72 {
        out.push('…');
    }
    out
}

fn text_card(width: u32, scale: i32, lines: &[String], bg: [u8; 4], fg: [u8; 4]) -> Image {
    let line_h = 8 * scale;
    let pad = 12;
    let height = (pad * 2 + line_h * lines.len() as i32).max(1) as u32;
    let mut image = Image {
        w: width,
        h: height,
        rgba: vec![0u8; width as usize * height as usize * 4],
    };
    fill(&mut image, bg);
    for (index, line) in lines.iter().enumerate() {
        let top = pad + line_h * index as i32;
        draw_text(&mut image, scale, 16, top, line, fg);
    }
    image
}

fn draw_text(image: &mut Image, scale: i32, x: i32, y: i32, text: &str, color: [u8; 4]) {
    let mut cursor = x;
    for ch in text.chars() {
        let glyph = glyph(ch);
        for row in 0..7 {
            for col in 0..5 {
                if glyph[row] & (1 << (4 - col)) == 0 {
                    continue;
                }
                for sy in 0..scale {
                    for sx in 0..scale {
                        let px_x = cursor + col as i32 * scale + sx;
                        let px_y = y + row as i32 * scale + sy;
                        if px_x < 0 || px_y < 0 || px_x >= image.w as i32 || px_y >= image.h as i32
                        {
                            continue;
                        }
                        let i = (px_y as u32 * image.w + px_x as u32) as usize * 4;
                        image.rgba[i..i + 4].copy_from_slice(&color);
                    }
                }
            }
        }
        cursor += 6 * scale;
    }
}

fn glyph(ch: char) -> [u8; 7] {
    let ch = if ch.is_ascii() {
        ch.to_ascii_uppercase()
    } else {
        '?'
    };
    match ch {
        ' ' => [0, 0, 0, 0, 0, 0, 0],
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        'C' => [
            0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110,
        ],
        'D' => [
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'G' => [
            0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110,
        ],
        'H' => [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'I' => [
            0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        'J' => [
            0b00111, 0b00010, 0b00010, 0b00010, 0b10010, 0b10010, 0b01100,
        ],
        'K' => [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'M' => [
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'Q' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'S' => [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        'W' => [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010,
        ],
        'X' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
        'Y' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'Z' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110,
        ],
        '6' => [
            0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110,
        ],
        ':' => [0, 0b00100, 0b00100, 0, 0b00100, 0b00100, 0],
        '-' => [0, 0, 0, 0b11111, 0, 0, 0],
        '.' => [0, 0, 0, 0, 0, 0b00100, 0b00100],
        ',' => [0, 0, 0, 0, 0b00100, 0b00100, 0b01000],
        '(' => [
            0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010,
        ],
        ')' => [
            0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000,
        ],
        '/' => [
            0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000,
        ],
        '?' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0, 0b00100],
        _ => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0, 0b00100],
    }
}

fn find_eyes(image: &Image) -> Vec<Rect> {
    let w = image.w as i32;
    let h = image.h as i32;
    let mut seen = vec![false; (w * h) as usize];
    let mut found = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            if seen[i] || !eye_pixel(image, x, y) {
                continue;
            }
            let mut stack = vec![(x, y)];
            seen[i] = true;
            let mut min_x = x;
            let mut max_x = x;
            let mut min_y = y;
            let mut max_y = y;
            let mut area = 0i32;
            while let Some((cx, cy)) = stack.pop() {
                area += 1;
                min_x = min_x.min(cx);
                max_x = max_x.max(cx);
                min_y = min_y.min(cy);
                max_y = max_y.max(cy);
                for (nx, ny) in [(cx - 1, cy), (cx + 1, cy), (cx, cy - 1), (cx, cy + 1)] {
                    if nx < 0 || ny < 0 || nx >= w || ny >= h {
                        continue;
                    }
                    let ni = (ny * w + nx) as usize;
                    if seen[ni] || !eye_pixel(image, nx, ny) {
                        continue;
                    }
                    seen[ni] = true;
                    stack.push((nx, ny));
                }
            }
            let rect_w = max_x - min_x + 1;
            let rect_h = max_y - min_y + 1;
            let cy = min_y + rect_h / 2;
            // The illustrated eyes are smaller than the old still sprites.
            if area > 200 && cy < (h * 70) / 100 && rect_w > 20 && rect_h > 16 {
                found.push((
                    area,
                    Rect {
                        x: min_x,
                        y: min_y,
                        w: rect_w,
                        h: rect_h,
                    },
                ));
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().take(2).map(|(_, rect)| rect).collect()
}

fn eye_pixel(image: &Image, x: i32, y: i32) -> bool {
    let i = (y as u32 * image.w + x as u32) as usize * 4;
    let (r, g, b, a) = (
        image.rgba[i],
        image.rgba[i + 1],
        image.rgba[i + 2],
        image.rgba[i + 3],
    );
    a > 200 && r > 190 && g > 180 && b > 170
}

fn fill(image: &mut Image, color: [u8; 4]) {
    for px in image.rgba.chunks_exact_mut(4) {
        px.copy_from_slice(&color);
    }
}

fn blit(dst: &mut [u8], dst_w: u32, src: &Image, dx: i32, dy: i32, min_y: i32) {
    for y in 0..src.h as i32 {
        for x in 0..src.w as i32 {
            let out_x = dx + x;
            let out_y = dy + y;
            if out_x < 0 || out_y < min_y || out_x >= dst_w as i32 {
                continue;
            }
            let dst_h = dst.len() / (dst_w as usize * 4);
            if out_y >= dst_h as i32 {
                continue;
            }
            let si = (y as u32 * src.w + x as u32) as usize * 4;
            let di = (out_y as u32 * dst_w + out_x as u32) as usize * 4;
            let sa = src.rgba[si + 3] as u32;
            if sa == 0 {
                continue;
            }
            if sa == 255 {
                dst[di..di + 4].copy_from_slice(&src.rgba[si..si + 4]);
                continue;
            }
            let da = dst[di + 3] as u32;
            let out_a = sa + da * (255 - sa) / 255;
            if out_a == 0 {
                continue;
            }
            for channel in 0..3 {
                let s = src.rgba[si + channel] as u32;
                let d = dst[di + channel] as u32;
                dst[di + channel] = ((s * sa + d * da * (255 - sa) / 255) / out_a) as u8;
            }
            dst[di + 3] = out_a as u8;
        }
    }
}

impl Image {
    fn clone_image(&self) -> Image {
        Image {
            w: self.w,
            h: self.h,
            rgba: self.rgba.clone(),
        }
    }
}

fn scale_width(src: &Image, target_w: u32) -> Image {
    if src.w == 0 || src.h == 0 {
        return Image {
            w: 1,
            h: 1,
            rgba: vec![0, 0, 0, 0],
        };
    }
    let target_w = target_w.max(1);
    let target_h = ((src.h as u64 * target_w as u64) / src.w as u64).max(1) as u32;
    let mut rgba = vec![0u8; target_w as usize * target_h as usize * 4];
    for y in 0..target_h {
        for x in 0..target_w {
            let sx = (x as u64 * src.w as u64) / target_w as u64;
            let sy = (y as u64 * src.h as u64) / target_h as u64;
            let si = (sy as u32 * src.w + sx as u32) as usize * 4;
            let di = (y * target_w + x) as usize * 4;
            rgba[di..di + 4].copy_from_slice(&src.rgba[si..si + 4]);
        }
    }
    Image {
        w: target_w,
        h: target_h,
        rgba,
    }
}

fn decode_png(bytes: &[u8]) -> Image {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().expect("png");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("png frame");
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(buf.len() / 3 * 4);
            for px in buf.chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            out
        }
        other => panic!("sprite color type {other:?}"),
    };
    Image {
        w: info.width,
        h: info.height,
        rgba,
    }
}

fn write_png(path: &Path, scene: &Scene) -> std::io::Result<()> {
    let file = File::create(path)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), scene.width, scene.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&scene.rgba)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(
        sprites: &Sprites,
        pose: &Pose,
        mascot: Mascot,
        frame: usize,
        eyes: bool,
        pointer: Option<(i32, i32)>,
    ) -> Scene {
        render(
            sprites,
            pose,
            mascot,
            frame,
            false,
            false,
            None,
            eyes,
            false,
            pointer,
            u32::MAX,
            0,
        )
    }

    #[test]
    fn animated_frames_replace_the_stills() {
        let sprites = Sprites::load();
        assert_eq!(sprites.idle_eyes[0].len(), 2, "{:?}", sprites.idle_eyes[0]);
        let idle = scene(&sprites, &Pose::Idle, Mascot::Idle, 0, false, None);
        let next = scene(&sprites, &Pose::Idle, Mascot::Idle, 1, false, None);
        assert_ne!(idle.rgba, next.rgba);
        let active = scene(
            &sprites,
            &Pose::Active {
                purpose: "review the desktop".into(),
                verb: Some("view".into()),
                mode: Some("view".into()),
            },
            Mascot::Idle,
            0,
            false,
            None,
        );
        let other = scene(
            &sprites,
            &Pose::Active {
                purpose: "review the desktop".into(),
                verb: Some("listen".into()),
                mode: Some("listen".into()),
            },
            Mascot::Idle,
            0,
            false,
            None,
        );
        assert!(idle.height < active.height);
        assert_ne!(active.rgba, other.rgba);
        assert!(painted_body(&idle));
        assert!(corner_clear(&idle));
        let (ms, count, looped) = sprites.timing(Mascot::Disconnected);
        assert_eq!((ms, count, looped), (110, 6, true));
    }

    #[test]
    fn eyes_follow_the_pointer_only_during_a_session_when_enabled() {
        let sprites = Sprites::load();
        let active = Pose::Active {
            purpose: "review the desktop".into(),
            verb: Some("view".into()),
            mode: Some("view".into()),
        };
        let still = scene(&sprites, &active, Mascot::Idle, 0, false, Some((0, 80)));
        let ignored = scene(&sprites, &active, Mascot::Idle, 0, false, Some((390, 80)));
        assert_eq!(still.rgba, ignored.rgba);
        let left = scene(&sprites, &active, Mascot::Idle, 0, true, Some((0, 80)));
        let right = scene(&sprites, &active, Mascot::Idle, 0, true, Some((390, 200)));
        assert_ne!(left.rgba, right.rgba);
        let band = |scene: &Scene| {
            let rows = 90.min(scene.height as usize);
            let start = (scene.height as usize - rows) * scene.width as usize * 4;
            scene.rgba[start..].to_vec()
        };
        assert_eq!(band(&left), band(&right));
        assert_eq!(band(&left), band(&still));
        let idle_left = scene(&sprites, &Pose::Idle, Mascot::Idle, 0, true, Some((0, 40)));
        let idle_right = scene(
            &sprites,
            &Pose::Idle,
            Mascot::Idle,
            0,
            true,
            Some((390, 40)),
        );
        assert_eq!(idle_left.rgba, idle_right.rgba);
        let visor = scene(
            &sprites,
            &active,
            Mascot::Automation,
            0,
            true,
            Some((0, 80)),
        );
        let visor_other = scene(
            &sprites,
            &active,
            Mascot::Automation,
            0,
            true,
            Some((390, 200)),
        );
        assert_eq!(visor.rgba, visor_other.rgba);
    }

    #[test]
    fn paused_prompt_offers_continue() {
        let sprites = Sprites::load();
        let scene = render(
            &sprites,
            &Pose::Idle,
            Mascot::Idle,
            0,
            false,
            true,
            None,
            false,
            false,
            None,
            u32::MAX,
            0,
        );
        assert!(scene
            .hits
            .iter()
            .any(|hit| hit.action == Action::ContinueAutomation));
        assert!(scene
            .hits
            .iter()
            .any(|hit| hit.action == Action::DismissAutomation));
    }

    fn painted_body(scene: &Scene) -> bool {
        // The beetle sits low on the 400x520 canvas, so a single center pixel
        // can land in the gap above the shell.
        scene.rgba.chunks(4).filter(|px| px[3] > 200).count() > 1_000
    }

    fn corner_clear(scene: &Scene) -> bool {
        scene.rgba[3] == 0
    }

    #[test]
    fn menu_keeps_every_row_on_a_short_display() {
        let sprites = Sprites::load();
        for screen in [768u32, 900] {
            let max_height = screen.saturating_sub(VIEW_MARGIN);
            let top = render(
                &sprites,
                &Pose::Idle,
                Mascot::Idle,
                0,
                true,
                false,
                None,
                false,
                false,
                None,
                max_height,
                0,
            );
            assert!(top.height <= max_height);
            assert!(top.height + VIEW_MARGIN <= screen);
            let open = top
                .hits
                .iter()
                .find(|hit| hit.action == Action::OpenCli)
                .expect("open cli");
            assert_eq!(open.h, row_h());
            assert!(open.y >= 0 && open.y + open.h <= top.height as i32);
            let buried = top
                .hits
                .iter()
                .find(|hit| hit.action == Action::ToggleBugCursor);
            assert!(buried.is_none_or(|hit| hit.h < row_h() || hit.y + hit.h > top.height as i32));
            let bottom = render(
                &sprites,
                &Pose::Idle,
                Mascot::Idle,
                0,
                true,
                false,
                None,
                false,
                false,
                None,
                max_height,
                i32::MAX,
            );
            let bug = bottom
                .hits
                .iter()
                .find(|hit| hit.action == Action::ToggleBugCursor)
                .expect("bug cursor");
            assert_eq!(bug.h, row_h(), "{bug:?}");
            assert!(bug.y >= 0 && bug.y + bug.h <= bottom.height as i32);
        }
    }

    fn row_h() -> i32 {
        // pad 12 * 2 + one 24 px line. Matches text_card for a menu row.
        48
    }
}
