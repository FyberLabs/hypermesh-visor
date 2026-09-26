//! Always-on-top X11 window (XWayland on Wayland). The pixels are `draw::render`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ClientMessageData, ClientMessageEvent, ColormapAlloc,
    ConfigureWindowAux, ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, ImageFormat,
    ImageOrder, PropMode, VisualClass, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use hypermesh_session::{self, KeyringStore};

use crate::auth::{self, Pose};
use crate::credentials;
use crate::draw::{self, Action, Scene, Sprites};
use crate::pages;
use crate::settings;

struct App {
    sprites: Sprites,
    pose: Pose,
    menu: bool,
    eyes_follow: bool,
    status: Arc<Mutex<String>>,
    login_busy: Arc<Mutex<bool>>,
}

pub fn run() -> Result<(), String> {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("hypermesh-companion: DISPLAY is unset; the bug stays off");
        return Ok(());
    }
    let sprites = Sprites::load();
    let app = App {
        sprites,
        pose: Pose::Idle,
        menu: false,
        eyes_follow: settings::eyes_follow_pointer(&credentials::config_dir()),
        status: Arc::new(Mutex::new(String::new())),
        login_busy: Arc::new(Mutex::new(false)),
    };
    show(app)
}

fn show(mut app: App) -> Result<(), String> {
    let (conn, screen_num) = x11rb::connect(None).map_err(|err| err.to_string())?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;
    let (depth, visual_id, red, green, blue) = window_visual(&conn, screen_num);
    let colormap = conn.generate_id().map_err(|err| err.to_string())?;
    conn.create_colormap(ColormapAlloc::NONE, colormap, root, visual_id)
        .map_err(|err| err.to_string())?;
    let window = conn.generate_id().map_err(|err| err.to_string())?;
    let status = app.status.lock().unwrap().clone();
    let mut scene = draw::render(
        &app.sprites,
        &app.pose,
        6,
        app.menu,
        Some(status.as_str()),
        app.eyes_follow,
        None,
    );
    let (x, y) = place(
        screen.width_in_pixels,
        screen.height_in_pixels,
        scene.width,
        scene.height,
    );
    conn.create_window(
        depth,
        window,
        root,
        x,
        y,
        scene.width as u16,
        scene.height as u16,
        0,
        WindowClass::INPUT_OUTPUT,
        visual_id,
        &CreateWindowAux::new()
            .colormap(colormap)
            .border_pixel(0)
            .background_pixel(0)
            .override_redirect(1u32)
            .event_mask(EventMask::BUTTON_PRESS | EventMask::EXPOSURE),
    )
    .map_err(|err| err.to_string())?;
    let gc = conn.generate_id().map_err(|err| err.to_string())?;
    conn.create_gc(gc, window, &CreateGCAux::new())
        .map_err(|err| err.to_string())?;
    let title = b"Hypermesh";
    conn.change_property(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        8,
        title.len() as u32,
        title,
    )
    .map_err(|err| err.to_string())?;
    conn.change_window_attributes(
        window,
        &ChangeWindowAttributesAux::new().background_pixel(0),
    )
    .map_err(|err| err.to_string())?;
    conn.map_window(window).map_err(|err| err.to_string())?;
    raise_above(&conn, screen_num, window)?;
    conn.flush().map_err(|err| err.to_string())?;
    let lsb = conn.setup().image_byte_order == ImageOrder::LSB_FIRST;
    let mut tick = 6u32;
    let mut size = (scene.width, scene.height);
    loop {
        if tick % 4 == 0 {
            app.pose = poll_pose();
        }
        let status = app.status.lock().unwrap().clone();
        let pointer = if app.eyes_follow && matches!(app.pose, Pose::Active { .. }) {
            pointer_in_window(&conn, window)
        } else {
            None
        };
        scene = draw::render(
            &app.sprites,
            &app.pose,
            tick,
            app.menu,
            Some(status.as_str()),
            app.eyes_follow,
            pointer,
        );
        if (scene.width, scene.height) != size {
            let (x, y) = place(
                screen.width_in_pixels,
                screen.height_in_pixels,
                scene.width,
                scene.height,
            );
            conn.configure_window(
                window,
                &ConfigureWindowAux::new()
                    .x(x as i32)
                    .y(y as i32)
                    .width(scene.width)
                    .height(scene.height),
            )
            .map_err(|err| err.to_string())?;
            size = (scene.width, scene.height);
        }
        put_scene(&conn, window, gc, &scene, depth, lsb, red, green, blue)?;
        conn.flush().map_err(|err| err.to_string())?;
        while let Some(event) = conn.poll_for_event().map_err(|err| err.to_string())? {
            if let Event::ButtonPress(press) = event {
                if press.detail == 1 {
                    on_click(&mut app, &scene, press.event_x as i32, press.event_y as i32);
                }
            }
        }
        tick = tick.wrapping_add(1);
        std::thread::sleep(Duration::from_millis(80));
    }
}

fn on_click(app: &mut App, scene: &Scene, x: i32, y: i32) {
    let Some(hit) = scene.hits.iter().find(|hit| hit.contains(x, y)) else {
        return;
    };
    match hit.action {
        Action::ToggleMenu => app.menu = !app.menu,
        Action::OpenCli => {
            app.menu = false;
            if let Err(err) = open_cli() {
                *app.status.lock().unwrap() = err.to_string();
            }
        }
        Action::Login => {
            app.menu = false;
            start_login(Arc::clone(&app.status), Arc::clone(&app.login_busy));
        }
        Action::OpenBilling => {
            app.menu = false;
            let _ = open_url(pages::BILLING_URL);
        }
        Action::OpenRent => {
            app.menu = false;
            let _ = open_url(pages::RENT_URL);
        }
        Action::OpenDashboard => {
            app.menu = false;
            let _ = open_url(pages::DASHBOARD_URL);
        }
        Action::ToggleEyes => {
            app.eyes_follow = !app.eyes_follow;
            if let Err(err) =
                settings::set_eyes_follow_pointer(&credentials::config_dir(), app.eyes_follow)
            {
                *app.status.lock().unwrap() = err;
            }
        }
        Action::McpProfiles => {
            app.menu = false;
            if let Err(err) = open_hypermesh_args(&["mcp", "profile", "ls"]) {
                *app.status.lock().unwrap() = err.to_string();
            }
        }
        Action::McpDoctor => {
            app.menu = false;
            if let Err(err) = open_hypermesh_args(&["mcp", "doctor"]) {
                *app.status.lock().unwrap() = err.to_string();
            }
        }
    }
}

fn pointer_in_window(conn: &RustConnection, window: u32) -> Option<(i32, i32)> {
    let reply = conn.query_pointer(window).ok()?.reply().ok()?;
    if !reply.same_screen {
        return None;
    }
    Some((i32::from(reply.win_x), i32::from(reply.win_y)))
}

fn start_login(status: Arc<Mutex<String>>, busy: Arc<Mutex<bool>>) {
    {
        let mut flag = busy.lock().unwrap();
        if *flag {
            return;
        }
        *flag = true;
    }
    *status.lock().unwrap() = "waiting for the browser".into();
    std::thread::spawn(move || {
        let result = hypermesh_session::sign_in(
            &KeyringStore,
            &hypermesh_session::Endpoints::panopticon(),
            false,
            hypermesh_session::display_available(),
            |url| hypermesh_session::open_system_browser(url),
            |device| {
                *status.lock().unwrap() =
                    format!("Enter {} at {}", device.user_code, device.verification_uri);
            },
            |access| {
                if let Ok(tenant) = hypermesh_session::first_tenant(pages::API_BASE, access) {
                    let _ = credentials::write_profile(&credentials::config_dir(), &tenant, "");
                }
                Ok(())
            },
        );
        match result {
            Ok(()) => {
                let _ = credentials::forget_plaintext_credentials(&credentials::config_dir());
                *status.lock().unwrap() = "logged in".into();
            }
            Err(err) => *status.lock().unwrap() = err.to_string(),
        }
        *busy.lock().unwrap() = false;
    });
}

fn open_url(url: &str) -> std::io::Result<()> {
    std::process::Command::new("xdg-open")
        .arg(url)
        .spawn()
        .map(|_| ())
}

fn open_cli() -> std::io::Result<()> {
    open_hypermesh_args(&[])
}

fn open_hypermesh_args(extra: &[&str]) -> std::io::Result<()> {
    let mut cmd = vec!["hypermesh".to_string()];
    for a in extra {
        cmd.push((*a).to_string());
    }
    let joined = shell_join(&cmd);
    let terminals: &[(&str, Vec<String>)] = &[
        (
            "x-terminal-emulator",
            vec!["-e".into(), "sh".into(), "-c".into(), joined.clone()],
        ),
        (
            "gnome-terminal",
            vec!["--".into(), "sh".into(), "-c".into(), joined.clone()],
        ),
        (
            "konsole",
            vec!["-e".into(), "sh".into(), "-c".into(), joined.clone()],
        ),
        (
            "xfce4-terminal",
            vec!["-e".into(), "sh".into(), "-c".into(), joined.clone()],
        ),
        ("kitty", {
            let mut v = vec!["sh".into(), "-c".into()];
            v.push(joined.clone());
            v
        }),
        (
            "alacritty",
            vec!["-e".into(), "sh".into(), "-c".into(), joined.clone()],
        ),
        (
            "xterm",
            vec!["-e".into(), "sh".into(), "-c".into(), joined],
        ),
    ];
    for (bin, args) in terminals {
        if std::process::Command::new(bin).args(args).spawn().is_ok() {
            return Ok(());
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no terminal emulator found for hypermesh",
    ))
}

fn shell_join(parts: &[String]) -> String {
    parts
        .iter()
        .map(|p| {
            if p.chars().any(|c| c.is_whitespace()) {
                format!("'{p}'")
            } else {
                p.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn poll_pose() -> Pose {
    let request = auth::visor_request("127.0.0.1:9847");
    let Ok(mut stream) = TcpStream::connect_timeout(
        &"127.0.0.1:9847".parse().unwrap(),
        Duration::from_millis(150),
    ) else {
        return Pose::Idle;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    if stream.write_all(request.as_bytes()).is_err() {
        return Pose::Idle;
    }
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.len() > 64 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let Some((_, body)) = text.split_once("\r\n\r\n") else {
        return Pose::Idle;
    };
    auth::pose_from_companion(body)
}

fn place(screen_w: u16, screen_h: u16, width: u32, height: u32) -> (i16, i16) {
    let x = screen_w.saturating_sub(width as u16).saturating_sub(28);
    let y = screen_h.saturating_sub(height as u16).saturating_sub(48);
    (x as i16, y as i16)
}

fn window_visual(conn: &RustConnection, screen_num: usize) -> (u8, u32, u32, u32, u32) {
    if let Some(visual) = argb_visual(conn, screen_num) {
        return visual;
    }
    let screen = &conn.setup().roots[screen_num];
    for depth in &screen.allowed_depths {
        for visual in &depth.visuals {
            if visual.visual_id == screen.root_visual {
                return (
                    depth.depth,
                    visual.visual_id,
                    visual.red_mask,
                    visual.green_mask,
                    visual.blue_mask,
                );
            }
        }
    }
    (
        screen.root_depth,
        screen.root_visual,
        0x00ff_0000,
        0x0000_ff00,
        0x0000_00ff,
    )
}

fn argb_visual(conn: &RustConnection, screen_num: usize) -> Option<(u8, u32, u32, u32, u32)> {
    let screen = &conn.setup().roots[screen_num];
    for depth in &screen.allowed_depths {
        if depth.depth != 32 {
            continue;
        }
        for visual in &depth.visuals {
            if visual.class == VisualClass::TRUE_COLOR {
                return Some((
                    32,
                    visual.visual_id,
                    visual.red_mask,
                    visual.green_mask,
                    visual.blue_mask,
                ));
            }
        }
    }
    None
}

fn raise_above(conn: &RustConnection, screen_num: usize, window: u32) -> Result<(), String> {
    let root = conn.setup().roots[screen_num].root;
    let wm_state = intern(conn, b"_NET_WM_STATE")?;
    let above = intern(conn, b"_NET_WM_STATE_ABOVE")?;
    let skip = intern(conn, b"_NET_WM_STATE_SKIP_TASKBAR")?;
    let event = ClientMessageEvent {
        response_type: x11rb::protocol::xproto::CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window,
        type_: wm_state,
        data: ClientMessageData::from([1, above, skip, 0, 0]),
    };
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        event,
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

fn intern(conn: &RustConnection, name: &[u8]) -> Result<u32, String> {
    Ok(conn
        .intern_atom(false, name)
        .map_err(|err| err.to_string())?
        .reply()
        .map_err(|err| err.to_string())?
        .atom)
}

fn put_scene(
    conn: &RustConnection,
    window: u32,
    gc: u32,
    scene: &Scene,
    depth: u8,
    lsb: bool,
    red: u32,
    green: u32,
    blue: u32,
) -> Result<(), String> {
    let bpp = if depth == 32 { 4 } else { 4 };
    let mut data = Vec::with_capacity(scene.rgba.len());
    let opaque = depth != 32;
    for px in scene.rgba.chunks_exact(4) {
        let (r, g, b, mut a) = (px[0], px[1], px[2], px[3]);
        if opaque && a < 255 {
            // No alpha visual: sit the bug on a dark card instead of a hole.
            let t = a as u32;
            let mix = |c: u8, bg: u8| ((c as u32 * t + bg as u32 * (255 - t)) / 255) as u8;
            let r = mix(r, 18);
            let g = mix(g, 22);
            let b = mix(b, 28);
            a = 255;
            data.extend_from_slice(&pack(r, g, b, a, red, green, blue, lsb, bpp));
        } else {
            data.extend_from_slice(&pack(r, g, b, a, red, green, blue, lsb, bpp));
        }
    }
    conn.put_image(
        ImageFormat::Z_PIXMAP,
        window,
        gc,
        scene.width as u16,
        scene.height as u16,
        0,
        0,
        0,
        depth,
        &data,
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

fn pack(
    r: u8,
    g: u8,
    b: u8,
    a: u8,
    red: u32,
    green: u32,
    blue: u32,
    lsb: bool,
    bpp: usize,
) -> [u8; 4] {
    let shift = |mask: u32| mask.trailing_zeros();
    let mut pixel = 0u32;
    if red != 0 {
        pixel |= (r as u32) << shift(red);
    }
    if green != 0 {
        pixel |= (g as u32) << shift(green);
    }
    if blue != 0 {
        pixel |= (b as u32) << shift(blue);
    }
    let used = red | green | blue;
    let alpha = (!used) & 0xffff_ffff;
    if alpha != 0 && bpp == 4 {
        pixel |= (a as u32) << shift(alpha);
    }
    if lsb {
        pixel.to_le_bytes()
    } else {
        pixel.to_be_bytes()
    }
}
