//! Always-on-top X11 window (XWayland on Wayland). The pixels are `draw::render`.
//! Leaving the loop, including a panic unwind, puts the system cursor back.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ClientMessageData, ClientMessageEvent, ColormapAlloc,
    ConfigureWindowAux, ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, ImageFormat,
    ImageOrder, PropMode, VisualClass, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use hypermesh_session::{self, KeyringStore};

use crate::auth::{Pose, Snapshot};
use crate::automation::{self, CursorPose, Effect, Machine, Motion, Phase, VisorPhase};
use crate::credentials;
use crate::draw::{self, Action, Scene, Sprites};
use crate::mascot::{self, Mascot, Reel};
use crate::pages;
use crate::pointer::{self, BugCursor, LiveInput, Watch};
use crate::settings;
use crate::visor::Poller;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_stop(_: i32) {
    STOP.store(true, Ordering::SeqCst);
}

struct App {
    sprites: Sprites,
    pose: Pose,
    menu: bool,
    eyes_follow: bool,
    bug_cursor: bool,
    sleep_after_ms: u64,
    status: Arc<Mutex<String>>,
    login_busy: Arc<Mutex<bool>>,
    machine: Machine,
    reel: Reel,
    poller: Poller,
    last_used: Instant,
    started: Instant,
    pending: Option<&'static str>,
    pointer_sample: Option<(i32, i32, u64)>,
    click_at: Option<u64>,
    key_at: Option<u64>,
    /// Resume sequence on the last connected snapshot.
    seen_resume_seq: u64,
    /// Resume sequence on the snapshot before that.
    resume_seq_prior: u64,
    /// Drives at or below this resume sequence were recorded before the last
    /// resume. `None` until a resume happens.
    fallback_after_resume: Option<u64>,
    /// Pointer fallback: baseline, button mask, and the in-flight injection.
    fallback: automation::FallbackTrack,
    /// When the companion first showed the current drive sample.
    sample_age: automation::SampleAge,
    /// Menu and continue-prompt scroll, in pixels.
    scroll: i32,
}

pub fn run() -> Result<(), String> {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("hypermesh-companion: DISPLAY is unset; the bug stays off");
        return Ok(());
    }
    let config = credentials::config_dir();
    let sprites = Sprites::load();
    let app = App {
        sprites,
        pose: Pose::Idle,
        menu: false,
        eyes_follow: settings::eyes_follow_pointer(&config),
        bug_cursor: settings::bug_cursor(&config),
        sleep_after_ms: settings::sleep_after_secs(&config).saturating_mul(1000),
        status: Arc::new(Mutex::new(String::new())),
        login_busy: Arc::new(Mutex::new(false)),
        machine: Machine::new(settings::automation_resume_secs(&config).saturating_mul(1000)),
        reel: Reel::new(Mascot::Disconnected),
        poller: Poller::new(),
        last_used: Instant::now(),
        started: Instant::now(),
        pending: None,
        pointer_sample: None,
        click_at: None,
        key_at: None,
        seen_resume_seq: 0,
        resume_seq_prior: 0,
        fallback_after_resume: None,
        fallback: automation::FallbackTrack::default(),
        sample_age: automation::SampleAge::default(),
        scroll: 0,
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
        Mascot::Disconnected,
        0,
        app.menu,
        false,
        Some(status.as_str()),
        app.eyes_follow,
        app.bug_cursor,
        None,
        u32::from(screen.height_in_pixels)
            .saturating_sub(draw::VIEW_MARGIN)
            .max(1),
        0,
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
    arm_signals();
    let lsb = conn.setup().image_byte_order == ImageOrder::LSB_FIRST;
    let mut size = (scene.width, scene.height);
    let mut watch = Watch::attach(&conn, root);
    let art = pointer::Art::load();
    let mut guard = CursorGuard {
        conn: &conn,
        cursor: BugCursor::new(root, window, depth, visual_id, colormap, red, green, blue),
    };
    let mut last_frame = Instant::now();
    loop {
        if STOP.load(Ordering::SeqCst) {
            break;
        }
        let now_ms = app.started.elapsed().as_millis() as u64;
        let dt = last_frame.elapsed().as_millis() as u64;
        last_frame = Instant::now();
        let (snap, epoch) = app.poller.snapshot();
        let sample_age_ms = snap.drive.as_ref().map_or(0, |drive| {
            app.sample_age
                .observe(drive.inject_seq, drive.at_ms, now_ms)
        });
        let visor_phase = if snap.connected {
            snap.phase
        } else {
            VisorPhase::Off
        };
        let phase_before = app.machine.phase();
        if snap.connected {
            app.machine.see_visor(visor_phase, now_ms, snap.resume_seq);
        } else {
            app.machine.note_disconnect();
        }
        if snap.connected {
            let prior = app.seen_resume_seq;
            if phase_before == Phase::Paused && app.machine.phase() == Phase::Running {
                // This snapshot already carries the post-resume sequence. A
                // drive recorded between the resume and this poll carries it
                // too, so the cutoff stays on the sequence from before.
                app.fallback_after_resume = Some(prior);
            }
            app.resume_seq_prior = prior;
            app.seen_resume_seq = snap.resume_seq;
        }
        if app.machine.phase() == Phase::Off {
            app.pending = None;
            app.fallback_after_resume = None;
        }
        let was_paused = app.machine.phase() == Phase::Paused;
        watch.sync_backend(
            snap.connected,
            matches!(snap.backend, crate::auth::InjectionBackend::Wayland),
        );
        let mut click: Option<(i32, i32)> = None;
        while let Some(event) = conn.poll_for_event().map_err(|err| err.to_string())? {
            if let Some(live) = watch.observe(&event) {
                let effect = app.machine.user_input(now_ms);
                note_effect(&mut app, effect);
                app.last_used = Instant::now();
                match live {
                    LiveInput::Button => app.click_at = Some(now_ms),
                    LiveInput::Key => app.key_at = Some(now_ms),
                    LiveInput::Motion => {}
                }
            }
            if let Event::ButtonPress(press) = event {
                if press.event != window {
                    continue;
                }
                match press.detail {
                    4 | 5 if app.menu || app.machine.asking() => {
                        let step = draw::MENU_SCROLL_STEP;
                        if press.detail == 4 {
                            app.scroll = app.scroll.saturating_sub(step);
                        } else {
                            app.scroll = app.scroll.saturating_add(step);
                        }
                        app.scroll = draw::clamp_scroll(app.scroll, scene.scroll_max);
                    }
                    1 => click = Some((i32::from(press.event_x), i32::from(press.event_y))),
                    _ => {}
                }
            }
        }
        if !watch.accepts_raw() {
            if let Some((x, y, mask)) = pointer::root_pointer_state(&conn, root) {
                let drive = snap.drive.as_ref().map(|drive| automation::FallbackDrive {
                    x: drive.x,
                    y: drive.y,
                    at_ms: drive.at_ms,
                    pointer_known: drive.pointer_known,
                    resume_seq: drive.resume_seq,
                    inject_seq: drive.inject_seq,
                });
                let input = automation::FallbackInput {
                    phase: app.machine.phase(),
                    x,
                    y,
                    mask,
                    now_ms,
                    sample_age_ms,
                    epoch,
                    injecting: snap.injecting,
                    inject_seq: snap.inject_seq,
                    last_settled_seq: snap.last_settled_seq,
                    drive,
                    after_resume: app.fallback_after_resume,
                };
                if automation::classify_fallback(&input, &mut app.fallback) {
                    let effect = app.machine.user_input(now_ms);
                    note_effect(&mut app, effect);
                    app.last_used = Instant::now();
                }
            }
        } else {
            app.fallback = automation::FallbackTrack::default();
        }
        if let Some((x, y)) = click {
            app.last_used = Instant::now();
            on_click(&mut app, &scene, x, y, was_paused, now_ms);
        }
        let effect = app.machine.poll(now_ms);
        note_effect(&mut app, effect);
        if let Some(action) = app.pending.take() {
            app.poller.enqueue(action);
        }
        if !app.menu && !app.machine.asking() {
            app.scroll = 0;
        }
        let mascot = mascot_now(&app, &snap);
        app.reel.set_state(mascot);
        let (frame_ms, count, looped) = app.sprites.timing(mascot);
        app.reel.advance(dt, frame_ms, count, looped);
        let status = display_status(&app);
        let pointer = if app.eyes_follow && matches!(app.pose, Pose::Active { .. }) {
            pointer_in_window(&conn, window)
        } else {
            None
        };
        scene = draw::render(
            &app.sprites,
            &app.pose,
            mascot,
            app.reel.frame(),
            app.menu && !app.machine.asking(),
            app.machine.asking(),
            Some(status.as_str()),
            app.eyes_follow,
            app.bug_cursor,
            pointer,
            u32::from(screen.height_in_pixels)
                .saturating_sub(draw::VIEW_MARGIN)
                .max(1),
            app.scroll,
        );
        app.scroll = draw::clamp_scroll(app.scroll, scene.scroll_max);
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
        sync_cursor(
            &mut app,
            &mut guard,
            &art,
            &snap,
            &conn,
            root,
            now_ms,
            sample_age_ms,
        )?;
        conn.flush().map_err(|err| err.to_string())?;
        std::thread::sleep(Duration::from_millis(80));
    }
    Ok(())
}

struct CursorGuard<'a> {
    conn: &'a RustConnection,
    cursor: BugCursor,
}

impl Drop for CursorGuard<'_> {
    fn drop(&mut self) {
        self.cursor.restore(self.conn);
    }
}

fn arm_signals() {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_stop as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGHUP, &action, std::ptr::null_mut());
    }
}

fn note_effect(app: &mut App, effect: Effect) {
    match effect {
        Effect::Pause => app.pending = Some("pause"),
        Effect::Resume => {
            app.pending = Some("resume");
            // The snapshot can still carry the pre-pause drive until the next
            // poll. Ignore that sequence. If this frame's snapshot is already
            // the post-resume one, `resume_seq_prior` is the sequence before
            // it, and the minimum keeps a drive recorded after the resume.
            app.fallback_after_resume = Some(automation::resume_cutoff(
                app.resume_seq_prior,
                app.seen_resume_seq,
            ));
        }
        Effect::None => {}
    }
}

fn mascot_now(app: &App, snap: &Snapshot) -> Mascot {
    let session_idle = if snap.connected { snap.idle_ms } else { None };
    let unused = mascot::unused_for_ms(app.last_used.elapsed().as_millis() as u64, session_idle);
    mascot::choose(&mascot::Signals {
        connected: snap.connected,
        upgrading: snap.upgrading || settings::upgrading_notice(&credentials::config_dir()),
        automation_running: app.machine.phase() == Phase::Running,
        unused_for_ms: unused,
        sleep_after_ms: app.sleep_after_ms,
    })
}

fn display_status(app: &App) -> String {
    if app.machine.phase() == Phase::Paused {
        return "Paused. Click the bug to continue.".into();
    }
    app.status.lock().unwrap().clone()
}

fn sync_cursor(
    app: &mut App,
    guard: &mut CursorGuard<'_>,
    art: &pointer::Art,
    snap: &Snapshot,
    conn: &RustConnection,
    root: u32,
    now_ms: u64,
    sample_age_ms: u64,
) -> Result<(), String> {
    app.pose = if snap.connected {
        snap.pose.clone()
    } else {
        Pose::Idle
    };
    let opt_in = app.bug_cursor && snap.connected;
    let motion = if app.machine.phase() == Phase::Running {
        snap.drive.as_ref().map(|drive| Motion {
            kind: drive.kind,
            age_ms: sample_age_ms,
            distance_px: drive.distance_px,
            elapsed_ms: drive.elapsed_ms,
            clickable: drive.clickable,
        })
    } else if opt_in {
        local_motion(app, conn, root, now_ms)
    } else {
        None
    };
    let pose = automation::cursor_pose(app.machine.phase(), opt_in, motion);
    if pose == CursorPose::System || !snap.connected {
        guard.cursor.restore(conn);
        return Ok(());
    }
    let age = motion.map(|item| item.age_ms).unwrap_or(now_ms);
    let frame = automation::cursor_frame(pose, age);
    if guard.cursor.shown() == Some((pose, frame)) {
        if let Some((x, y)) = pointer::root_pointer(conn, root) {
            guard.cursor.follow(conn, x, y);
        }
        return Ok(());
    }
    let image = art.frame(automation::cursor_frame_name(pose, frame));
    if let Err(err) = guard.cursor.apply(conn, image, pose, frame) {
        eprintln!("hypermesh-companion: cursor: {err}");
        guard.cursor.restore(conn);
    } else if let Some((x, y)) = pointer::root_pointer(conn, root) {
        guard.cursor.follow(conn, x, y);
    }
    Ok(())
}

fn local_motion(app: &mut App, conn: &RustConnection, root: u32, now_ms: u64) -> Option<Motion> {
    if let Some(at) = app.click_at {
        if now_ms.saturating_sub(at) < automation::CLICK_FRESH_MS {
            return Some(Motion {
                kind: automation::MotionKind::Click,
                age_ms: now_ms.saturating_sub(at),
                distance_px: 0,
                elapsed_ms: 0,
                clickable: false,
            });
        }
    }
    if let Some(at) = app.key_at {
        if now_ms.saturating_sub(at) < automation::TYPE_FRESH_MS {
            return Some(Motion {
                kind: automation::MotionKind::Type,
                age_ms: now_ms.saturating_sub(at),
                distance_px: 0,
                elapsed_ms: 0,
                clickable: false,
            });
        }
    }
    let Some((x, y)) = pointer::root_pointer(conn, root) else {
        return None;
    };
    let moved = if let Some((px, py, at)) = app.pointer_sample {
        let dx = (x - px).unsigned_abs();
        let dy = (y - py).unsigned_abs();
        let distance = ((u64::from(dx) * u64::from(dx) + u64::from(dy) * u64::from(dy)) as f64)
            .sqrt()
            .round() as u32;
        let elapsed = now_ms.saturating_sub(at).min(u32::MAX as u64) as u32;
        app.pointer_sample = Some((x, y, now_ms));
        if distance >= 4 {
            Some(Motion {
                kind: automation::MotionKind::Move,
                age_ms: 0,
                distance_px: distance,
                elapsed_ms: elapsed.max(1),
                clickable: false,
            })
        } else {
            None
        }
    } else {
        app.pointer_sample = Some((x, y, now_ms));
        None
    };
    moved
}

fn on_click(app: &mut App, scene: &Scene, x: i32, y: i32, was_paused: bool, now_ms: u64) {
    let Some(hit) = scene.hits.iter().find(|hit| hit.contains(x, y)) else {
        return;
    };
    match hit.action {
        Action::ToggleMenu => {
            if was_paused {
                app.machine.ask(now_ms);
            } else if app.machine.phase() == Phase::Running {
                let effect = app.machine.user_input(now_ms);
                note_effect(app, effect);
            } else if app.machine.phase() == Phase::Paused {
                // This click is the one that paused the automation.
            } else {
                app.menu = !app.menu;
            }
        }
        Action::ContinueAutomation => {
            let effect = app.machine.continue_now(now_ms);
            note_effect(app, effect);
        }
        Action::DismissAutomation => app.machine.dismiss(now_ms),
        Action::ToggleBugCursor => {
            app.bug_cursor = !app.bug_cursor;
            if let Err(err) = settings::set_bug_cursor(&credentials::config_dir(), app.bug_cursor) {
                *app.status.lock().unwrap() = err;
            }
        }
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
        ("xterm", vec!["-e".into(), "sh".into(), "-c".into(), joined]),
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
