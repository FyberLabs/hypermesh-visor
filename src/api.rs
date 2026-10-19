use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::Engine;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::automation::{self, Control, Notice, Phase};
use crate::desktop::{AudioRead, Desktop, DesktopError, KeyboardPipe, AUDIO_FORMAT};
use crate::harness::{
    Agent, Harness, HarnessError, Recipe, Skill, SESSION_LIST_MAX, SESSION_TEXT_MAX,
};
use crate::input::{
    drag_point_count, drag_steps, keys_activity, keystroke_count, mouse_activity, mouse_in_bounds,
    plan_type, take_keystrokes, Button, MouseOp, PlanError, PointerStep, Stroke, TypeBody,
    POINTER_BATCH, TYPE_CHUNK_KEYSTROKES, TYPE_MAX_KEYSTROKES,
};
use crate::mcp::{
    self, companion_mode, resolve_preferred, BindingCache, FocusedApp, McpAuditRow, McpAuditor,
    McpError, ToolInfo,
};
use crate::prompt::{self, AuditRecord, PromptError};
use crate::session::{Session, Verb};
use crate::stream::{self, StreamError};
use crate::vault::{SecretRequest, Vault, VaultError};

pub struct AppState {
    sessions: Mutex<HashMap<Uuid, Session>>,
    desktop: Arc<dyn Desktop>,
    watch_poll: Duration,
    prompts: prompt::PromptPass,
    /// Directory for mcp.json / mcp-profiles.json. Defaults to the Hypermesh config dir.
    mcp_dir: PathBuf,
    mcp_audit: Mutex<McpAuditor>,
    /// Latest view/watch frame per session. The feed serves this cache.
    /// It does not call `Desktop::view` again.
    frames: Mutex<HashMap<Uuid, CachedFrame>>,
    /// Driving phase for the desktop companion. Mouse and type enter it.
    automation: Mutex<automation::Gate>,
    /// Held across a desktop injection. Pause, stop, and the gate snapshot
    /// do not take this lock, and the gate lock is never held across a
    /// desktop call.
    injection: Mutex<()>,
    /// Lock-free copy of the gate's epoch and phase. Only [`automation::Gate`]
    /// methods store it. Injection threads load it between chunks.
    published: Arc<AtomicU64>,
    /// Last focused app. `GET /companion` must not probe the compositor on
    /// every poll; a Wayland probe spawns a helper process.
    focus_cache: Arc<Mutex<FocusCache>>,
    focus_refreshing: Arc<AtomicBool>,
    /// Chosen once per process. A restart is a new value, published on
    /// `GET /companion` so sequence numbers from the previous process are
    /// not compared with this one.
    instance: u64,
    /// `mcp-bindings.json`, refreshed outside the sessions lock.
    bindings: Mutex<BindingCache>,
    /// How long `running` may continue without a companion-socket poll.
    companion_poll_timeout: Duration,
}

/// A running automation pauses when the companion socket has not polled
/// within this long. `HYPERMESH_COMPANION_POLL_MS` overrides it (200–60000).
pub const COMPANION_POLL_TIMEOUT: Duration = Duration::from_secs(2);

pub fn companion_poll_timeout_from(value: Option<&str>) -> Duration {
    let Some(text) = value.map(str::trim).filter(|text| !text.is_empty()) else {
        return COMPANION_POLL_TIMEOUT;
    };
    match text.parse::<u64>() {
        Ok(ms) if (200..=60_000).contains(&ms) => Duration::from_millis(ms),
        _ => COMPANION_POLL_TIMEOUT,
    }
}

struct FocusCache {
    fetched_at: Option<Instant>,
    value: Option<FocusedApp>,
}

/// A focused-app sample younger than this is reused. The companion polls much
/// faster than a compositor probe needs to run.
const FOCUS_TTL: Duration = Duration::from_millis(750);

pub(crate) fn focus_fresh(age: Duration, ttl: Duration) -> bool {
    age < ttl
}

struct CachedFrame {
    mime: String,
    bytes: Vec<u8>,
    handle: String,
    seq: u64,
}

impl AppState {
    pub fn new(desktop: Arc<dyn Desktop>, watch_poll: Duration) -> Self {
        Self::assemble(
            desktop,
            watch_poll,
            prompt::PromptPass::unconfigured(),
            mcp::config_dir(),
        )
    }

    pub fn with_orchestrator(
        desktop: Arc<dyn Desktop>,
        watch_poll: Duration,
        config: prompt::DoorConfig,
    ) -> Result<Self, String> {
        Ok(Self::assemble(
            desktop,
            watch_poll,
            prompt::PromptPass::open(&config)?,
            mcp::config_dir(),
        ))
    }

    /// `door` is one cloud-agent pass-through. The desktop daemon uses
    /// [`prompt::PromptPass::unconfigured`] unless a supervisor URL is set.
    pub fn with_prompt_door(
        desktop: Arc<dyn Desktop>,
        watch_poll: Duration,
        door: Arc<dyn prompt::PromptDoor>,
    ) -> Self {
        Self::assemble(
            desktop,
            watch_poll,
            prompt::PromptPass::new(door),
            mcp::config_dir(),
        )
    }

    fn assemble(
        desktop: Arc<dyn Desktop>,
        watch_poll: Duration,
        prompts: prompt::PromptPass,
        mcp_dir: PathBuf,
    ) -> Self {
        let gate = automation::Gate::default();
        let published = gate.snapshot_handle();
        Self {
            sessions: Mutex::new(HashMap::new()),
            desktop,
            watch_poll,
            prompts,
            mcp_dir,
            mcp_audit: Mutex::new(McpAuditor::default()),
            frames: Mutex::new(HashMap::new()),
            automation: Mutex::new(gate),
            injection: Mutex::new(()),
            published,
            focus_cache: Arc::new(Mutex::new(FocusCache {
                fetched_at: None,
                value: None,
            })),
            focus_refreshing: Arc::new(AtomicBool::new(false)),
            instance: automation::fresh_instance(),
            bindings: Mutex::new(BindingCache::default()),
            companion_poll_timeout: companion_poll_timeout_from(
                std::env::var("HYPERMESH_COMPANION_POLL_MS").ok().as_deref(),
            ),
        }
    }

    fn close_all(&self) {
        self.shutdown(automation::SHUTDOWN_INJECTION_WAIT);
    }

    /// Bump the epoch so a live injection cancels at its next chunk, wait up
    /// to `bound` for the injection lock, then reset even if that call is
    /// still inside a desktop request.
    fn shutdown(&self, bound: Duration) {
        lock(&self.automation).shutdown_cancel();
        let _held = lock_for(&self.injection, bound);
        lock(&self.sessions).clear();
        lock(&self.frames).clear();
        lock(&self.automation).reset();
    }

    fn has_session(&self, id: Uuid) -> bool {
        lock(&self.sessions).contains_key(&id)
    }

    fn note(&self, id: Uuid, kind: &'static str, text: &str) {
        let mut sessions = lock(&self.sessions);
        let Some(session) = sessions.get_mut(&id) else {
            return;
        };
        session.note(kind, text);
    }

    fn note_focus(&self, id: Uuid, label: &str) {
        let mut sessions = lock(&self.sessions);
        let Some(session) = sessions.get_mut(&id) else {
            return;
        };
        session.note_focus(label);
    }

    /// Stores the frame `view` or `watch` already captured.
    fn remember_frame(&self, id: Uuid, mime: String, bytes: Vec<u8>) {
        let handle = format!("{:016x}", hash_frame(&bytes));
        let mut frames = lock(&self.frames);
        let seq = match frames.get(&id) {
            Some(prev) if prev.handle == handle => prev.seq,
            Some(prev) => prev.seq.saturating_add(1),
            None => 1,
        };
        frames.insert(
            id,
            CachedFrame {
                mime,
                bytes,
                handle,
                seq,
            },
        );
    }

    fn mark_verb(&self, id: Uuid, verb: Verb) -> bool {
        let mut sessions = lock(&self.sessions);
        let Some(session) = sessions.get_mut(&id) else {
            return false;
        };
        session.mark_verb(verb);
        true
    }

    fn deliver_stream(&self, id: Uuid, body: &str, api_key: &str) -> Result<String, StreamError> {
        let (parked, mut secrets) = {
            let mut sessions = lock(&self.sessions);
            let session = sessions.get_mut(&id).ok_or(StreamError::Closed)?;
            let parked = stream::deliver(session, body)?;
            let secrets = stream::vault_needles(session);
            (parked, secrets)
        };
        let prompts = parked
            .prompts
            .iter()
            .map(|prompt| (prompt.prompt.clone(), prompt.model.clone()))
            .collect::<Vec<_>>();
        self.prompts
            .drive_parked(&prompts, &parked.file_texts, api_key, &secrets);
        for secret in &mut secrets {
            secret.zeroize();
        }
        stream::render(&parked.acks)
    }

    fn session_inbox(&self, id: Uuid) -> Option<stream::Inbox> {
        lock(&self.sessions).get(&id).map(stream::inbox)
    }

    /// Purpose, verb, and prefer-MCP mode for the desktop companion.
    /// The CLI auth key is not a session secret and is not included.
    fn companion_session(&self) -> Option<CompanionSession> {
        let driving = {
            let gate = lock(&self.automation);
            if gate.phase() == Phase::Off {
                None
            } else {
                gate.session()
            }
        };
        let chosen = {
            let sessions = lock(&self.sessions);
            let session = match driving {
                Some(id) => sessions.get(&id),
                None => None,
            }
            .or_else(|| sessions.values().max_by_key(|session| session.touched()))?;
            ChosenSession {
                id: session.id(),
                purpose: clip_text(session.purpose(), SESSION_TEXT_MAX),
                verb: session.verb().map(Verb::as_str),
                mcp_servers: session.mcp().server_names(),
                healthy: session.mcp().healthy_server_names(),
                idle_ms: u64::try_from(session.touched().elapsed().as_millis()).unwrap_or(u64::MAX),
            }
        };
        let focused = self.cached_focus();
        let bindings = lock(&self.bindings).current(&self.mcp_dir, Instant::now());
        let preferred = focused
            .as_ref()
            .and_then(|app| resolve_preferred(app, &bindings, &chosen.healthy));
        let mode = companion_mode(preferred.as_deref(), chosen.verb)
            .map(|mode| clip_text(&mode, SESSION_TEXT_MAX));
        let prefer_mcp = preferred.is_some();
        let mcp_servers = chosen
            .mcp_servers
            .into_iter()
            .take(SESSION_LIST_MAX)
            .map(|name| clip_text(&name, SESSION_TEXT_MAX))
            .collect();
        let focused = focused.map(|app| FocusedApp {
            wm_class: app
                .wm_class
                .map(|value| clip_text(&value, SESSION_TEXT_MAX)),
            app_id: app.app_id.map(|value| clip_text(&value, SESSION_TEXT_MAX)),
            executable: app
                .executable
                .map(|value| clip_text(&value, SESSION_TEXT_MAX)),
        });
        Some(CompanionSession {
            id: chosen.id,
            purpose: chosen.purpose,
            verb: chosen.verb,
            mode,
            mcp: preferred.map(|name| clip_text(&name, SESSION_TEXT_MAX)),
            mcp_servers,
            focused,
            prefer_mcp,
            idle_ms: chosen.idle_ms,
        })
    }

    /// The last focused app, refreshing it off the request worker when the
    /// sample is missing or older than [`FOCUS_TTL`]. The poll returns the
    /// previous sample immediately.
    fn cached_focus(&self) -> Option<FocusedApp> {
        let cached = {
            let cache = lock(&self.focus_cache);
            let fresh = cache
                .fetched_at
                .is_some_and(|at| focus_fresh(at.elapsed(), FOCUS_TTL));
            if fresh {
                return cache.value.clone().filter(|app| !app.is_empty());
            }
            cache.value.clone()
        };
        self.spawn_focus_refresh();
        cached.filter(|app| !app.is_empty())
    }

    fn spawn_focus_refresh(&self) {
        if self.focus_refreshing.swap(true, Ordering::SeqCst) {
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            self.focus_refreshing.store(false, Ordering::SeqCst);
            return;
        }
        let desktop = Arc::clone(&self.desktop);
        let cache = Arc::clone(&self.focus_cache);
        let flag = Arc::clone(&self.focus_refreshing);
        tokio::spawn(async move {
            struct Clear(Arc<AtomicBool>);
            impl Drop for Clear {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::SeqCst);
                }
            }
            let _clear = Clear(flag);
            let probed = match tokio::task::spawn_blocking(move || desktop.focused_app()).await {
                Ok(Ok(value)) => value,
                Ok(Err(_)) => None,
                Err(_) => return,
            };
            let mut slot = lock(&cache);
            slot.fetched_at = Some(Instant::now());
            slot.value = probed;
        });
    }

    fn record_mouse(&self, id: Uuid, op: &MouseOp, from: Option<(i32, i32)>, generation: u64) {
        lock(&self.automation).record_mouse(id, op, from, generation, automation::unix_ms());
    }

    fn record_type(&self, id: Uuid, at: Option<(i32, i32)>, generation: u64) {
        lock(&self.automation).record_type(id, at, generation, automation::unix_ms());
    }

    fn keyboard_blocked(&self) -> bool {
        !lock(&self.automation).keyboard_interrupt()
    }

    fn control_automation(&self, action: Control, companion: bool) -> Result<(), ApiError> {
        if matches!(action, Control::Start | Control::Resume)
            && self.desktop.backend_name() == "wayland"
        {
            return Err(ApiError::not_implemented(
                crate::desktop::WAYLAND_AUTOMATION,
            ));
        }
        if matches!(action, Control::Start | Control::Resume) && self.keyboard_blocked() {
            return Err(ApiError::not_implemented(automation::KEYBOARD_INTERRUPT));
        }
        if action == Control::Pause {
            // Epoch and Paused are one publish. Pause does not take the
            // injection lock and does not wait for a desktop call.
            lock(&self.automation).pause();
            return Ok(());
        }
        if action == Control::Stop {
            // Stop does not take the injection lock. An in-flight call sees
            // the new epoch at its next chunk; the phase stays running until
            // that call ends.
            let mut gate = lock(&self.automation);
            if automation::lifts_user_pause(action, gate.phase()) && !companion {
                return Err(ApiError::forbidden(
                    "a paused automation can only be resumed by the companion",
                ));
            }
            gate.stop();
            return Ok(());
        }
        // Start and resume only publish. They do not wait on the injection
        // lock. An in-flight call sees the snapshot at its next check.
        let session = self.control_session();
        let mut gate = lock(&self.automation);
        // Decide under the same lock that applies the action. Only the
        // companion socket may leave a user pause. Loopback TCP never does.
        if automation::lifts_user_pause(action, gate.phase()) && !companion {
            return Err(ApiError::forbidden(
                "a paused automation can only be resumed by the companion",
            ));
        }
        let now = Instant::now();
        let timeout = self.companion_poll_timeout;
        gate.apply(session, action, companion, now, timeout)
            .map_err(|err| match err {
                automation::ControlReject::NeedsSession => {
                    ApiError::bad("start needs an open session")
                }
                automation::ControlReject::StalePoll => {
                    ApiError::unavailable(automation::STALE_POLL)
                }
            })
    }

    /// The gate's live session while one exists. The most recently touched
    /// session is only the fallback when the gate is off, or that session is gone.
    fn control_session(&self) -> Option<Uuid> {
        let live = {
            let gate = lock(&self.automation);
            if gate.phase() == Phase::Off {
                None
            } else {
                gate.session()
            }
        };
        let sessions = lock(&self.sessions);
        if let Some(id) = live {
            if sessions.contains_key(&id) {
                return Some(id);
            }
        }
        sessions
            .values()
            .max_by_key(|session| session.touched())
            .map(|session| session.id())
    }
}

#[derive(Serialize)]
struct CompanionBody {
    open: bool,
    /// Process nonce. Changes when the visor starts again.
    instance: u64,
    /// `x11`, `wayland`, or `fixture`. The companion uses this to decide
    /// whether XInput2 can tell injected input from a physical device.
    backend: &'static str,
    /// `upgrading` when an updater published that notice. Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    notice: Option<&'static str>,
    /// Set when the companion cannot see physical key presses.
    #[serde(skip_serializing_if = "Option::is_none")]
    interrupt: Option<&'static str>,
    automation: AutomationBody,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<CompanionSession>,
}

#[derive(Serialize)]
struct AutomationBody {
    /// `off`, `running`, or `paused`.
    phase: &'static str,
    /// Bumps on resume and start. A drive records the value from when it happened.
    resume_seq: u64,
    /// True while mouse or type is inside the desktop call, before the drive is stored.
    injecting: bool,
    /// Bumps at the start of each mouse or type call.
    inject_seq: u64,
    /// Highest sequence whose desktop call has returned, including failures.
    last_settled_seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    drive: Option<DriveBody>,
}

#[derive(Serialize)]
struct DriveBody {
    kind: &'static str,
    x: i32,
    y: i32,
    from_x: i32,
    from_y: i32,
    clickable: bool,
    distance_px: u32,
    elapsed_ms: u32,
    at_ms: u64,
    pointer_known: bool,
    resume_seq: u64,
    inject_seq: u64,
}

struct ChosenSession {
    id: Uuid,
    purpose: String,
    verb: Option<&'static str>,
    mcp_servers: Vec<String>,
    healthy: Vec<String>,
    idle_ms: u64,
}

#[derive(Serialize)]
struct CompanionSession {
    id: Uuid,
    purpose: String,
    verb: Option<&'static str>,
    /// Prefer-MCP display: `mcp:<id>` when a healthy binding matches, else the verb.
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    /// Preferred healthy MCP server id for the focused app, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    mcp: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    mcp_servers: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    focused: Option<FocusedApp>,
    prefer_mcp: bool,
    /// Milliseconds since this session was last used.
    idle_ms: u64,
}

pub fn router(state: Arc<AppState>) -> Router {
    routes(state, false)
}

pub(crate) fn companion_router(state: Arc<AppState>) -> Router {
    routes(state, true)
}

/// Opt-in TCP router. The bearer check runs before routing, so a missing or
/// wrong token is 401 on every path, including paths with no handler. The
/// inner routes are not the companion link: the token cannot lift a pause.
fn tcp_router(state: Arc<AppState>, token: String) -> Router {
    let api = router(state).fallback(|| async { StatusCode::NOT_FOUND });
    let guarded = tower::ServiceBuilder::new()
        .layer(middleware::from_fn(move |request, next| {
            let token = token.clone();
            async move { reject_without_bearer(token, request, next).await }
        }))
        .service(api);
    Router::new().fallback_service(guarded)
}

async fn reject_without_bearer(token: String, request: Request<Body>, next: Next) -> Response {
    let header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    if !crate::companion_sock::bearer_matches(&token, header) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "unauthorized"})),
        )
            .into_response();
    }
    next.run(request).await
}

#[derive(Clone, Copy)]
struct CompanionLink(bool);

fn routes(state: Arc<AppState>, from_companion: bool) -> Router {
    Router::new()
        .route("/companion", get(companion))
        .route("/companion/automation", post(companion_automation))
        .route("/companion/interrupt", post(companion_interrupt))
        .route("/companion/notice", post(companion_notice))
        .route("/session", post(open_session))
        .route("/session/:id", delete(close_session))
        .route("/session/:id/view", post(view))
        .route("/session/:id/watch", get(watch))
        .route("/session/:id/feed/snapshot", get(session_feed_snapshot))
        .route("/session/:id/feed", get(session_feed))
        .route("/session/:id/listen", get(listen))
        .route("/session/:id/mouse", post(mouse))
        .route("/session/:id/type", post(type_keys))
        .route("/prompt", post(submit_prompt))
        .route("/audit", get(list_audit))
        .route("/mcp-audit", get(list_mcp_audit))
        .route("/session/:id/stream", post(session_stream))
        .route("/session/:id/inbox", get(session_inbox))
        .route("/session/:id/mcp", get(session_mcp))
        .route("/session/:id/mcp/call", post(mcp_call))
        .layer(Extension(CompanionLink(from_companion)))
        .with_state(state)
}

/// `tcp` is the opt-in loopback listener. `None` does not bind a port.
/// The control API is always on `control.sock`. The companion socket stays
/// a separate listener and is the only one that can lift a user pause.
pub async fn serve(tcp: Option<SocketAddr>, state: Arc<AppState>) -> std::io::Result<()> {
    let runtime = crate::companion_sock::runtime_dir()?;
    crate::companion_sock::prepare_dir(&runtime)?;
    let companion_path = crate::companion_sock::socket_file(&runtime);
    let control_path = crate::companion_sock::control_socket_file(&runtime);
    // Held until this function returns, including across the socket replace
    // below. A second process fails here and does not unlink the live socket.
    let _runtime_lock = crate::companion_sock::lock_runtime(&runtime)?;
    // A second process must not unlink a live socket and then fail the TCP
    // bind, leaving the running visor unreachable.
    if crate::companion_sock::unix_socket_listening(&companion_path)
        || crate::companion_sock::unix_socket_listening(&control_path)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "visor already running",
        ));
    }
    // Bind TCP before replacing a stale socket file. A failed port bind
    // leaves the previous sockets in place.
    let tcp_listener = match tcp {
        Some(addr) => Some(tokio::net::TcpListener::bind(addr).await?),
        None => None,
    };
    let companion_listener = crate::companion_sock::bind(&runtime).await?;
    let control_listener = crate::companion_sock::bind_control(&runtime).await?;
    let token = if tcp_listener.is_some() {
        Some(crate::companion_sock::write_api_token(&runtime)?)
    } else {
        crate::companion_sock::remove_api_token(&runtime)?;
        None
    };
    println!("control socket {}", control_path.display());
    println!("companion socket {}", companion_path.display());
    if let Some(listener) = tcp_listener.as_ref() {
        println!("listening {}", listener.local_addr()?);
    }
    let _ = std::io::Write::flush(&mut std::io::stdout());
    spawn_companion_watch(Arc::clone(&state));
    // Older builds wrote this name under the config dir. It is not a credential.
    let _ = std::fs::remove_file(state.mcp_dir.join("companion.token"));
    // One stop flag for every listener. The first one to finish cancels the
    // rest, then close_all runs, then the others have the shutdown deadline.
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let companion_app = companion_router(Arc::clone(&state));
    let companion_stop = cancel_rx.clone();
    let companion = tokio::spawn(async move {
        crate::companion_sock::accept_loop(
            companion_listener,
            companion_app,
            "companion",
            companion_stop,
        )
        .await
    });
    let control_app = router(Arc::clone(&state));
    let control_stop = cancel_rx.clone();
    let control = tokio::spawn(async move {
        crate::companion_sock::accept_loop(control_listener, control_app, "control", control_stop)
            .await
    });
    let mut jobs = vec![companion, control];
    if let (Some(listener), Some(token)) = (tcp_listener, token) {
        let app = tcp_router(Arc::clone(&state), token);
        let mut tcp_stop = cancel_rx.clone();
        jobs.push(tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = tcp_stop.wait_for(|stop| *stop).await;
                })
                .await
        }));
    }
    let mut signal_stop = cancel_rx;
    jobs.push(tokio::spawn(async move {
        tokio::select! {
            _ = shutdown_signal() => Ok(()),
            _ = signal_stop.wait_for(|stop| *stop) => Ok(()),
        }
    }));
    when_first_stops(&state, jobs, cancel_tx).await
}

/// The first listener to finish or fail cancels the others. `close_all` then
/// cancels injection, releases held input, and clears sessions. The remaining
/// listeners have [`automation::SHUTDOWN_DEADLINE`] to return.
async fn when_first_stops(
    state: &AppState,
    jobs: Vec<tokio::task::JoinHandle<std::io::Result<()>>>,
    cancel: tokio::sync::watch::Sender<bool>,
) -> std::io::Result<()> {
    let (first, rest) = wait_first(jobs).await;
    let _ = cancel.send(true);
    state.close_all();
    if rest.is_empty() {
        return first;
    }
    let outcome = wait_bounded(automation::SHUTDOWN_DEADLINE, async move {
        for job in rest {
            let _ = job.await;
        }
    })
    .await;
    match outcome {
        ShutdownWait::Finished(()) => first,
        ShutdownWait::Deadline => {
            // A desktop call stuck in one request never reaches the injection
            // lock. The X server drops this client's grabs when the process
            // exits. ReleaseGuard has already had the injection-wait budget.
            eprintln!(
                "hypermesh-visor: shutdown exceeded {}s; exiting",
                automation::SHUTDOWN_DEADLINE.as_secs()
            );
            std::process::exit(0);
        }
    }
}

fn wait_first(
    jobs: Vec<tokio::task::JoinHandle<std::io::Result<()>>>,
) -> impl std::future::Future<
    Output = (
        std::io::Result<()>,
        Vec<tokio::task::JoinHandle<std::io::Result<()>>>,
    ),
> {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    let mut slots: Vec<Option<tokio::task::JoinHandle<std::io::Result<()>>>> =
        jobs.into_iter().map(Some).collect();
    std::future::poll_fn(move |cx: &mut Context<'_>| {
        let mut ready = None;
        for (index, slot) in slots.iter_mut().enumerate() {
            let Some(job) = slot.as_mut() else {
                continue;
            };
            if let Poll::Ready(result) = Pin::new(job).poll(cx) {
                ready = Some((index, result));
                break;
            }
        }
        let Some((index, result)) = ready else {
            return if slots.iter().all(|slot| slot.is_none()) {
                Poll::Ready((Ok(()), Vec::new()))
            } else {
                Poll::Pending
            };
        };
        slots[index] = None;
        let rest = slots.iter_mut().filter_map(|slot| slot.take()).collect();
        let mapped = match result {
            Ok(result) => result,
            Err(err) => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                err.to_string(),
            )),
        };
        Poll::Ready((mapped, rest))
    })
}

enum ShutdownWait<T> {
    Finished(T),
    Deadline,
}

async fn wait_bounded<F, T>(deadline: Duration, future: F) -> ShutdownWait<T>
where
    F: std::future::Future<Output = T>,
{
    match tokio::time::timeout(deadline, future).await {
        Ok(value) => ShutdownWait::Finished(value),
        Err(_) => ShutdownWait::Deadline,
    }
}

async fn shutdown_signal() {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("terminate signal");
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .expect("interrupt signal");
    tokio::select! {
        _ = terminate.recv() => {}
        _ = interrupt.recv() => {}
    }
}

#[derive(Deserialize)]
struct OpenRequest {
    purpose: String,
    recipes: Vec<Recipe>,
    agent: Agent,
    skills: Vec<Skill>,
    #[serde(default)]
    secrets: Vec<SecretRequest>,
    /// Optional MCP profile name. When omitted, the active profile is used
    /// if mcp.json exists; missing config attaches nothing.
    #[serde(default)]
    mcp_profile: Option<String>,
}

#[derive(Serialize)]
struct OpenedSession {
    id: Uuid,
    purpose: String,
    recipes: Vec<Recipe>,
    agent: Agent,
    skills: Vec<Skill>,
    mcp_profile: String,
    mcp_servers: Vec<String>,
}

#[derive(Serialize)]
struct SessionMcpBody {
    profile: String,
    servers: Vec<SessionMcpServer>,
}

#[derive(Serialize)]
struct SessionMcpServer {
    name: String,
    transport: String,
    healthy: bool,
    tools: Vec<ToolInfo>,
}

#[derive(Deserialize)]
struct McpCallRequest {
    server: String,
    tool: String,
    /// `approve` or `deny`.
    decision: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

#[derive(Serialize)]
struct McpCallResponse {
    server: String,
    tool: String,
    decision: String,
    outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
}

async fn open_session(
    State(state): State<Arc<AppState>>,
    Json(req): Json<OpenRequest>,
) -> Result<(StatusCode, Json<OpenedSession>), ApiError> {
    let harness = Harness {
        purpose: req.purpose,
        recipes: req.recipes,
        agent: req.agent,
        skills: req.skills,
    };
    harness.validate().map_err(ApiError::from_harness)?;
    if req
        .secrets
        .iter()
        .any(|secret| secret.name.chars().count() > SESSION_TEXT_MAX)
    {
        return Err(ApiError::bad("secret name is too long"));
    }
    if req
        .mcp_profile
        .as_deref()
        .is_some_and(|profile| profile.chars().count() > SESSION_TEXT_MAX)
    {
        return Err(ApiError::bad("mcp profile is too long"));
    }
    let mcp_dir = state.mcp_dir.clone();
    let vault_dir = mcp_dir.clone();
    let secrets = req.secrets;
    // Vault source mcp (role B) is a short-lived fetch; session tool attach (role A) follows.
    let vault = tokio::task::spawn_blocking(move || Vault::open(&secrets, vault_dir))
        .await
        .map_err(|_| ApiError::internal("vault open task failed"))?
        .map_err(ApiError::from_vault)?;
    let profile = req.mcp_profile.clone();
    let mcp =
        tokio::task::spawn_blocking(move || mcp::attach_profile(&mcp_dir, profile.as_deref()))
            .await
            .map_err(|_| ApiError::internal("mcp attach task failed"))?
            .map_err(ApiError::from_mcp)?;
    let id = Uuid::new_v4();
    let opened = OpenedSession {
        id,
        purpose: harness.purpose.clone(),
        recipes: harness.recipes.clone(),
        agent: harness.agent.clone(),
        skills: harness.skills.clone(),
        mcp_profile: mcp.profile.clone(),
        mcp_servers: mcp.server_names(),
    };
    lock(&state.sessions).insert(id, Session::create(id, harness, vault, mcp));
    Ok((StatusCode::CREATED, Json(opened)))
}

async fn session_mcp(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<SessionMcpBody>, ApiError> {
    let id = parse_id(&id)?;
    let sessions = lock(&state.sessions);
    let session = sessions.get(&id).ok_or_else(ApiError::missing)?;
    let servers = session
        .mcp()
        .servers
        .iter()
        .map(|server| SessionMcpServer {
            name: server.name.clone(),
            transport: server.transport.clone(),
            healthy: server.is_healthy(),
            tools: server.tools.clone(),
        })
        .collect();
    Ok(Json(SessionMcpBody {
        profile: session.mcp().profile.clone(),
        servers,
    }))
}

async fn mcp_call(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<McpCallRequest>,
) -> Result<Json<McpCallResponse>, ApiError> {
    let id = parse_id(&id)?;
    let decision = body.decision.trim().to_ascii_lowercase();
    if decision != "approve" && decision != "deny" {
        return Err(ApiError::bad("decision must be approve or deny"));
    }
    let server = body.server.trim().to_string();
    let tool = body.tool.trim().to_string();
    if server.is_empty() || tool.is_empty() {
        return Err(ApiError::bad("server and tool are required"));
    }

    if decision == "deny" {
        lock(&state.mcp_audit).record(McpAuditRow {
            server: server.clone(),
            tool: tool.clone(),
            decision: "deny".into(),
            outcome: "denied".into(),
        });
        state.note(id, "mcp", &format!("{server} {tool} deny"));
        return Ok(Json(McpCallResponse {
            server,
            tool,
            decision: "deny".into(),
            outcome: "denied".into(),
            result: None,
        }));
    }

    // Approve path: call the tool. Arguments are not written to the audit row.
    let arguments = body.arguments;
    let call_result = {
        let mut sessions = lock(&state.sessions);
        let session = sessions.get_mut(&id).ok_or_else(ApiError::missing)?;
        session.mcp_mut().call_tool(&server, &tool, arguments)
    };
    match call_result {
        Ok(result) => {
            lock(&state.mcp_audit).record(McpAuditRow {
                server: server.clone(),
                tool: tool.clone(),
                decision: "approve".into(),
                outcome: "ok".into(),
            });
            state.note(id, "mcp", &format!("{server} {tool} approve"));
            Ok(Json(McpCallResponse {
                server,
                tool,
                decision: "approve".into(),
                outcome: "ok".into(),
                result: Some(result),
            }))
        }
        Err(err) => {
            lock(&state.mcp_audit).record(McpAuditRow {
                server: server.clone(),
                tool: tool.clone(),
                decision: "approve".into(),
                outcome: "failed".into(),
            });
            state.note(id, "mcp", &format!("{server} {tool} approve"));
            Err(ApiError::from_mcp(err))
        }
    }
}

async fn list_mcp_audit(State(state): State<Arc<AppState>>) -> Json<Vec<McpAuditRow>> {
    Json(lock(&state.mcp_audit).rows().to_vec())
}

fn clip_text(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

/// Hot path for the companion poll. Session choice, the focus cache, and the
/// bindings cache are in memory. Focus is probed only when a session is open,
/// and `mcp-bindings.json` is stat'ed at most about once a second.
/// A poll on the companion socket marks the companion present. A TCP poll does not.
async fn companion(
    State(state): State<Arc<AppState>>,
    Extension(CompanionLink(from_companion)): Extension<CompanionLink>,
    headers: HeaderMap,
) -> Json<CompanionBody> {
    if from_companion {
        // Level-triggered. A missing keyboard pauses a live gate before this
        // poll is stored, so the heartbeat cannot keep that stretch running.
        if headers
            .get("x-hypermesh-keyboard")
            .and_then(|value| value.to_str().ok())
            == Some("0")
        {
            lock(&state.automation).set_keyboard_interrupt(false);
        }
        lock(&state.automation).note_poll(Instant::now(), state.companion_poll_timeout);
    } else {
        lock(&state.automation).watchdog(Instant::now(), state.companion_poll_timeout);
    }
    let session = state.companion_session();
    let (phase, resume_seq, injecting, inject_seq, last_settled_seq, drive, notice, interrupt) = {
        let gate = lock(&state.automation);
        let resume_seq = gate.resume_seq();
        let injecting = gate.injecting();
        let inject_seq = gate.inject_seq();
        let last_settled_seq = gate.last_settled_seq();
        let interrupt = if gate.keyboard_interrupt() {
            None
        } else {
            Some(automation::KEYBOARD_INTERRUPT)
        };
        let drive = gate.drive().map(|drive| DriveBody {
            kind: drive.kind.as_str(),
            x: drive.x,
            y: drive.y,
            from_x: drive.from_x,
            from_y: drive.from_y,
            clickable: drive.clickable,
            distance_px: drive.distance_px,
            elapsed_ms: drive.elapsed_ms,
            at_ms: drive.at_ms,
            pointer_known: drive.pointer_known,
            resume_seq: drive.resume_seq,
            inject_seq: drive.inject_seq,
        });
        (
            gate.phase().as_str(),
            resume_seq,
            injecting,
            inject_seq,
            last_settled_seq,
            drive,
            gate.notice().as_str(),
            interrupt,
        )
    };
    Json(CompanionBody {
        open: session.is_some(),
        instance: state.instance,
        backend: state.desktop.backend_name(),
        notice,
        interrupt,
        automation: AutomationBody {
            phase,
            resume_seq,
            injecting,
            inject_seq,
            last_settled_seq,
            drive,
        },
        session,
    })
}

#[derive(Deserialize)]
struct AutomationRequest {
    action: String,
}

/// `start`, `pause`, `resume`, or `stop`. `start` and `stop` are the seam for
/// a runner that is not the mouse and type verbs. `start` needs an open session.
///
/// Once the gate is paused, `resume`, `start`, and `stop` are accepted only on
/// the companion socket. The loopback route refuses them.
async fn companion_automation(
    State(state): State<Arc<AppState>>,
    Extension(CompanionLink(companion)): Extension<CompanionLink>,
    Json(body): Json<AutomationRequest>,
) -> Result<StatusCode, ApiError> {
    let action = Control::parse(&body.action)
        .ok_or_else(|| ApiError::bad("action must be start, pause, resume, or stop"))?;
    state.control_automation(action, companion)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct InterruptRequest {
    available: bool,
}

/// The companion reports that X11 cannot see key presses. Only the companion
/// socket may set this, and only to unavailable.
async fn companion_interrupt(
    State(state): State<Arc<AppState>>,
    Extension(CompanionLink(companion)): Extension<CompanionLink>,
    Json(body): Json<InterruptRequest>,
) -> Result<StatusCode, ApiError> {
    if !companion {
        return Err(ApiError::forbidden(
            "only the companion can report a missing keyboard interrupt",
        ));
    }
    if body.available {
        return Err(ApiError::bad("keyboard interrupt cannot be re-enabled"));
    }
    lock(&state.automation).set_keyboard_interrupt(false);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct NoticeRequest {
    notice: String,
}

/// `upgrading` or `clear`. Nothing in this process publishes an update by itself.
async fn companion_notice(
    State(state): State<Arc<AppState>>,
    Json(body): Json<NoticeRequest>,
) -> Result<StatusCode, ApiError> {
    let notice = Notice::parse(&body.notice)
        .ok_or_else(|| ApiError::bad("notice must be upgrading or clear"))?;
    lock(&state.automation).set_notice(notice);
    Ok(StatusCode::NO_CONTENT)
}

async fn close_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    if lock(&state.sessions).remove(&id).is_none() {
        return Err(ApiError::missing());
    }
    lock(&state.frames).remove(&id);
    lock(&state.automation).note_session_closed(id);
    let state2 = Arc::clone(&state);
    tokio::task::spawn_blocking(move || {
        let _injection = lock(&state2.injection);
        lock(&state2.automation).finish_closed_session(id);
    })
    .await
    .map_err(|_| ApiError::internal("desktop task failed"))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn view(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let id = parse_id(&id)?;
    if !state.mark_verb(id, Verb::View) {
        return Err(ApiError::missing());
    }
    let desktop = Arc::clone(&state.desktop);
    let (frame, focus) = tokio::task::spawn_blocking(move || {
        let frame = desktop.view()?;
        let focus = desktop.focused_app().ok().flatten();
        Ok::<_, DesktopError>((frame, focus))
    })
    .await
    .map_err(|_| ApiError::internal("desktop task failed"))?
    .map_err(ApiError::from_desktop)?;
    if !state.has_session(id) {
        return Err(ApiError::missing());
    }
    if let Some(label) = focus.as_ref().and_then(focus_label) {
        state.note_focus(id, &label);
    }
    state.remember_frame(id, frame.mime.clone(), frame.bytes.clone());
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, frame.mime)
        .body(Body::from(frame.bytes))
        .map_err(|_| ApiError::internal("response"))
}

async fn watch(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<WatchQuery>,
) -> Result<Response, ApiError> {
    let id = parse_id(&id)?;
    if !state.mark_verb(id, Verb::Watch) {
        return Err(ApiError::missing());
    }
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let mode = query.mode;
    tokio::spawn(watch_loop(state, id, mode, tx));
    let body = Body::from_stream(ReceiverStream::new(rx));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .map_err(|_| ApiError::internal("response"))
}

async fn watch_loop(
    state: Arc<AppState>,
    id: Uuid,
    mode: WatchMode,
    tx: tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
) {
    let mut seq = 0u64;
    let mut last: Option<u64> = None;
    loop {
        if !state.has_session(id) {
            let _ = tx
                .send(Ok(sse_json(serde_json::json!({
                    "kind": "error",
                    "error": "session closed"
                }))))
                .await;
            break;
        }
        let desktop = Arc::clone(&state.desktop);
        let viewed = tokio::task::spawn_blocking(move || {
            let frame = desktop.view()?;
            let focus = desktop.focused_app().ok().flatten();
            Ok::<_, DesktopError>((frame, focus))
        })
        .await;
        let (frame, focus) = match viewed {
            Ok(Ok(pair)) => pair,
            Ok(Err(err)) => {
                let _ = tx
                    .send(Ok(sse_json(serde_json::json!({
                        "kind": "error",
                        "error": err.to_string()
                    }))))
                    .await;
                break;
            }
            Err(_) => {
                let _ = tx
                    .send(Ok(sse_json(serde_json::json!({
                        "kind": "error",
                        "error": "desktop task failed"
                    }))))
                    .await;
                break;
            }
        };
        if !state.has_session(id) {
            let _ = tx
                .send(Ok(sse_json(serde_json::json!({
                    "kind": "error",
                    "error": "session closed"
                }))))
                .await;
            break;
        }
        if let Some(label) = focus.as_ref().and_then(focus_label) {
            state.note_focus(id, &label);
        }
        let digest = hash_frame(&frame.bytes);
        if last != Some(digest) {
            state.remember_frame(id, frame.mime.clone(), frame.bytes.clone());
            seq += 1;
            let payload = match mode {
                WatchMode::Notice => serde_json::json!({
                    "kind": "notice",
                    "changed": true,
                    "seq": seq
                }),
                WatchMode::Frames => serde_json::json!({
                    "kind": "frame",
                    "changed": true,
                    "seq": seq,
                    "mime": frame.mime,
                    "png_base64": base64::engine::general_purpose::STANDARD.encode(&frame.bytes)
                }),
            };
            if tx.send(Ok(sse_json(payload))).await.is_err() {
                break;
            }
            last = Some(digest);
        }
        tokio::time::sleep(state.watch_poll).await;
    }
}

async fn listen(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let id = parse_id(&id)?;
    if !state.mark_verb(id, Verb::Listen) {
        return Err(ApiError::missing());
    }
    let desktop = Arc::clone(&state.desktop);
    let reader = tokio::task::spawn_blocking(move || desktop.open_audio())
        .await
        .map_err(|_| ApiError::internal("desktop task failed"))?
        .map_err(ApiError::from_desktop)?;
    if !state.has_session(id) {
        return Err(ApiError::missing());
    }
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let watch_state = Arc::clone(&state);
    tokio::task::spawn_blocking(move || audio_loop(watch_state, id, reader, tx));
    let body = Body::from_stream(ReceiverStream::new(rx));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header("x-audio-format", AUDIO_FORMAT)
        .body(body)
        .map_err(|_| ApiError::internal("response"))
}

fn audio_loop(
    state: Arc<AppState>,
    id: Uuid,
    mut reader: Box<dyn AudioRead>,
    tx: tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
) {
    loop {
        if !state.has_session(id) {
            break;
        }
        match reader.read_chunk() {
            Ok(Some(chunk)) => {
                if tx.blocking_send(Ok(Bytes::from(chunk))).is_err() {
                    break;
                }
            }
            Ok(None) => break,
            Err(err) => {
                let _ = tx.blocking_send(Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    err.to_string(),
                )));
                break;
            }
        }
    }
}

fn paused_after(sent: usize, unit: &str) -> ApiError {
    if sent == 0 {
        ApiError::conflict("automation paused")
    } else {
        ApiError::conflict(format!("automation paused after {sent} {unit}"))
    }
}

/// True when a pause or stop landed after the request captured `epoch`, or
/// the published phase is already paused. The word is the lock-free snapshot.
fn injection_cancelled(captured: u64, word: u64) -> bool {
    let (epoch, phase) = automation::unpack_snap(word);
    epoch != captured || phase == Phase::Paused
}

/// A mouse or type request that waited on the injection lock must not open a
/// new stretch when unwind left the gate `off` and the companion poll has
/// since expired. A first injection, with no trusted instant yet, is allowed.
fn stretch_ended(state: &AppState) -> bool {
    let gate = lock(&state.automation);
    gate.phase() == Phase::Off && gate.stale_reentry(Instant::now(), state.companion_poll_timeout)
}

fn gate_word(state: &AppState) -> u64 {
    state.published.load(Ordering::SeqCst)
}

/// `Some` when the injection lock was free within `bound`. Shutdown keeps
/// going when the desktop call is still inside that lock.
fn lock_for<'a, T>(mutex: &'a Mutex<T>, bound: Duration) -> Option<std::sync::MutexGuard<'a, T>> {
    let start = Instant::now();
    loop {
        match mutex.try_lock() {
            Ok(guard) => return Some(guard),
            Err(std::sync::TryLockError::Poisoned(err)) => return Some(err.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                if start.elapsed() >= bound {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

fn coords_on_root(width: u16, height: u16, op: &MouseOp) -> bool {
    let inside = |x: i32, y: i32| x >= 0 && y >= 0 && x < i32::from(width) && y < i32::from(height);
    match *op {
        MouseOp::Move { x, y, .. } | MouseOp::Click { x, y, .. } => inside(x, y),
        MouseOp::Drag {
            x, y, to_x, to_y, ..
        } => inside(x, y) && inside(to_x, to_y),
    }
}

fn with_reached(op: &MouseOp, at: Option<(i32, i32)>) -> MouseOp {
    let Some((x, y)) = at else {
        return op.clone();
    };
    match *op {
        MouseOp::Move { clickable, .. } => MouseOp::Move { x, y, clickable },
        MouseOp::Click { button, .. } => MouseOp::Click { x, y, button },
        MouseOp::Drag {
            x: from_x,
            y: from_y,
            button,
            ..
        } => MouseOp::Drag {
            x: from_x,
            y: from_y,
            to_x: x,
            to_y: y,
            button,
        },
    }
}

/// Buttons and keys this request pressed and has not released.
enum Held {
    Button(Button),
    Key(u32),
}

/// Best-effort release of whatever this request pressed. Drop covers error,
/// cancel, panic, and a success that still has an unmatched down. A key or
/// button is not left down for a later request.
struct ReleaseGuard<'a> {
    desktop: &'a dyn Desktop,
    held: Vec<Held>,
    armed: bool,
}

impl<'a> ReleaseGuard<'a> {
    fn arm(desktop: &'a dyn Desktop) -> Self {
        Self {
            desktop,
            held: Vec::new(),
            armed: true,
        }
    }

    fn note_step(&mut self, step: PointerStep) {
        let PointerStep::Button { button, down } = step else {
            return;
        };
        if down {
            self.held.push(Held::Button(button));
            return;
        }
        if let Some(index) = self
            .held
            .iter()
            .rposition(|item| matches!(item, Held::Button(pressed) if *pressed == button))
        {
            self.held.remove(index);
        }
    }

    fn note_strokes(&mut self, strokes: &[Stroke]) {
        for stroke in strokes {
            if stroke.down {
                self.held.push(Held::Key(stroke.keysym));
                continue;
            }
            if let Some(index) = self
                .held
                .iter()
                .rposition(|item| matches!(item, Held::Key(key) if *key == stroke.keysym))
            {
                self.held.remove(index);
            }
        }
    }

    fn release(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        while let Some(item) = self.held.pop() {
            match item {
                Held::Button(button) => {
                    let _ = self.desktop.pointer_steps(&[PointerStep::Button {
                        button,
                        down: false,
                    }]);
                }
                Held::Key(keysym) => {
                    let _ = self.desktop.type_input(&[Stroke {
                        keysym,
                        down: false,
                    }]);
                }
            }
        }
    }
}

impl Drop for ReleaseGuard<'_> {
    fn drop(&mut self) {
        self.release();
    }
}

/// A short drag is one desktop call. A longer one is pointer steps, and the
/// latch is checked between them. [`ReleaseGuard`] releases a button this
/// request pressed when a later step fails or the latch trips.
fn inject_mouse(
    desktop: &dyn Desktop,
    op: &MouseOp,
    cancelled: &dyn Fn() -> bool,
    release: &mut ReleaseGuard<'_>,
) -> Result<(), ApiError> {
    let MouseOp::Drag {
        x,
        y,
        to_x,
        to_y,
        button,
    } = *op
    else {
        if cancelled() {
            return Err(paused_after(0, "pointer steps"));
        }
        return inject_short_mouse(desktop, op, release);
    };
    let points =
        drag_point_count(x, y, to_x, to_y).map_err(|_| ApiError::bad("drag is too long"))?;
    if points <= 1 {
        if cancelled() {
            return Err(paused_after(0, "pointer steps"));
        }
        return inject_short_mouse(desktop, op, release);
    }
    let mut steps =
        drag_steps(x, y, to_x, to_y, button).map_err(|_| ApiError::bad("drag is too long"))?;
    let mut pipe = desktop.open_pointer().map_err(ApiError::from_desktop)?;
    let mut done = 0usize;
    loop {
        if cancelled() {
            return Err(paused_after(done, "pointer steps"));
        }
        let batch: Vec<PointerStep> = steps.by_ref().take(POINTER_BATCH).collect();
        if batch.is_empty() {
            break;
        }
        for step in &batch {
            if matches!(step, PointerStep::Button { down: true, .. }) {
                release.note_step(*step);
            }
        }
        pipe.send(&batch).map_err(ApiError::from_desktop)?;
        for step in batch {
            if matches!(step, PointerStep::Button { down: false, .. }) {
                release.note_step(step);
            }
            done += 1;
        }
    }
    Ok(())
}

/// Click and a one-step drag press a button inside `mouse`. Note the down
/// before that call so a failure still releases it.
fn inject_short_mouse(
    desktop: &dyn Desktop,
    op: &MouseOp,
    release: &mut ReleaseGuard<'_>,
) -> Result<(), ApiError> {
    let button = match *op {
        MouseOp::Click { button, .. } | MouseOp::Drag { button, .. } => Some(button),
        MouseOp::Move { .. } => None,
    };
    if let Some(button) = button {
        release.note_step(PointerStep::Button { button, down: true });
    }
    desktop.mouse(op).map_err(ApiError::from_desktop)?;
    if let Some(button) = button {
        release.note_step(PointerStep::Button {
            button,
            down: false,
        });
    }
    Ok(())
}

fn spawn_companion_watch(state: Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            let slice = (state.companion_poll_timeout / 4)
                .clamp(Duration::from_millis(10), Duration::from_millis(250));
            tokio::time::sleep(slice).await;
            lock(&state.automation).watchdog(Instant::now(), state.companion_poll_timeout);
        }
    });
}

struct InjectGuard<'a> {
    gate: &'a Mutex<automation::Gate>,
}

impl<'a> InjectGuard<'a> {
    fn arm(gate: &'a Mutex<automation::Gate>, session: Uuid, epoch: u64) -> Result<Self, ApiError> {
        lock(gate)
            .begin_inject(session, epoch)
            .map_err(|_| ApiError::conflict("automation paused"))?;
        Ok(Self { gate })
    }
}

impl Drop for InjectGuard<'_> {
    fn drop(&mut self) {
        lock(self.gate).end_inject();
    }
}

async fn mouse(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(op): Json<MouseOp>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let backend = state.desktop.backend_name();
    if !mouse_in_bounds(backend, &op) {
        return Err(ApiError::bad("pointer coordinate is out of range"));
    }
    if let MouseOp::Drag {
        x, y, to_x, to_y, ..
    } = op
    {
        drag_point_count(x, y, to_x, to_y).map_err(|_| ApiError::bad("drag is too long"))?;
    }
    lock(&state.automation).watchdog(Instant::now(), state.companion_poll_timeout);
    // Captured before the phase check. A pause that publishes a new epoch
    // after this read cannot be adopted as the baseline inside the desktop task.
    let epoch = automation::unpack_snap(gate_word(&state)).0;
    if backend == "wayland" {
        return Err(ApiError::not_implemented(
            crate::desktop::WAYLAND_AUTOMATION,
        ));
    }
    if state.keyboard_blocked() {
        return Err(ApiError::not_implemented(automation::KEYBOARD_INTERRUPT));
    }
    if injection_cancelled(epoch, gate_word(&state)) {
        return Err(ApiError::conflict("automation paused"));
    }
    if lock(&state.automation).stale_reentry(Instant::now(), state.companion_poll_timeout) {
        return Err(ApiError::unavailable(automation::STALE_POLL));
    }
    if !state.mark_verb(id, Verb::Mouse) {
        return Err(ApiError::missing());
    }
    let activity = mouse_activity(&op);
    let desktop = Arc::clone(&state.desktop);
    let state2 = Arc::clone(&state);
    let op_for_drive = op.clone();
    tokio::task::spawn_blocking(move || {
        let _injection = lock(&state2.injection);
        if !state2.has_session(id) {
            return Err(ApiError::missing());
        }
        if let Some((width, height)) = desktop.root_size().map_err(ApiError::from_desktop)? {
            if !coords_on_root(width, height, &op) {
                return Err(ApiError::bad("pointer coordinate is out of range"));
            }
        }
        let cancelled = || injection_cancelled(epoch, gate_word(&state2));
        if cancelled() || stretch_ended(&state2) {
            return Err(ApiError::conflict("automation paused"));
        }
        let _injecting = InjectGuard::arm(&state2.automation, id, epoch)?;
        let mut release = ReleaseGuard::arm(&*desktop);
        let generation = lock(&state2.automation).generation();
        let from = desktop.pointer_position().ok().flatten();
        inject_mouse(&*desktop, &op, &cancelled, &mut release)?;
        if cancelled() {
            return Err(ApiError::conflict("automation paused"));
        }
        let reached = desktop.pointer_position().ok().flatten();
        let recorded = with_reached(&op_for_drive, reached);
        state2.record_mouse(id, &recorded, from, generation);
        Ok(())
    })
    .await
    .map_err(|_| ApiError::internal("desktop task failed"))??;
    state.note(id, "mouse", &activity);
    Ok(StatusCode::NO_CONTENT)
}

async fn type_keys(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(mut body): Json<TypeBody>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    lock(&state.automation).watchdog(Instant::now(), state.companion_poll_timeout);
    let epoch = automation::unpack_snap(gate_word(&state)).0;
    if state.desktop.backend_name() == "wayland" {
        return Err(ApiError::not_implemented(
            crate::desktop::WAYLAND_AUTOMATION,
        ));
    }
    if state.keyboard_blocked() {
        return Err(ApiError::not_implemented(automation::KEYBOARD_INTERRUPT));
    }
    if injection_cancelled(epoch, gate_word(&state)) {
        return Err(ApiError::conflict("automation paused"));
    }
    if lock(&state.automation).stale_reentry(Instant::now(), state.companion_poll_timeout) {
        return Err(ApiError::unavailable(automation::STALE_POLL));
    }
    let (secret, typed, secret_handle, key_line) = {
        let mut sessions = lock(&state.sessions);
        let session = sessions.get_mut(&id).ok_or_else(ApiError::missing)?;
        session.mark_verb(Verb::Type);
        let typed = body
            .text
            .as_deref()
            .map(|text| session.redact(text))
            .filter(|text| !text.is_empty());
        let secret_handle = body.secret.clone().filter(|name| !name.is_empty());
        let key_line = keys_activity(&body);
        let secret = match &body.secret {
            Some(name) => Some(
                session
                    .vault()
                    .get(name)
                    .ok_or_else(|| ApiError::bad(format!("unknown secret \"{name}\"")))?,
            ),
            None => None,
        };
        (secret, typed, secret_handle, key_line)
    };
    let strokes = plan_type(&body, secret.as_ref().map(|value| value.as_slice()))
        .map_err(ApiError::from_plan)?;
    let planned = keystroke_count(&strokes);
    if planned > TYPE_MAX_KEYSTROKES {
        return Err(ApiError::bad(format!(
            "type request is too large ({planned} keystrokes, max {TYPE_MAX_KEYSTROKES})"
        )));
    }
    if let Some(text) = &mut body.text {
        text.zeroize();
    }
    drop(secret);
    if !state.has_session(id) {
        return Err(ApiError::missing());
    }
    let desktop = Arc::clone(&state.desktop);
    let state2 = Arc::clone(&state);
    tokio::task::spawn_blocking(move || {
        let _injection = lock(&state2.injection);
        if !state2.has_session(id) {
            return Err(ApiError::missing());
        }
        let cancelled = || injection_cancelled(epoch, gate_word(&state2));
        if cancelled() || stretch_ended(&state2) {
            return Err(ApiError::conflict("automation paused"));
        }
        let _injecting = InjectGuard::arm(&state2.automation, id, epoch)?;
        let mut release = ReleaseGuard::arm(&*desktop);
        let generation = lock(&state2.automation).generation();
        let mut rest = strokes.as_slice();
        let mut sent = 0usize;
        let mut keyboard = desktop.open_keyboard().map_err(ApiError::from_desktop)?;
        while !rest.is_empty() {
            if cancelled() {
                return Err(paused_after(sent, "keystrokes"));
            }
            let chunk = take_keystrokes(rest, TYPE_CHUNK_KEYSTROKES);
            for keysym in keyboard.presses(chunk) {
                release.note_strokes(&[Stroke { keysym, down: true }]);
            }
            KeyboardPipe::send(&mut *keyboard, chunk).map_err(ApiError::from_desktop)?;
            for stroke in chunk {
                if !stroke.down {
                    release.note_strokes(std::slice::from_ref(stroke));
                }
            }
            sent += keystroke_count(chunk);
            rest = &rest[chunk.len()..];
        }
        if cancelled() {
            return Err(paused_after(sent, "keystrokes"));
        }
        let at = desktop.pointer_position().ok().flatten();
        state2.record_type(id, at, generation);
        Ok(())
    })
    .await
    .map_err(|_| ApiError::internal("desktop task failed"))??;
    if let Some(text) = &typed {
        state.note(id, "type", text);
    }
    if let Some(text) = &key_line {
        state.note(id, "type", text);
    }
    if let Some(handle) = &secret_handle {
        state.note(id, "secret", handle);
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct PromptRequest {
    prompt: String,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Serialize)]
struct PromptResponse {
    response: String,
    passed_through: bool,
}

async fn submit_prompt(
    State(state): State<Arc<AppState>>,
    Json(mut body): Json<PromptRequest>,
) -> Result<Json<PromptResponse>, ApiError> {
    let prompt = std::mem::take(&mut body.prompt);
    let mut api_key = std::mem::take(&mut body.api_key);
    let model = body.model.clone();
    let state = Arc::clone(&state);
    let result = tokio::task::spawn_blocking(move || {
        let result = state.prompts.submit(&prompt, &api_key, model.as_deref());
        api_key.zeroize();
        result
    })
    .await
    .map_err(|_| ApiError::internal("prompt task failed"))?;
    let response = result.map_err(ApiError::from_prompt)?;
    Ok(Json(PromptResponse {
        response,
        passed_through: true,
    }))
}

async fn list_audit(State(state): State<Arc<AppState>>) -> Json<Vec<AuditRecord>> {
    Json(state.prompts.audit())
}

/// Ongoing input for a session that is already open.
/// The API key is the `X-Api-Key` header. It is checked before the body is read.
async fn session_stream(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: axum::http::HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let mut api_key = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    if let Err(err) = stream::gate(&api_key) {
        api_key.zeroize();
        return Err(ApiError::from_stream(err));
    }
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(err) => {
            api_key.zeroize();
            return Err(err);
        }
    };
    if !state.has_session(id) {
        api_key.zeroize();
        return Err(ApiError::missing());
    }
    let bytes = match axum::body::to_bytes(body, stream::MAX_STREAM_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            api_key.zeroize();
            return Err(ApiError::bad("stream body is too large"));
        }
    };
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text.to_string(),
        Err(_) => {
            api_key.zeroize();
            return Err(ApiError::bad("stream body is not utf-8"));
        }
    };
    let rendered = match state.deliver_stream(id, &text, &api_key) {
        Ok(rendered) => rendered,
        Err(err) => {
            api_key.zeroize();
            return Err(ApiError::from_stream(err));
        }
    };
    let rendered = redact_key(&rendered, &api_key);
    api_key.zeroize();
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(Body::from(rendered))
        .map_err(|_| ApiError::internal("response"))
}

async fn session_inbox(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<stream::Inbox>, ApiError> {
    let id = parse_id(&id)?;
    state
        .session_inbox(id)
        .map(Json)
        .ok_or_else(ApiError::missing)
}

fn redact_key(text: &str, key: &str) -> String {
    let key = key.trim();
    if key.is_empty() {
        return text.to_string();
    }
    text.replace(key, "***")
}

#[derive(Deserialize)]
struct WatchQuery {
    #[serde(default)]
    mode: WatchMode,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum WatchMode {
    Notice,
    Frames,
}

impl Default for WatchMode {
    fn default() -> Self {
        Self::Notice
    }
}

fn parse_id(value: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(value).map_err(|_| ApiError::missing())
}

#[derive(Serialize)]
struct FeedBody {
    session: Uuid,
    snapshot: Option<FeedSnapshot>,
    thinking: Vec<FeedLine>,
    files: Vec<FeedHandle>,
    secrets: Vec<FeedHandle>,
}

#[derive(Serialize)]
struct FeedSnapshot {
    handle: String,
    mime: String,
    seq: u64,
}

#[derive(Serialize)]
struct FeedLine {
    kind: &'static str,
    text: String,
}

#[derive(Serialize)]
struct FeedHandle {
    handle: String,
}

/// Latest desktop snapshot handle plus terminal/IDE activity.
/// Image bytes stay on `GET /session/{id}/feed/snapshot`. Secret values
/// and file bytes are not on this document.
async fn session_feed(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<FeedBody>, ApiError> {
    let id = parse_id(&id)?;
    let (thinking, files, secrets) = {
        let sessions = lock(&state.sessions);
        let session = sessions.get(&id).ok_or_else(ApiError::missing)?;
        let thinking = session
            .activity()
            .iter()
            .map(|line| FeedLine {
                kind: line.kind,
                text: session.redact(&line.text),
            })
            .collect();
        let files = session
            .held_files()
            .iter()
            .map(|file| FeedHandle {
                handle: file.name.clone(),
            })
            .collect();
        let secrets = session
            .vault()
            .names()
            .into_iter()
            .map(|handle| FeedHandle { handle })
            .collect();
        (thinking, files, secrets)
    };
    let snapshot = {
        let frames = lock(&state.frames);
        frames.get(&id).map(|frame| FeedSnapshot {
            handle: frame.handle.clone(),
            mime: frame.mime.clone(),
            seq: frame.seq,
        })
    };
    Ok(Json(FeedBody {
        session: id,
        snapshot,
        thinking,
        files,
        secrets,
    }))
}

async fn session_feed_snapshot(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let id = parse_id(&id)?;
    if !state.has_session(id) {
        return Err(ApiError::missing());
    }
    let held = {
        let frames = lock(&state.frames);
        frames
            .get(&id)
            .map(|frame| (frame.mime.clone(), frame.bytes.clone()))
    };
    let Some((mime, bytes)) = held else {
        return Err(ApiError::missing());
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(bytes))
        .map_err(|_| ApiError::internal("response"))
}

fn focus_label(app: &FocusedApp) -> Option<String> {
    let raw = app
        .wm_class
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| app.app_id.clone().filter(|value| !value.trim().is_empty()))
        .or_else(|| {
            app.executable.as_ref().and_then(|path| {
                std::path::Path::new(path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
        })?;
    let label = raw.trim();
    if label.is_empty() {
        None
    } else {
        Some(label.to_string())
    }
}

fn hash_frame(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn sse_json(value: serde_json::Value) -> Bytes {
    Bytes::from(format!("data: {value}\n\n"))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn missing() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: "session closed or unknown".into(),
        }
    }

    fn bad(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }

    fn not_implemented(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_IMPLEMENTED,
            message: message.into(),
        }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }

    fn from_harness(err: HarnessError) -> Self {
        Self::bad(err.to_string())
    }

    fn from_plan(err: PlanError) -> Self {
        Self::bad(err.to_string())
    }

    fn from_vault(err: VaultError) -> Self {
        let status = match &err {
            VaultError::NotImplemented(_) => StatusCode::NOT_IMPLEMENTED,
            VaultError::McpFetch { .. } => StatusCode::BAD_GATEWAY,
            _ => StatusCode::BAD_REQUEST,
        };
        Self {
            status,
            message: err.to_string(),
        }
    }

    fn from_mcp(err: McpError) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: err.to_string(),
        }
    }

    fn from_prompt(err: PromptError) -> Self {
        let status = match &err {
            PromptError::MissingKey | PromptError::InvalidKey { .. } => StatusCode::UNAUTHORIZED,
            PromptError::EmptyPrompt | PromptError::UnknownModel => StatusCode::BAD_REQUEST,
            PromptError::Door(_) => StatusCode::BAD_GATEWAY,
        };
        Self {
            status,
            message: err.to_string(),
        }
    }

    fn from_stream(err: StreamError) -> Self {
        match err {
            StreamError::Key(err) => Self::from_prompt(err),
            StreamError::Closed => Self::missing(),
            StreamError::Vault(err) => Self::from_vault(err),
            StreamError::Empty => Self::bad(err.to_string()),
            StreamError::Bad(message) => Self::bad(message),
        }
    }

    fn from_desktop(err: DesktopError) -> Self {
        let status = match err {
            DesktopError::RootRefused => StatusCode::FORBIDDEN,
            _ => StatusCode::BAD_GATEWAY,
        };
        Self {
            status,
            message: err.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({"error": self.message}));
        (self.status, body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::pixels::encode_rgba_png;
    use crate::desktop::Frame;
    use crate::desktop::{KeyboardPipe, PointerPipe};
    use crate::input::Stroke;
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::client::conn::http1::handshake;
    use hyper::Request;
    use hyper_util::rt::TokioIo;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct FakeDesktop {
        frame: Mutex<Vec<u8>>,
        audio: Mutex<Vec<u8>>,
        mice: Mutex<Vec<MouseOp>>,
        typed: Mutex<Vec<Stroke>>,
        pointer_log: Mutex<Vec<crate::input::PointerStep>>,
        block_type: AtomicBool,
        /// Record this many strokes of the next `type_input`, then fail and clear.
        /// `0` does not fail. The guard's later release is a new call.
        type_fail_after: AtomicUsize,
        /// The next `mouse` call fails before it is recorded.
        fail_mouse: AtomicBool,
        /// 1-based `pointer_steps` call to block inside. `0` blocks nothing.
        block_step_call: AtomicUsize,
        /// 1-based `pointer_steps` call that fails before it is recorded.
        fail_step_call: AtomicUsize,
        /// `pointer_position` waits while this is set.
        block_pointer: AtomicBool,
        pointer_waits: AtomicUsize,
        step_calls: AtomicUsize,
        pointer_sessions: AtomicUsize,
        keyboard_sessions: AtomicUsize,
        root_size: Mutex<Option<(u16, u16)>>,
        views: AtomicUsize,
        audio_opens: AtomicUsize,
        root: AtomicBool,
        focus: Mutex<Option<FocusedApp>>,
        pointer: Mutex<Option<(i32, i32)>>,
        focus_probes: AtomicUsize,
        wayland: AtomicBool,
    }

    impl FakeDesktop {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                frame: Mutex::new(encode_rgba_png(1, 1, &[9, 8, 7, 255]).unwrap()),
                audio: Mutex::new(b"pcm-bytes".to_vec()),
                mice: Mutex::new(Vec::new()),
                typed: Mutex::new(Vec::new()),
                pointer_log: Mutex::new(Vec::new()),
                block_type: AtomicBool::new(false),
                type_fail_after: AtomicUsize::new(0),
                fail_mouse: AtomicBool::new(false),
                block_step_call: AtomicUsize::new(0),
                fail_step_call: AtomicUsize::new(0),
                block_pointer: AtomicBool::new(false),
                pointer_waits: AtomicUsize::new(0),
                step_calls: AtomicUsize::new(0),
                pointer_sessions: AtomicUsize::new(0),
                keyboard_sessions: AtomicUsize::new(0),
                root_size: Mutex::new(None),
                views: AtomicUsize::new(0),
                audio_opens: AtomicUsize::new(0),
                root: AtomicBool::new(false),
                focus: Mutex::new(None),
                pointer: Mutex::new(None),
                focus_probes: AtomicUsize::new(0),
                wayland: AtomicBool::new(false),
            })
        }

        fn set_focus(self: &Arc<Self>, focus: Option<FocusedApp>) {
            *lock(&self.focus) = focus;
        }
    }

    struct FakeAudio {
        chunk: Option<Vec<u8>>,
    }

    impl AudioRead for FakeAudio {
        fn read_chunk(&mut self) -> Result<Option<Vec<u8>>, DesktopError> {
            Ok(self.chunk.take())
        }
    }

    impl Desktop for FakeDesktop {
        fn view(&self) -> Result<Frame, DesktopError> {
            self.views.fetch_add(1, Ordering::SeqCst);
            if self.root.load(Ordering::SeqCst) {
                return Err(DesktopError::RootRefused);
            }
            Ok(Frame::png(lock(&self.frame).clone()))
        }

        fn open_audio(&self) -> Result<Box<dyn AudioRead>, DesktopError> {
            self.audio_opens.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeAudio {
                chunk: Some(lock(&self.audio).clone()),
            }))
        }

        fn mouse(&self, op: &MouseOp) -> Result<(), DesktopError> {
            if self.fail_mouse.swap(false, Ordering::SeqCst) {
                return Err(DesktopError::Input("mouse failed after press".into()));
            }
            lock(&self.mice).push(op.clone());
            Ok(())
        }

        fn type_input(&self, strokes: &[Stroke]) -> Result<(), DesktopError> {
            let fail_after = self.type_fail_after.swap(0, Ordering::SeqCst);
            if fail_after == 0 {
                lock(&self.typed).extend_from_slice(strokes);
                while self.block_type.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                return Ok(());
            }
            let keep = strokes.len().min(fail_after);
            lock(&self.typed).extend_from_slice(&strokes[..keep]);
            if strokes.len() > keep {
                return Err(DesktopError::Input("type failed mid-chunk".into()));
            }
            Ok(())
        }

        fn open_pointer(&self) -> Result<Box<dyn PointerPipe + '_>, DesktopError> {
            self.pointer_sessions.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakePipe { desktop: self }))
        }

        fn open_keyboard(&self) -> Result<Box<dyn KeyboardPipe + '_>, DesktopError> {
            self.keyboard_sessions.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeKeys { desktop: self }))
        }

        fn root_size(&self) -> Result<Option<(u16, u16)>, DesktopError> {
            Ok(*lock(&self.root_size))
        }

        fn pointer_steps(&self, steps: &[crate::input::PointerStep]) -> Result<(), DesktopError> {
            let n = self.step_calls.fetch_add(1, Ordering::SeqCst) + 1;
            let fail = self.fail_step_call.load(Ordering::SeqCst);
            if fail != 0 && n == fail {
                return Err(DesktopError::Input("pointer step failed".into()));
            }
            lock(&self.pointer_log).extend_from_slice(steps);
            let hold = self.block_step_call.load(Ordering::SeqCst);
            if hold != 0 && n == hold {
                while self.block_step_call.load(Ordering::SeqCst) == hold {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            Ok(())
        }

        fn focused_app(&self) -> Result<Option<FocusedApp>, DesktopError> {
            self.focus_probes.fetch_add(1, Ordering::SeqCst);
            Ok(lock(&self.focus).clone())
        }

        fn pointer_position(&self) -> Result<Option<(i32, i32)>, DesktopError> {
            if self.block_pointer.load(Ordering::SeqCst) {
                self.pointer_waits.fetch_add(1, Ordering::SeqCst);
                while self.block_pointer.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            Ok(*lock(&self.pointer))
        }

        fn backend_name(&self) -> &'static str {
            if self.wayland.load(Ordering::SeqCst) {
                "wayland"
            } else {
                "fixture"
            }
        }
    }

    struct FakePipe<'a> {
        desktop: &'a FakeDesktop,
    }

    impl PointerPipe for FakePipe<'_> {
        fn send(&mut self, steps: &[crate::input::PointerStep]) -> Result<(), DesktopError> {
            self.desktop.pointer_steps(steps)
        }
    }

    struct FakeKeys<'a> {
        desktop: &'a FakeDesktop,
    }

    impl KeyboardPipe for FakeKeys<'_> {
        fn send(&mut self, strokes: &[Stroke]) -> Result<(), DesktopError> {
            self.desktop.type_input(strokes)
        }
    }

    async fn spawn(desktop: Arc<FakeDesktop>) -> SocketAddr {
        spawn_timeout(desktop, COMPANION_POLL_TIMEOUT).await
    }

    async fn spawn_timeout(desktop: Arc<FakeDesktop>, poll_timeout: Duration) -> SocketAddr {
        spawn_with_timeout(
            desktop,
            std::env::temp_dir().join("hypermesh-mcp-empty"),
            poll_timeout,
        )
        .await
    }

    async fn spawn_with_mcp_dir(desktop: Arc<FakeDesktop>, mcp_dir: PathBuf) -> SocketAddr {
        spawn_with_timeout(desktop, mcp_dir, COMPANION_POLL_TIMEOUT).await
    }

    async fn spawn_with_timeout(
        desktop: Arc<FakeDesktop>,
        mcp_dir: PathBuf,
        poll_timeout: Duration,
    ) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut state = AppState::new(desktop, Duration::from_millis(20));
        state.mcp_dir = mcp_dir;
        state.companion_poll_timeout = poll_timeout;
        let state = Arc::new(state);
        spawn_companion_watch(Arc::clone(&state));
        tokio::spawn(async move {
            axum::serve(listener, router(state)).await.unwrap();
        });
        addr
    }

    async fn spawn_pair(desktop: Arc<FakeDesktop>) -> (SocketAddr, std::path::PathBuf) {
        spawn_pair_timeout(desktop, COMPANION_POLL_TIMEOUT).await
    }

    async fn spawn_pair_timeout(
        desktop: Arc<FakeDesktop>,
        poll_timeout: Duration,
    ) -> (SocketAddr, std::path::PathBuf) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "hypermesh-companion-api-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        crate::companion_sock::prepare_dir(&dir).unwrap();
        let unix = crate::companion_sock::bind(&dir).await.unwrap();
        let mut state = AppState::new(desktop, Duration::from_millis(20));
        state.mcp_dir = std::env::temp_dir().join("hypermesh-mcp-empty");
        state.companion_poll_timeout = poll_timeout;
        let state = Arc::new(state);
        spawn_companion_watch(Arc::clone(&state));
        let tcp_state = Arc::clone(&state);
        let sock_state = Arc::clone(&state);
        tokio::spawn(async move {
            axum::serve(listener, router(tcp_state)).await.unwrap();
        });
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            let _stop_tx = stop_tx;
            let _ = crate::companion_sock::accept_loop(
                unix,
                companion_router(sock_state),
                "companion",
                stop_rx,
            )
            .await;
        });
        (addr, dir)
    }

    fn companion_post(dir: &std::path::Path, action: &str) -> u16 {
        use std::io::{Read, Write};
        let mut stream =
            std::os::unix::net::UnixStream::connect(crate::companion_sock::socket_file(dir))
                .unwrap();
        assert!(crate::companion_sock::peer_is_current_user(&stream));
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let body = format!(r#"{{"action":"{action}"}}"#);
        let request = format!(
            "POST /companion/automation HTTP/1.1\r\nhost: hypermesh\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).unwrap();
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf);
        let text = String::from_utf8_lossy(&buf);
        if text.contains(" 204 ") {
            204
        } else if text.contains(" 403 ") {
            403
        } else {
            panic!("companion socket reply: {text}");
        }
    }

    fn companion_interrupt(dir: &std::path::Path) -> u16 {
        use std::io::{Read, Write};
        let mut stream =
            std::os::unix::net::UnixStream::connect(crate::companion_sock::socket_file(dir))
                .unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let body = r#"{"available":false}"#;
        let request = format!(
            "POST /companion/interrupt HTTP/1.1\r\nhost: hypermesh\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).unwrap();
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf);
        let text = String::from_utf8_lossy(&buf);
        if text.contains(" 204 ") {
            204
        } else {
            panic!("companion interrupt reply: {text}");
        }
    }

    fn companion_get(dir: &std::path::Path) -> serde_json::Value {
        use std::io::{Read, Write};
        let mut stream =
            std::os::unix::net::UnixStream::connect(crate::companion_sock::socket_file(dir))
                .unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .write_all(b"GET /companion HTTP/1.1\r\nhost: hypermesh\r\nconnection: close\r\n\r\n")
            .unwrap();
        let mut buf = Vec::new();
        if let Err(err) = stream.read_to_end(&mut buf) {
            if buf.is_empty() {
                panic!("companion get read: {err}");
            }
        }
        let text = String::from_utf8_lossy(&buf);
        assert!(text.contains(" 200 "), "{text}");
        let body = text
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .unwrap_or("");
        serde_json::from_str(body).unwrap_or_else(|err| panic!("{err}: {text}"))
    }

    fn companion_get_keyboard(dir: &std::path::Path, available: bool) -> serde_json::Value {
        use std::io::{Read, Write};
        let mut stream =
            std::os::unix::net::UnixStream::connect(crate::companion_sock::socket_file(dir))
                .unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let bit = if available { "1" } else { "0" };
        let request = format!(
            "GET /companion HTTP/1.1\r\nhost: hypermesh\r\nx-hypermesh-keyboard: {bit}\r\nconnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).unwrap();
        let mut buf = Vec::new();
        if let Err(err) = stream.read_to_end(&mut buf) {
            if buf.is_empty() {
                panic!("companion get read: {err}");
            }
        }
        let text = String::from_utf8_lossy(&buf);
        assert!(text.contains(" 200 "), "{text}");
        let body = text
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .unwrap_or("");
        serde_json::from_str(body).unwrap_or_else(|err| panic!("{err}: {text}"))
    }

    fn session_body(purpose: &str) -> String {
        serde_json::json!({
            "purpose": purpose,
            "recipes": [],
            "agent": {"name": "desk", "instructions": ""},
            "skills": []
        })
        .to_string()
    }

    fn open_body(secrets: serde_json::Value) -> String {
        serde_json::json!({
            "purpose": "drive the desktop",
            "recipes": [{"name": "focus", "steps": ["look", "click"]}],
            "agent": {"name": "desk", "instructions": "share the seat"},
            "skills": [{"name": "typing", "instructions": "type into the focus"}],
            "secrets": secrets
        })
        .to_string()
    }

    async fn send(
        addr: SocketAddr,
        method: &str,
        path: &str,
        body: Option<String>,
    ) -> (u16, http::HeaderMap, Bytes) {
        let response = dispatch(addr, method, path, body, None).await;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let bytes = tokio::time::timeout(Duration::from_secs(2), response.into_body().collect())
            .await
            .expect("body timeout")
            .unwrap()
            .to_bytes();
        (status, headers, bytes)
    }

    async fn dispatch(
        addr: SocketAddr,
        method: &str,
        path: &str,
        body: Option<String>,
        extra: Option<(&str, &str)>,
    ) -> hyper::Response<hyper::body::Incoming> {
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (mut sender, conn) = handshake(TokioIo::new(stream)).await.unwrap();
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let payload = body.unwrap_or_default();
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", addr.to_string())
            .header("content-type", "application/json");
        if let Some((name, value)) = extra {
            request = request.header(name, value);
        }
        let request = request.body(Full::new(Bytes::from(payload))).unwrap();
        sender.send_request(request).await.unwrap()
    }

    async fn open_session_id(addr: SocketAddr, secrets: serde_json::Value) -> (u16, String, Uuid) {
        let (status, _, body) = send(addr, "POST", "/session", Some(open_body(secrets))).await;
        let text = String::from_utf8(body.to_vec()).unwrap();
        let id = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|value| {
                value
                    .get("id")
                    .and_then(|id| id.as_str())
                    .map(str::to_string)
            })
            .and_then(|id| Uuid::parse_str(&id).ok());
        (status, text, id.unwrap_or(Uuid::nil()))
    }

    #[tokio::test]
    async fn session_carries_harness_and_hides_secrets() {
        let addr = spawn(FakeDesktop::new()).await;
        let (status, text, id) = open_session_id(
            addr,
            serde_json::json!([{"name": "password", "source": "local", "value": "hunter2"}]),
        )
        .await;
        assert_eq!(status, 201);
        assert_ne!(id, Uuid::nil());
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["purpose"], "drive the desktop");
        assert_eq!(value["recipes"][0]["name"], "focus");
        assert_eq!(value["agent"]["name"], "desk");
        assert_eq!(value["skills"][0]["name"], "typing");
        assert!(value.get("secrets").is_none());
        assert!(!text.contains("hunter2"));
    }

    #[test]
    fn companion_poll_timeout_accepts_only_the_documented_range() {
        assert_eq!(companion_poll_timeout_from(None), Duration::from_secs(2));
        assert_eq!(
            companion_poll_timeout_from(Some("")),
            Duration::from_secs(2)
        );
        assert_eq!(
            companion_poll_timeout_from(Some("nope")),
            Duration::from_secs(2)
        );
        assert_eq!(
            companion_poll_timeout_from(Some("199")),
            Duration::from_secs(2)
        );
        assert_eq!(
            companion_poll_timeout_from(Some("60001")),
            Duration::from_secs(2)
        );
        assert_eq!(
            companion_poll_timeout_from(Some("200")),
            Duration::from_millis(200)
        );
        assert_eq!(
            companion_poll_timeout_from(Some(" 60000 ")),
            Duration::from_secs(60)
        );
    }

    async fn spawn_with_bearer(token: &str) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut state = AppState::new(FakeDesktop::new(), Duration::from_millis(20));
        state.companion_poll_timeout = COMPANION_POLL_TIMEOUT;
        let app = tcp_router(Arc::new(state), token.to_string());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        addr
    }

    async fn status_with(
        addr: SocketAddr,
        method: &str,
        path: &str,
        body: Option<String>,
        authorization: Option<&str>,
    ) -> u16 {
        let extra = authorization.map(|value| ("authorization", value));
        let response = dispatch(addr, method, path, body, extra).await;
        response.status().as_u16()
    }

    #[tokio::test]
    async fn tcp_without_the_bearer_is_401_on_every_route_and_cannot_lift_a_pause() {
        let token = "a".repeat(64);
        let addr = spawn_with_bearer(&token).await;
        let routes = [
            ("GET", "/health"),
            ("GET", "/no-such-route"),
            ("GET", "/companion"),
            ("POST", "/companion/automation"),
            ("POST", "/companion/interrupt"),
            ("POST", "/companion/notice"),
            ("POST", "/session"),
            ("DELETE", "/session/00000000-0000-0000-0000-000000000001"),
            ("POST", "/session/00000000-0000-0000-0000-000000000001/view"),
            ("GET", "/session/00000000-0000-0000-0000-000000000001/watch"),
            (
                "GET",
                "/session/00000000-0000-0000-0000-000000000001/feed/snapshot",
            ),
            ("GET", "/session/00000000-0000-0000-0000-000000000001/feed"),
            (
                "GET",
                "/session/00000000-0000-0000-0000-000000000001/listen",
            ),
            (
                "POST",
                "/session/00000000-0000-0000-0000-000000000001/mouse",
            ),
            ("POST", "/session/00000000-0000-0000-0000-000000000001/type"),
            ("POST", "/prompt"),
            ("GET", "/audit"),
            ("GET", "/mcp-audit"),
            (
                "POST",
                "/session/00000000-0000-0000-0000-000000000001/stream",
            ),
            ("GET", "/session/00000000-0000-0000-0000-000000000001/inbox"),
            ("GET", "/session/00000000-0000-0000-0000-000000000001/mcp"),
            (
                "POST",
                "/session/00000000-0000-0000-0000-000000000001/mcp/call",
            ),
        ];
        for (method, path) in routes {
            let missing = status_with(addr, method, path, Some("{}".into()), None).await;
            assert_eq!(missing, 401, "{method} {path} without a token");
            let wrong = status_with(
                addr,
                method,
                path,
                Some("{}".into()),
                Some("Bearer not-the-token"),
            )
            .await;
            assert_eq!(wrong, 401, "{method} {path} with the wrong token");
        }

        let bearer = format!("Bearer {token}");
        let (status, body) = {
            let response = dispatch(
                addr,
                "POST",
                "/session",
                Some(open_body(serde_json::json!([]))),
                Some(("authorization", bearer.as_str())),
            )
            .await;
            let status = response.status().as_u16();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (status, bytes)
        };
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 201, "{text}");
        let paused = status_with(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"pause"}"#.into()),
            Some(bearer.as_str()),
        )
        .await;
        assert_eq!(paused, 204, "pause stays available with the token");
        let resumed = status_with(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"resume"}"#.into()),
            Some(bearer.as_str()),
        )
        .await;
        assert_eq!(resumed, 403, "the bearer must not lift a user pause");
        let health = status_with(addr, "GET", "/health", None, Some(bearer.as_str())).await;
        assert_eq!(
            health, 404,
            "an unknown path with the token is not a handler"
        );
    }

    #[tokio::test]
    async fn a_long_purpose_is_refused_and_a_legal_one_stays_under_the_poll_cap() {
        let addr = spawn(FakeDesktop::new()).await;
        let too_long = "p".repeat(crate::harness::SESSION_TEXT_MAX + 1);
        let (status, _, body) = send(addr, "POST", "/session", Some(session_body(&too_long))).await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 400, "{text}");
        assert!(text.contains("too long"), "{text}");

        let purpose = "p".repeat(crate::harness::SESSION_TEXT_MAX);
        let (status, _, body) = send(addr, "POST", "/session", Some(session_body(&purpose))).await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 201, "{text}");
        let (status, _, body) = send(addr, "GET", "/companion", None).await;
        assert_eq!(status, 200);
        assert!(body.len() < 64 * 1024, "poll is {} bytes", body.len());
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["session"]["purpose"], purpose);
    }

    #[tokio::test]
    async fn session_attaches_stdio_mcp_from_config_dir() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "hm-mcp-api-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fixture-mcp.py");
        std::fs::write(
            &script,
            r#"#!/usr/bin/env python3
import json, sys
def recv():
    line = sys.stdin.readline()
    if not line:
        raise SystemExit(0)
    return json.loads(line)
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()
while True:
    msg = recv()
    method = msg.get("method")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"0"}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"tools":[{"name":"ping","description":"ping","inputSchema":{"type":"object"}}]}})
    elif method == "tools/call":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"content":[{"type":"text","text":"pong"}]}})
"#,
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        let servers = serde_json::json!({
            "mcpServers": {
                "fixture": {
                    "command": script.to_string_lossy(),
                    "type": "stdio"
                }
            }
        });
        std::fs::write(dir.join("mcp.json"), servers.to_string()).unwrap();
        std::fs::write(
            dir.join("mcp-profiles.json"),
            r#"{"active":"default","profiles":{"default":{"servers":["fixture"]}}}"#,
        )
        .unwrap();
        // Prefer-MCP needs a binding + focused app; without focus, mode falls back to verb.
        std::fs::write(
            dir.join("mcp-bindings.json"),
            r#"{"bindings":[{"server":"fixture","wm_class":"FixtureApp"}]}"#,
        )
        .unwrap();

        let desktop = FakeDesktop::new();
        let addr = spawn_with_mcp_dir(Arc::clone(&desktop), dir.clone()).await;
        let (status, text, id) = open_session_id(addr, serde_json::json!([])).await;
        assert_eq!(status, 201, "{text}");
        let opened: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(opened["mcp_profile"], "default");
        assert_eq!(opened["mcp_servers"][0], "fixture");

        let (status, _, body) = send(addr, "GET", &format!("/session/{id}/mcp"), None).await;
        assert_eq!(status, 200);
        let mcp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(mcp["profile"], "default");
        assert_eq!(mcp["servers"][0]["name"], "fixture");
        assert_eq!(mcp["servers"][0]["healthy"], true);
        assert_eq!(mcp["servers"][0]["tools"][0]["name"], "ping");

        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let companion: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(companion["session"]["prefer_mcp"], false);
        assert!(companion["session"].get("mcp").is_none() || companion["session"]["mcp"].is_null());

        desktop.set_focus(Some(FocusedApp {
            wm_class: Some("FixtureApp".into()),
            app_id: None,
            executable: None,
        }));
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":1,"y":2}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let mut preferred = false;
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(2_000) {
            let (_, _, body) = send(addr, "GET", "/companion", None).await;
            let companion: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if companion["session"]["prefer_mcp"] == true {
                assert_eq!(companion["session"]["mcp"], "fixture");
                assert_eq!(companion["session"]["mode"], "mcp:fixture");
                assert_eq!(companion["session"]["verb"], "mouse");
                preferred = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(preferred, "focused app should show up within the cache ttl");
        assert!(desktop.focus_probes.load(Ordering::SeqCst) <= 2);

        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/mcp/call"),
            Some(
                serde_json::json!({
                    "server": "fixture",
                    "tool": "ping",
                    "decision": "deny",
                    "arguments": {"secret": "should-not-be-audited"}
                })
                .to_string(),
            ),
        )
        .await;
        assert_eq!(status, 200);
        let denied: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(denied["outcome"], "denied");

        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/mcp/call"),
            Some(
                serde_json::json!({
                    "server": "fixture",
                    "tool": "ping",
                    "decision": "approve",
                    "arguments": {}
                })
                .to_string(),
            ),
        )
        .await;
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
        let approved: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(approved["outcome"], "ok");
        assert_eq!(approved["result"]["content"][0]["text"], "pong");

        let (status, _, body) = send(addr, "GET", "/mcp-audit", None).await;
        assert_eq!(status, 200);
        let audit: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let audit_text = String::from_utf8_lossy(&body);
        assert!(!audit_text.contains("should-not-be-audited"));
        assert!(!audit_text.contains("secret"));
        assert_eq!(audit.as_array().unwrap().len(), 2);
        assert_eq!(audit[0]["decision"], "deny");
        assert_eq!(audit[0]["tool"], "ping");
        assert_eq!(audit[0]["server"], "fixture");
        assert_eq!(audit[1]["decision"], "approve");
        assert_eq!(audit[1]["outcome"], "ok");

        let (status, _, _) = send(addr, "DELETE", &format!("/session/{id}"), None).await;
        assert_eq!(status, 204);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn companion_shows_purpose_and_verb_without_secrets() {
        let addr = spawn(FakeDesktop::new()).await;
        let (status, _, body) = send(addr, "GET", "/companion", None).await;
        assert_eq!(status, 200);
        let idle: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(idle["open"], false);
        assert!(idle.get("session").is_none());

        let (_, _, id) = open_session_id(
            addr,
            serde_json::json!([{"name": "password", "source": "local", "value": "hunter2"}]),
        )
        .await;
        let (status, _, body) = send(addr, "GET", "/companion", None).await;
        assert_eq!(status, 200);
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(!text.contains("hunter2"));
        assert!(!text.contains("api_key"));
        let open: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(open["open"], true);
        assert_eq!(open["session"]["id"], id.to_string());
        assert_eq!(open["session"]["purpose"], "drive the desktop");
        assert!(open["session"]["verb"].is_null());

        let (status, _, _) = send(addr, "POST", &format!("/session/{id}/view"), None).await;
        assert_eq!(status, 200);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let active: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(active["session"]["verb"], "view");

        let (status, _, _) = send(addr, "DELETE", &format!("/session/{id}"), None).await;
        assert_eq!(status, 204);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let closed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(closed["open"], false);
        assert_eq!(closed["automation"]["phase"], "off");
    }

    #[test]
    fn focus_cache_is_fresh_until_the_ttl() {
        let ttl = Duration::from_millis(750);
        assert!(focus_fresh(Duration::from_millis(499), ttl));
        assert!(focus_fresh(Duration::from_millis(749), ttl));
        assert!(!focus_fresh(ttl, ttl));
        assert!(!focus_fresh(Duration::from_millis(1_000), ttl));
    }

    #[tokio::test]
    async fn companion_reuses_a_focused_app_across_polls() {
        let desktop = FakeDesktop::new();
        desktop.set_focus(Some(FocusedApp {
            wm_class: Some("Terminal".into()),
            app_id: None,
            executable: None,
        }));
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let mut focused = false;
        for _ in 0..50 {
            let (_, _, body) = send(addr, "GET", "/companion", None).await;
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["session"]["id"], id.to_string());
            if value["session"]["focused"]["wm_class"] == "Terminal" {
                focused = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(focused);
        assert_eq!(desktop.focus_probes.load(Ordering::SeqCst), 1);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["session"]["focused"]["wm_class"], "Terminal");
        assert_eq!(desktop.focus_probes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn companion_skips_focus_when_no_session_is_open() {
        let desktop = FakeDesktop::new();
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let idle: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(idle["open"], false);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(desktop.focus_probes.load(Ordering::SeqCst), 0);
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let open: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(open["session"]["id"], id.to_string());
        let saw = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if desktop.focus_probes.load(Ordering::SeqCst) >= 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(saw.is_ok(), "an open session did not probe focus");
    }

    #[tokio::test]
    async fn wayland_refuses_automation_start_resume_and_injection() {
        let desktop = FakeDesktop::new();
        desktop.wayland.store(true, Ordering::SeqCst);
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let before: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(before["backend"], "wayland");
        assert_eq!(before["automation"]["phase"], "off");
        for action in ["start", "resume"] {
            let (status, _, body) = send(
                addr,
                "POST",
                "/companion/automation",
                Some(format!(r#"{{"action":"{action}"}}"#)),
            )
            .await;
            assert_eq!(status, 501, "{action}");
            let text = String::from_utf8(body.to_vec()).unwrap();
            assert!(text.contains(crate::desktop::WAYLAND_AUTOMATION), "{text}");
        }
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":4,"y":5}"#.into()),
        )
        .await;
        assert_eq!(status, 501);
        assert!(String::from_utf8(body.to_vec()).unwrap().contains("#17"));
        assert!(lock(&desktop.mice).is_empty());
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/type"),
            Some(r#"{"text":"hi"}"#.into()),
        )
        .await;
        assert_eq!(status, 501);
        assert!(lock(&desktop.typed).is_empty());
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let after: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(after["automation"]["phase"], "off");
        assert_eq!(after["automation"]["injecting"], false);
        let (status, _, _) = send(addr, "POST", &format!("/session/{id}/view"), None).await;
        assert_eq!(status, 200);
    }

    #[tokio::test]
    async fn automation_tracks_drive_and_pause_blocks_input() {
        let desktop = FakeDesktop::new();
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;

        let (status, _, _) = send(addr, "POST", &format!("/session/{id}/view"), None).await;
        assert_eq!(status, 200);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let viewed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(viewed["automation"]["phase"], "off");
        assert_eq!(viewed["session"]["verb"], "view");
        let _ = body;

        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":10,"y":0}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":300,"y":0,"clickable":true}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let running: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(running["automation"]["phase"], "running");
        assert_eq!(running["automation"]["drive"]["kind"], "move");
        assert_eq!(running["automation"]["drive"]["clickable"], true);
        assert_eq!(running["automation"]["drive"]["distance_px"], 290);
        assert_eq!(running["automation"]["resume_seq"], 0);
        assert_eq!(running["automation"]["drive"]["resume_seq"], 0);
        assert_eq!(running["automation"]["injecting"], false);
        assert_eq!(running["automation"]["inject_seq"], 2);
        assert_eq!(running["automation"]["last_settled_seq"], 2);
        assert_eq!(running["automation"]["drive"]["inject_seq"], 2);
        let instance = running["instance"].as_u64().expect("instance");
        assert_ne!(instance, 0);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let again: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(again["instance"].as_u64(), Some(instance));
        assert!(running["session"]["idle_ms"].as_u64().is_some());

        let (status, _, _) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"pause"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let before = lock(&desktop.mice).len();
        let (status, _, paused_body) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"click","x":1,"y":1}"#.into()),
        )
        .await;
        assert_eq!(status, 409, "{}", String::from_utf8_lossy(&paused_body));
        assert_eq!(lock(&desktop.mice).len(), before);
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/type"),
            Some(r#"{"text":"hi"}"#.into()),
        )
        .await;
        assert_eq!(status, 409);
        assert!(lock(&desktop.typed).is_empty());

        let (status, _, _) = send(
            addr,
            "POST",
            "/companion/notice",
            Some(r#"{"notice":"upgrading"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (status, _, body) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"resume"}"#.into()),
        )
        .await;
        assert_eq!(status, 403, "{}", String::from_utf8_lossy(&body));
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let still: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(still["automation"]["phase"], "paused");
        assert_eq!(still["notice"], "upgrading");
        assert_eq!(still["backend"], "fixture");

        let (status, _, _) = send(addr, "DELETE", &format!("/session/{id}"), None).await;
        assert_eq!(status, 204);
    }

    #[tokio::test]
    async fn a_paused_automation_resumes_only_for_the_companion() {
        let desktop = FakeDesktop::new();
        let (addr, sock) = spawn_pair(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":1,"y":1}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (status, _, _) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"pause"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        for action in ["resume", "start", "stop"] {
            let (status, _, body) = send(
                addr,
                "POST",
                "/companion/automation",
                Some(format!(r#"{{"action":"{action}"}}"#)),
            )
            .await;
            assert_eq!(status, 403, "{action} {}", String::from_utf8_lossy(&body));
            let (status, _, body) = send(
                addr,
                "POST",
                "/companion/automation",
                Some(format!(r#"{{"action":"{action}"}}"#)),
            )
            .await;
            assert_eq!(status, 403, "{action}");
            let _ = body;
        }
        let (status, _, body) = send_with_stale_header(addr).await;
        assert_eq!(status, 403, "{}", String::from_utf8_lossy(&body));
        let (status, _, _) = send(addr, "DELETE", &format!("/session/{id}"), None).await;
        assert_eq!(status, 204);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let paused: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(paused["automation"]["phase"], "paused");
        assert_eq!(paused["open"], false);
        let (_, _, fresh) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{fresh}/mouse"),
            Some(r#"{"action":"move","x":2,"y":2}"#.into()),
        )
        .await;
        assert_eq!(status, 409);
        let status = tokio::task::spawn_blocking(move || companion_post(&sock, "resume"))
            .await
            .unwrap();
        assert_eq!(status, 204);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let resumed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(resumed["automation"]["phase"], "running");
        assert_eq!(resumed["automation"]["resume_seq"], 1);
        assert!(resumed["automation"]["drive"].is_null());
    }

    #[tokio::test]
    async fn a_stop_then_pause_latches_and_tcp_start_stays_blocked() {
        let desktop = FakeDesktop::new();
        let (addr, sock) = spawn_pair(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":4,"y":5}"#.into()),
        )
        .await;
        assert_eq!(status, 204);

        let (status, _, _) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"stop"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let stopped: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(stopped["automation"]["phase"], "off");

        let (status, _, _) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"pause"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let paused: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(paused["automation"]["phase"], "paused");

        let (status, _, body) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"start"}"#.into()),
        )
        .await;
        assert_eq!(status, 403, "{}", String::from_utf8_lossy(&body));
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let still: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(still["automation"]["phase"], "paused");
        let before = lock(&desktop.mice).len();
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":9,"y":9}"#.into()),
        )
        .await;
        assert_eq!(status, 409);
        assert_eq!(lock(&desktop.mice).len(), before);

        let poll_sock = sock.clone();
        let status = tokio::task::spawn_blocking(move || companion_post(&sock, "stop"))
            .await
            .unwrap();
        assert_eq!(status, 204);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let lifted: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(lifted["automation"]["phase"], "off");
        let _ = tokio::task::spawn_blocking(move || companion_get(&poll_sock))
            .await
            .unwrap();
        let (status, _, _) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"start"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let started: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(started["automation"]["phase"], "running");
    }

    #[tokio::test]
    async fn a_pause_during_type_stops_the_next_batch() {
        let desktop = FakeDesktop::new();
        desktop.block_type.store(true, Ordering::SeqCst);
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let text = "a".repeat(64);
        let payload = format!(r#"{{"text":"{text}"}}"#);
        let type_addr = addr;
        let type_id = id;
        let job = tokio::spawn(async move {
            send(
                type_addr,
                "POST",
                &format!("/session/{type_id}/type"),
                Some(payload),
            )
            .await
        });
        let ready = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if lock(&desktop.typed).len() >= 64 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(ready.is_ok(), "the first type batch did not start");
        let paused = tokio::time::timeout(
            Duration::from_secs(2),
            send(
                addr,
                "POST",
                "/companion/automation",
                Some(r#"{"action":"pause"}"#.into()),
            ),
        )
        .await;
        desktop.block_type.store(false, Ordering::SeqCst);
        let (status, _, _) = paused.expect("pause waited on the injection lock");
        assert_eq!(status, 204);
        let (status, _, body) = job.await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 409, "{text}");
        assert!(text.contains("after 32 keystrokes"), "{text}");
        assert_eq!(lock(&desktop.typed).len(), 64);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let paused: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(paused["automation"]["phase"], "paused");
    }

    #[tokio::test]
    async fn a_type_request_over_the_cap_is_refused() {
        let desktop = FakeDesktop::new();
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let text = "a".repeat(crate::input::TYPE_MAX_KEYSTROKES + 1);
        let payload = format!(r#"{{"text":"{text}"}}"#);
        let (status, _, body) =
            send(addr, "POST", &format!("/session/{id}/type"), Some(payload)).await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 400, "{text}");
        assert!(text.contains("too large"), "{text}");
        assert!(lock(&desktop.typed).is_empty());
    }

    #[tokio::test]
    async fn a_pause_during_a_long_drag_stops_later_steps() {
        let desktop = FakeDesktop::new();
        desktop.block_step_call.store(2, Ordering::SeqCst);
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let drag_addr = addr;
        let drag_id = id;
        let job = tokio::spawn(async move {
            send(
                drag_addr,
                "POST",
                &format!("/session/{drag_id}/mouse"),
                Some(r#"{"action":"drag","x":0,"y":0,"to_x":2048,"to_y":0}"#.into()),
            )
            .await
        });
        let ready = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if desktop.step_calls.load(Ordering::SeqCst) >= 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(ready.is_ok(), "the drag did not reach the second batch");
        let paused = tokio::time::timeout(
            Duration::from_secs(2),
            send(
                addr,
                "POST",
                "/companion/automation",
                Some(r#"{"action":"pause"}"#.into()),
            ),
        )
        .await;
        desktop.block_step_call.store(0, Ordering::SeqCst);
        let (status, _, _) = paused.expect("pause waited on the injection lock");
        assert_eq!(status, 204);
        let (status, _, body) = job.await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 409, "{text}");
        assert!(text.contains("after 32 pointer steps"), "{text}");
        let steps = lock(&desktop.pointer_log).clone();
        assert!(
            matches!(
                steps.last(),
                Some(crate::input::PointerStep::Button { down: false, .. })
            ),
            "{steps:?}"
        );
        assert!(
            !steps
                .iter()
                .any(|step| matches!(step, crate::input::PointerStep::Move { x: 2048, .. })),
            "{steps:?}"
        );
    }

    #[tokio::test]
    async fn a_drag_outside_the_backend_range_is_refused_before_injection() {
        let desktop = FakeDesktop::new();
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"drag","x":0,"y":0,"to_x":40000,"to_y":0}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 400, "{text}");
        assert!(text.contains("out of range"), "{text}");
        assert!(lock(&desktop.mice).is_empty());
        assert!(lock(&desktop.pointer_log).is_empty());

        desktop.wayland.store(true, Ordering::SeqCst);
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"drag","x":0,"y":0,"to_x":40000,"to_y":0}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 501, "{text}");
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":2147483647,"y":0}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 400, "{text}");
        assert!(text.contains("out of range"), "{text}");
        assert!(lock(&desktop.mice).is_empty());
        assert!(lock(&desktop.pointer_log).is_empty());
    }

    #[tokio::test]
    async fn a_failed_pointer_step_releases_the_button() {
        let desktop = FakeDesktop::new();
        desktop.fail_step_call.store(2, Ordering::SeqCst);
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"drag","x":0,"y":0,"to_x":2048,"to_y":0}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 502, "{text}");
        let steps = lock(&desktop.pointer_log).clone();
        assert!(
            steps
                .iter()
                .any(|step| matches!(step, crate::input::PointerStep::Button { down: true, .. })),
            "{steps:?}"
        );
        assert!(
            matches!(
                steps.last(),
                Some(crate::input::PointerStep::Button { down: false, .. })
            ),
            "{steps:?}"
        );
        assert!(
            !steps
                .iter()
                .any(|step| matches!(step, crate::input::PointerStep::Move { x: 2048, .. })),
            "{steps:?}"
        );
    }

    #[tokio::test]
    async fn a_long_drag_uses_one_pointer_connection() {
        let desktop = FakeDesktop::new();
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"drag","x":0,"y":0,"to_x":2048,"to_y":0}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 204, "{text}");
        assert!(lock(&desktop.mice).is_empty());
        assert_eq!(desktop.pointer_sessions.load(Ordering::SeqCst), 1);
        assert_eq!(desktop.step_calls.load(Ordering::SeqCst), 3);
        let steps = lock(&desktop.pointer_log).clone();
        assert!(
            matches!(
                steps.last(),
                Some(crate::input::PointerStep::Button { down: false, .. })
            ),
            "{steps:?}"
        );
        assert!(
            steps
                .iter()
                .any(|step| matches!(step, crate::input::PointerStep::Move { x: 2048, .. })),
            "{steps:?}"
        );
    }

    #[tokio::test]
    async fn a_successful_key_down_is_released_when_the_request_finishes() {
        let desktop = FakeDesktop::new();
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/type"),
            Some(r#"{"keys":[{"key":"control","action":"down"}]}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 204, "{text}");
        let typed = lock(&desktop.typed).clone();
        assert_eq!(
            typed,
            vec![
                Stroke {
                    keysym: 0xffe3,
                    down: true,
                },
                Stroke {
                    keysym: 0xffe3,
                    down: false,
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_failed_click_releases_the_button() {
        let desktop = FakeDesktop::new();
        desktop.fail_mouse.store(true, Ordering::SeqCst);
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"click","x":1,"y":2,"button":"left"}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 502, "{text}");
        assert!(lock(&desktop.mice).is_empty());
        let steps = lock(&desktop.pointer_log).clone();
        assert!(
            matches!(
                steps.last(),
                Some(crate::input::PointerStep::Button { down: false, .. })
            ),
            "{steps:?}"
        );
        desktop.fail_mouse.store(true, Ordering::SeqCst);
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"drag","x":4,"y":5,"to_x":4,"to_y":6,"button":"left"}"#.into()),
        )
        .await;
        assert_eq!(status, 502);
        let steps = lock(&desktop.pointer_log).clone();
        assert_eq!(
            steps
                .iter()
                .filter(|step| matches!(
                    step,
                    crate::input::PointerStep::Button { down: false, .. }
                ))
                .count(),
            2,
            "{steps:?}"
        );
    }

    #[tokio::test]
    async fn a_type_chunk_that_fails_mid_send_still_releases_the_key() {
        let desktop = FakeDesktop::new();
        desktop.type_fail_after.store(1, Ordering::SeqCst);
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/type"),
            Some(
                r#"{"keys":[{"key":"control","action":"down"},{"key":"a","action":"tap"}]}"#.into(),
            ),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 502, "{text}");
        let typed = lock(&desktop.typed).clone();
        assert!(
            typed
                .iter()
                .any(|stroke| stroke.keysym == 0xffe3 && stroke.down),
            "{typed:?}"
        );
        assert!(
            typed
                .iter()
                .any(|stroke| stroke.keysym == 0xffe3 && !stroke.down),
            "{typed:?}"
        );
        assert!(
            typed
                .iter()
                .any(|stroke| stroke.keysym == b'a' as u32 && !stroke.down),
            "the down was recorded before the chunk failed: {typed:?}"
        );
    }

    #[tokio::test]
    async fn start_does_not_wait_on_the_injection_lock() {
        let desktop = FakeDesktop::new();
        desktop.block_type.store(true, Ordering::SeqCst);
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let type_addr = addr;
        let type_id = id;
        let job = tokio::spawn(async move {
            send(
                type_addr,
                "POST",
                &format!("/session/{type_id}/type"),
                Some(r#"{"text":"ab"}"#.into()),
            )
            .await
        });
        let ready = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let (_, _, body) = send(addr, "GET", "/companion", None).await;
                let live: serde_json::Value = serde_json::from_slice(&body).unwrap();
                if live["automation"]["injecting"] == true {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(ready.is_ok(), "type did not take the injection lock");
        let started = tokio::time::Instant::now();
        let (status, _, body) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"start"}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 204, "{text}");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "start waited on the injection lock"
        );
        desktop.block_type.store(false, Ordering::SeqCst);
        let (status, _, _) = job.await.unwrap();
        assert_eq!(status, 204);
    }

    #[tokio::test]
    async fn a_stale_deferred_start_does_not_let_a_waiting_mouse_inject() {
        let desktop = FakeDesktop::new();
        desktop.block_type.store(true, Ordering::SeqCst);
        let timeout = Duration::from_millis(400);
        let (addr, dir) = spawn_pair_timeout(Arc::clone(&desktop), timeout).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let type_addr = addr;
        let type_id = id;
        let typed = tokio::spawn(async move {
            send(
                type_addr,
                "POST",
                &format!("/session/{type_id}/type"),
                Some(r#"{"text":"ab"}"#.into()),
            )
            .await
        });
        let ready = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let (_, _, body) = send(addr, "GET", "/companion", None).await;
                let live: serde_json::Value = serde_json::from_slice(&body).unwrap();
                if live["automation"]["injecting"] == true {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(ready.is_ok(), "type did not take the injection lock");
        let poll_dir = dir.clone();
        let polled = tokio::task::spawn_blocking(move || companion_get(&poll_dir))
            .await
            .unwrap();
        assert_eq!(polled["automation"]["phase"], "running");
        let (status, _, _) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"stop"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (status, _, body) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"start"}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 204, "{text}");
        let mouse_addr = addr;
        let mouse_id = id;
        let mouse = tokio::spawn(async move {
            send(
                mouse_addr,
                "POST",
                &format!("/session/{mouse_id}/mouse"),
                Some(r#"{"action":"move","x":3,"y":4}"#.into()),
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        tokio::time::sleep(timeout + Duration::from_millis(150)).await;
        desktop.block_type.store(false, Ordering::SeqCst);
        let _ = typed.await.unwrap();
        let (status, _, body) = mouse.await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 409, "{text}");
        assert!(
            lock(&desktop.mice).is_empty(),
            "the waiting move must not inject"
        );
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let snap: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_ne!(snap["automation"]["phase"], "running");
        assert_eq!(
            snap["automation"]["resume_seq"], 0,
            "a start replayed after the poll expired must not enter running"
        );
        let _ = dir;
    }

    #[tokio::test]
    async fn repeated_tcp_starts_do_not_extend_the_watchdog() {
        let desktop = FakeDesktop::new();
        let addr = spawn_timeout(Arc::clone(&desktop), Duration::from_millis(80)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"click","x":1,"y":2,"button":"left"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let mut accepted = 0u32;
        let began = tokio::time::Instant::now();
        while began.elapsed() < Duration::from_millis(400) {
            let (status, _, _) = send(
                addr,
                "POST",
                "/companion/automation",
                Some(r#"{"action":"start"}"#.into()),
            )
            .await;
            if status == 204 {
                accepted += 1;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            accepted >= 1,
            "a running TCP start should still be accepted"
        );
        let (status, _, body) = send(addr, "GET", "/companion", None).await;
        assert_eq!(status, 200);
        let snap: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(snap["automation"]["phase"], "paused");
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":3,"y":4}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 409, "{text}");
    }

    #[tokio::test]
    async fn a_tcp_stop_start_loop_cannot_keep_automation_alive() {
        let desktop = FakeDesktop::new();
        let addr = spawn_timeout(Arc::clone(&desktop), Duration::from_millis(80)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":1,"y":2}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (status, _, _) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"stop"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        for _ in 0..3 {
            let (status, _, body) = send(
                addr,
                "POST",
                "/companion/automation",
                Some(r#"{"action":"start"}"#.into()),
            )
            .await;
            let text = String::from_utf8_lossy(&body);
            assert_eq!(status, 503, "{text}");
            assert!(text.contains("no recent companion poll"), "{text}");
            let (status, _, _) = send(
                addr,
                "POST",
                "/companion/automation",
                Some(r#"{"action":"stop"}"#.into()),
            )
            .await;
            assert_eq!(status, 204);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":3,"y":4}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 503, "{text}");
        assert!(
            lock(&desktop.mice).len() == 1,
            "the later move must not inject"
        );
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/type"),
            Some(r#"{"text":"a"}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 503, "{text}");
        assert!(lock(&desktop.typed).is_empty());
    }

    #[tokio::test]
    async fn a_quiet_companion_pauses_until_a_socket_resume() {
        let desktop = FakeDesktop::new();
        let (addr, dir) = spawn_pair_timeout(Arc::clone(&desktop), Duration::from_millis(80)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"click","x":1,"y":2,"button":"left"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        tokio::time::sleep(Duration::from_millis(400)).await;
        let (status, _, body) = send(addr, "GET", "/companion", None).await;
        assert_eq!(status, 200);
        let paused: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(paused["automation"]["phase"], "paused");
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":3,"y":4}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 409, "{text}");
        let (status, _, body) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"start"}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 403, "{text}");
        let sock = dir.clone();
        let status = tokio::task::spawn_blocking(move || companion_post(&sock, "resume"))
            .await
            .unwrap();
        assert_eq!(status, 204);
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":5,"y":6}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 204, "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_socket_poll_keeps_a_running_automation_alive() {
        let desktop = FakeDesktop::new();
        let (addr, dir) = spawn_pair_timeout(Arc::clone(&desktop), Duration::from_millis(80)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"click","x":1,"y":2,"button":"left"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let started = tokio::time::Instant::now();
        while started.elapsed() < Duration::from_millis(400) {
            let poll_dir = dir.clone();
            let snap = tokio::task::spawn_blocking(move || companion_get(&poll_dir))
                .await
                .unwrap();
            assert_eq!(snap["automation"]["phase"], "running");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":8,"y":9}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 204, "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_tcp_poll_does_not_refresh_the_companion_clock() {
        let desktop = FakeDesktop::new();
        let addr = spawn_timeout(Arc::clone(&desktop), Duration::from_millis(150)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"click","x":1,"y":2,"button":"left"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (status, _, body) = send(addr, "GET", "/companion", None).await;
        assert_eq!(status, 200);
        let snap: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(snap["automation"]["phase"], "running");
        let mut saw_paused = false;
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(40)).await;
            let (status, _, body) = send(addr, "GET", "/companion", None).await;
            assert_eq!(status, 200);
            let snap: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if snap["automation"]["phase"] == "paused" {
                saw_paused = true;
                break;
            }
        }
        assert!(saw_paused, "tcp polls must not keep the gate running");
    }

    #[tokio::test]
    async fn a_pause_between_key_chunks_releases_a_held_modifier() {
        let desktop = FakeDesktop::new();
        desktop.block_type.store(true, Ordering::SeqCst);
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let mut keys = vec![serde_json::json!({"key": "control", "action": "down"})];
        for _ in 0..40 {
            keys.push(serde_json::json!({"key": "a", "action": "tap"}));
        }
        let payload = serde_json::json!({ "keys": keys }).to_string();
        let type_addr = addr;
        let type_id = id;
        let job = tokio::spawn(async move {
            send(
                type_addr,
                "POST",
                &format!("/session/{type_id}/type"),
                Some(payload),
            )
            .await
        });
        let ready = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if lock(&desktop.typed).len() >= 63 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(ready.is_ok(), "the first key batch did not start");
        let paused = tokio::time::timeout(
            Duration::from_secs(2),
            send(
                addr,
                "POST",
                "/companion/automation",
                Some(r#"{"action":"pause"}"#.into()),
            ),
        )
        .await;
        desktop.block_type.store(false, Ordering::SeqCst);
        let (status, _, _) = paused.expect("pause waited on the injection lock");
        assert_eq!(status, 204);
        let (status, _, body) = job.await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 409, "{text}");
        assert!(text.contains("after 32 keystrokes"), "{text}");
        let typed = lock(&desktop.typed).clone();
        assert_eq!(
            typed.last().copied(),
            Some(Stroke {
                keysym: 0xffe3,
                down: false,
            }),
            "{typed:?}"
        );
        assert_eq!(
            typed
                .iter()
                .filter(|stroke| stroke.keysym == 0xffe3 && !stroke.down)
                .count(),
            1,
            "{typed:?}"
        );
    }

    #[tokio::test]
    async fn a_pause_after_entry_cancels_before_the_pointer_call() {
        let desktop = FakeDesktop::new();
        desktop.block_pointer.store(true, Ordering::SeqCst);
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let move_addr = addr;
        let move_id = id;
        let job = tokio::spawn(async move {
            send(
                move_addr,
                "POST",
                &format!("/session/{move_id}/mouse"),
                Some(r#"{"action":"move","x":3,"y":4}"#.into()),
            )
            .await
        });
        let ready = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if desktop.pointer_waits.load(Ordering::SeqCst) >= 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(ready.is_ok(), "the move did not reach the pointer query");
        let (status, _, _) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"pause"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        desktop.block_pointer.store(false, Ordering::SeqCst);
        let (status, _, body) = job.await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 409, "{text}");
        assert!(
            lock(&desktop.mice).is_empty(),
            "the move ran after the pause"
        );
    }

    #[test]
    fn release_guard_releases_held_input_on_drop_and_panic() {
        let desktop = FakeDesktop::new();
        {
            let mut guard = ReleaseGuard::arm(&*desktop);
            guard.note_strokes(&[
                Stroke {
                    keysym: 0xffe1,
                    down: true,
                },
                Stroke {
                    keysym: 0xffe3,
                    down: true,
                },
            ]);
        }
        let typed = lock(&desktop.typed).clone();
        assert_eq!(
            typed,
            vec![
                Stroke {
                    keysym: 0xffe3,
                    down: false,
                },
                Stroke {
                    keysym: 0xffe1,
                    down: false,
                },
            ]
        );

        let desktop = FakeDesktop::new();
        let hooked = Arc::clone(&desktop);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut guard = ReleaseGuard::arm(&*hooked);
            guard.note_step(crate::input::PointerStep::Button {
                button: crate::input::Button::Left,
                down: true,
            });
            panic!("inject failed");
        }));
        assert!(panicked.is_err());
        let steps = lock(&desktop.pointer_log).clone();
        assert!(
            matches!(
                steps.last(),
                Some(crate::input::PointerStep::Button { down: false, .. })
            ),
            "{steps:?}"
        );
    }

    #[test]
    fn a_changed_pause_epoch_cancels_injection() {
        let running = automation::pack_snap(4, Phase::Running);
        let paused = automation::pack_snap(4, Phase::Paused);
        let bumped = automation::pack_snap(5, Phase::Running);
        assert!(!injection_cancelled(4, running));
        assert!(injection_cancelled(4, bumped));
        assert!(injection_cancelled(4, paused));
    }

    #[test]
    fn shutdown_returns_while_an_injection_lock_is_still_held() {
        let state = AppState::new(FakeDesktop::new(), Duration::from_millis(20));
        let held = lock(&state.injection);
        let started = Instant::now();
        state.shutdown(Duration::from_millis(40));
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "shutdown waited on the injection lock"
        );
        assert_eq!(lock(&state.automation).phase(), Phase::Off);
        drop(held);
    }

    #[tokio::test]
    async fn a_tcp_exit_still_runs_close_all() {
        let state = Arc::new(AppState::new(FakeDesktop::new(), Duration::from_millis(20)));
        lock(&state.automation).pause();
        assert_eq!(lock(&state.automation).phase(), Phase::Paused);
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let tcp = tokio::spawn(async { Ok(()) });
        let saw_cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&saw_cancel);
        let mut companion_stop = cancel_rx;
        let companion = tokio::spawn(async move {
            let _ = companion_stop.wait_for(|stop| *stop).await;
            flag.store(true, Ordering::SeqCst);
            Ok(())
        });
        when_first_stops(&state, vec![tcp, companion], cancel_tx)
            .await
            .unwrap();
        assert_eq!(
            lock(&state.automation).phase(),
            Phase::Off,
            "close_all resets the gate after the tcp task ends"
        );
        assert!(
            saw_cancel.load(Ordering::SeqCst),
            "the other listener was cancelled"
        );
    }

    #[tokio::test]
    async fn shutdown_deadline_ends_a_stuck_future() {
        let started = tokio::time::Instant::now();
        let outcome = wait_bounded(Duration::from_millis(40), std::future::pending::<()>()).await;
        assert!(matches!(outcome, ShutdownWait::Deadline));
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "the deadline did not bound the wait"
        );
    }

    #[tokio::test]
    async fn a_stop_during_injection_stays_running_until_the_call_ends() {
        let desktop = FakeDesktop::new();
        desktop.block_type.store(true, Ordering::SeqCst);
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let move_addr = addr;
        let move_id = id;
        let job = tokio::spawn(async move {
            send(
                move_addr,
                "POST",
                &format!("/session/{move_id}/type"),
                Some(r#"{"text":"ab"}"#.into()),
            )
            .await
        });
        let ready = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let (_, _, body) = send(addr, "GET", "/companion", None).await;
                let live: serde_json::Value = serde_json::from_slice(&body).unwrap();
                if live["automation"]["phase"] == "running"
                    && live["automation"]["injecting"] == true
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(ready.is_ok(), "type did not publish running");
        let (status, _, _) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"stop"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let during: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(during["automation"]["phase"], "running");
        assert!(desktop.block_type.load(Ordering::SeqCst));
        desktop.block_type.store(false, Ordering::SeqCst);
        let (status, _, body) = job.await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 409, "{text}");
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let after: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(after["automation"]["phase"], "off");
    }

    #[tokio::test]
    async fn deleting_the_driving_session_during_injection_does_not_reset_early() {
        let desktop = FakeDesktop::new();
        desktop.block_type.store(true, Ordering::SeqCst);
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let type_addr = addr;
        let type_id = id;
        let job = tokio::spawn(async move {
            send(
                type_addr,
                "POST",
                &format!("/session/{type_id}/type"),
                Some(r#"{"text":"ab"}"#.into()),
            )
            .await
        });
        let ready = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let (_, _, body) = send(addr, "GET", "/companion", None).await;
                let live: serde_json::Value = serde_json::from_slice(&body).unwrap();
                if live["automation"]["injecting"] == true {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(ready.is_ok(), "type did not start");
        let close_addr = addr;
        let close_id = id;
        let close = tokio::spawn(async move {
            send(close_addr, "DELETE", &format!("/session/{close_id}"), None).await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let during: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(during["automation"]["phase"], "running");
        assert_eq!(during["automation"]["injecting"], true);
        desktop.block_type.store(false, Ordering::SeqCst);
        let _ = job.await.unwrap();
        let (status, _, _) = close.await.unwrap();
        assert_eq!(status, 204);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let after: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(after["automation"]["phase"], "off");
    }

    #[tokio::test]
    async fn a_late_socket_poll_pauses_instead_of_refreshing_the_heartbeat() {
        let desktop = FakeDesktop::new();
        let (addr, dir) = spawn_pair_timeout(Arc::clone(&desktop), Duration::from_millis(80)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":1,"y":2}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        tokio::time::sleep(Duration::from_millis(400)).await;
        let sock = dir.clone();
        let body = tokio::task::spawn_blocking(move || companion_get(&sock))
            .await
            .unwrap();
        assert_eq!(body["automation"]["phase"], "paused");
    }

    #[tokio::test]
    async fn a_missing_keyboard_interrupt_refuses_automation() {
        let desktop = FakeDesktop::new();
        let (addr, dir) = spawn_pair(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let sock = dir.clone();
        let status = tokio::task::spawn_blocking(move || companion_interrupt(&sock))
            .await
            .unwrap();
        assert_eq!(status, 204);
        let (status, _, body) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"start"}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 501, "{text}");
        assert!(text.contains("no keyboard interrupt available"), "{text}");
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":1,"y":2}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 501, "{text}");
        assert!(lock(&desktop.mice).is_empty());
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/type"),
            Some(r#"{"text":"a"}"#.into()),
        )
        .await;
        assert_eq!(status, 501);
        let sock = dir.clone();
        let body = tokio::task::spawn_blocking(move || companion_get(&sock))
            .await
            .unwrap();
        assert_eq!(body["interrupt"], "no keyboard interrupt available");
    }

    #[tokio::test]
    async fn a_keyboard_report_pauses_an_inflight_type() {
        let desktop = FakeDesktop::new();
        desktop.block_type.store(true, Ordering::SeqCst);
        let (addr, dir) = spawn_pair(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let type_addr = addr;
        let type_id = id;
        let job = tokio::spawn(async move {
            send(
                type_addr,
                "POST",
                &format!("/session/{type_id}/type"),
                Some(r#"{"text":"ab"}"#.into()),
            )
            .await
        });
        let ready = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let (_, _, body) = send(addr, "GET", "/companion", None).await;
                let live: serde_json::Value = serde_json::from_slice(&body).unwrap();
                if live["automation"]["injecting"] == true {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(ready.is_ok(), "type did not enter the gate");
        let sock = dir.clone();
        let status = tokio::task::spawn_blocking(move || companion_interrupt(&sock))
            .await
            .unwrap();
        assert_eq!(status, 204);
        desktop.block_type.store(false, Ordering::SeqCst);
        let (status, _, body) = job.await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 409, "{text}");
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let paused: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(paused["automation"]["phase"], "paused");
        assert_eq!(paused["interrupt"], "no keyboard interrupt available");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_companion_poll_reporting_no_keyboard_pauses() {
        let desktop = FakeDesktop::new();
        let (addr, dir) = spawn_pair(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":1,"y":2}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let sock = dir.clone();
        let body = tokio::task::spawn_blocking(move || companion_get_keyboard(&sock, false))
            .await
            .unwrap();
        assert_eq!(body["automation"]["phase"], "paused");
        assert_eq!(body["interrupt"], "no keyboard interrupt available");
        let sock = dir.clone();
        let again = tokio::task::spawn_blocking(move || companion_get_keyboard(&sock, false))
            .await
            .unwrap();
        assert_eq!(again["automation"]["phase"], "paused");
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":3,"y":4}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 501, "{text}");
        assert_eq!(lock(&desktop.mice).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_coordinate_outside_the_root_is_refused() {
        let desktop = FakeDesktop::new();
        *lock(&desktop.root_size) = Some((100, 80));
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":300,"y":10}"#.into()),
        )
        .await;
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, 400, "{text}");
        assert!(lock(&desktop.mice).is_empty());
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":10,"y":12}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
    }

    #[tokio::test]
    async fn the_drive_target_is_the_pointer_after_injection() {
        let desktop = FakeDesktop::new();
        *lock(&desktop.pointer) = Some((7, 9));
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"move","x":10,"y":12}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let live: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(live["automation"]["drive"]["x"], 7);
        assert_eq!(live["automation"]["drive"]["y"], 9);
    }

    #[tokio::test]
    async fn a_long_type_uses_one_keyboard_connection() {
        let desktop = FakeDesktop::new();
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let text = "a".repeat(64);
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/type"),
            Some(format!(r#"{{"text":"{text}"}}"#)),
        )
        .await;
        let response = String::from_utf8_lossy(&body);
        assert_eq!(status, 204, "{response}");
        assert_eq!(desktop.keyboard_sessions.load(Ordering::SeqCst), 1);
        assert!(lock(&desktop.typed).len() > 32);
    }

    #[tokio::test]
    async fn resume_after_closing_the_driving_session_uses_the_other_session() {
        let desktop = FakeDesktop::new();
        let (addr, sock) = spawn_pair(Arc::clone(&desktop)).await;
        let (_, _, driving) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{driving}/mouse"),
            Some(r#"{"action":"move","x":1,"y":1}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (status, _, _) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"pause"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (_, _, other) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(addr, "DELETE", &format!("/session/{driving}"), None).await;
        assert_eq!(status, 204);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let paused: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(paused["automation"]["phase"], "paused");
        assert_eq!(paused["session"]["id"], other.to_string());
        let status = tokio::task::spawn_blocking(move || companion_post(&sock, "resume"))
            .await
            .unwrap();
        assert_eq!(status, 204);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let resumed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(resumed["automation"]["phase"], "running");
        assert_eq!(resumed["session"]["id"], other.to_string());
    }

    async fn send_with_stale_header(addr: SocketAddr) -> (u16, http::HeaderMap, Bytes) {
        let response = dispatch(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"resume"}"#.into()),
            Some(("x-hypermesh-companion", "abab")),
        )
        .await;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let bytes = tokio::time::timeout(Duration::from_secs(2), response.into_body().collect())
            .await
            .expect("body timeout")
            .unwrap()
            .to_bytes();
        (status, headers, bytes)
    }

    #[tokio::test]
    async fn start_keeps_the_session_the_gate_is_driving() {
        let (addr, dir) = spawn_pair(FakeDesktop::new()).await;
        let (_, _, first) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{first}/mouse"),
            Some(r#"{"action":"move","x":3,"y":4}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (_, _, second) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(addr, "POST", &format!("/session/{second}/view"), None).await;
        assert_eq!(status, 200);
        let (status, _, _) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"start"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let started: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(started["session"]["id"], first.to_string());
        assert!(started["automation"]["drive"].is_null());

        let (status, _, _) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"stop"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let poll_dir = dir.clone();
        let _ = tokio::task::spawn_blocking(move || companion_get(&poll_dir))
            .await
            .unwrap();
        let (status, _, _) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"start"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let fresh: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(fresh["automation"]["phase"], "running");
        assert_eq!(fresh["session"]["id"], second.to_string());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn driving_session_stays_visible_and_pause_covers_the_desktop() {
        let desktop = FakeDesktop::new();
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, driving) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{driving}/mouse"),
            Some(r#"{"action":"move","x":12,"y":14}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (_, _, other) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(addr, "POST", &format!("/session/{other}/view"), None).await;
        assert_eq!(status, 200);

        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let shown: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(shown["automation"]["phase"], "running");
        assert_eq!(shown["session"]["id"], driving.to_string());
        assert_eq!(shown["automation"]["drive"]["pointer_known"], true);

        let (status, _, _) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"pause"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let before = lock(&desktop.mice).len();
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{other}/mouse"),
            Some(r#"{"action":"move","x":1,"y":1}"#.into()),
        )
        .await;
        assert_eq!(status, 409);
        assert_eq!(lock(&desktop.mice).len(), before);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let paused: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(paused["automation"]["phase"], "paused");
        assert_eq!(paused["session"]["id"], driving.to_string());
    }

    #[tokio::test]
    async fn type_records_the_live_pointer() {
        let desktop = FakeDesktop::new();
        *lock(&desktop.pointer) = Some((40, 70));
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/type"),
            Some(r#"{"text":"hi"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let typed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(typed["automation"]["drive"]["kind"], "type");
        assert_eq!(typed["automation"]["drive"]["x"], 40);
        assert_eq!(typed["automation"]["drive"]["y"], 70);
        assert_eq!(typed["automation"]["drive"]["pointer_known"], true);

        *lock(&desktop.pointer) = None;
        let (_, _, fresh) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(
            addr,
            "POST",
            "/companion/automation",
            Some(r#"{"action":"stop"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{fresh}/type"),
            Some(r#"{"text":"yo"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (_, _, body) = send(addr, "GET", "/companion", None).await;
        let unknown: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(unknown["automation"]["drive"]["pointer_known"], false);
    }

    #[tokio::test]
    async fn verbs_fail_without_a_session_and_after_close() {
        let desktop = FakeDesktop::new();
        let addr = spawn(Arc::clone(&desktop)).await;
        for (method, path, body) in [
            ("POST", "/session/not-a-session/view", None),
            ("GET", "/session/not-a-session/watch", None),
            ("GET", "/session/not-a-session/listen", None),
            (
                "POST",
                "/session/not-a-session/mouse",
                Some(r#"{"action":"move","x":1,"y":2}"#.to_string()),
            ),
            (
                "POST",
                "/session/not-a-session/type",
                Some(r#"{"text":"hi"}"#.to_string()),
            ),
        ] {
            let (status, _, payload) = send(addr, method, path, body).await;
            assert_eq!(status, 404, "{method} {path}");
            assert!(String::from_utf8_lossy(&payload).contains("session closed or unknown"));
        }
        assert_eq!(desktop.views.load(Ordering::SeqCst), 0);
        assert!(lock(&desktop.mice).is_empty());
        assert!(lock(&desktop.typed).is_empty());

        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, _) = send(addr, "DELETE", &format!("/session/{id}"), None).await;
        assert_eq!(status, 204);
        let (status, _, _) = send(addr, "DELETE", &format!("/session/{id}"), None).await;
        assert_eq!(status, 404);
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"click","x":3,"y":4,"button":"left"}"#.into()),
        )
        .await;
        assert_eq!(status, 404);
        assert!(lock(&desktop.mice).is_empty());
        assert_eq!(desktop.audio_opens.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn view_mouse_and_listen_use_the_open_session() {
        let desktop = FakeDesktop::new();
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, headers, body) =
            send(addr, "POST", &format!("/session/{id}/view"), None).await;
        assert_eq!(status, 200);
        assert_eq!(headers["content-type"], "image/png");
        assert!(body.starts_with(b"\x89PNG"));

        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"drag","x":1,"y":2,"to_x":8,"to_y":9,"button":"right"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        assert_eq!(
            lock(&desktop.mice).as_slice(),
            &[MouseOp::Drag {
                x: 1,
                y: 2,
                to_x: 8,
                to_y: 9,
                button: crate::input::Button::Right,
            }]
        );
        assert_eq!(desktop.pointer_sessions.load(Ordering::SeqCst), 0);

        let (status, headers, body) =
            send(addr, "GET", &format!("/session/{id}/listen"), None).await;
        assert_eq!(status, 200);
        assert_eq!(headers["x-audio-format"], AUDIO_FORMAT);
        assert_eq!(body.as_ref(), b"pcm-bytes");
    }

    #[tokio::test]
    async fn type_secret_reaches_the_desktop_and_not_the_response() {
        let desktop = FakeDesktop::new();
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(
            addr,
            serde_json::json!([{"name": "password", "source": "local", "value": "hunter2"}]),
        )
        .await;
        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/type"),
            Some(r#"{"secret":"password"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        assert!(body.is_empty());
        let typed = lock(&desktop.typed).clone();
        let expected = plan_type(
            &TypeBody {
                text: None,
                keys: Vec::new(),
                secret: None,
            },
            Some(b"hunter2"),
        )
        .unwrap();
        assert_eq!(typed, expected);
        let rendered = format!("{typed:?}");
        assert!(!rendered.contains("hunter2"));
    }

    #[tokio::test]
    async fn remote_secret_sources_do_not_open_a_session() {
        let desktop = FakeDesktop::new();
        let addr = spawn(desktop).await;
        let locator = "https://user:password@example.invalid/secret";
        for source in ["url", "chain", "ipfs"] {
            let (status, text, _) = open_session_id(
                addr,
                serde_json::json!([{
                    "name": "token",
                    "source": source,
                    "locator": locator
                }]),
            )
            .await;
            assert_eq!(status, 501, "{source}");
            assert!(text.contains(source));
            assert!(!text.contains("password"));
            assert!(!text.contains("example.invalid"));
        }
        let (status, _, _) = send(
            addr,
            "POST",
            "/session/00000000-0000-0000-0000-000000000000/view",
            None,
        )
        .await;
        assert_eq!(status, 404);
    }

    #[tokio::test]
    async fn mcp_secret_source_materializes_via_locator() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "hm-api-mcp-vault-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("secret-mcp.py");
        std::fs::write(
            &script,
            r#"#!/usr/bin/env python3
import json, sys
def recv():
    line = sys.stdin.readline()
    if not line:
        raise SystemExit(0)
    return json.loads(line)
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()
while True:
    msg = recv()
    method = msg.get("method")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"secrets","version":"0"}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"tools":[{"name":"get_secret","inputSchema":{"type":"object"}}]}})
    elif method == "tools/call":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"content":[{"type":"text","text":"typed-from-mcp"}]}})
"#,
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        std::fs::write(
            dir.join("mcp.json"),
            serde_json::json!({
                "mcpServers": {
                    "vault-fixture": {
                        "command": script.to_string_lossy(),
                        "type": "stdio"
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        let desktop = FakeDesktop::new();
        let addr = spawn_with_mcp_dir(Arc::clone(&desktop), dir.clone()).await;
        let (status, text, id) = open_session_id(
            addr,
            serde_json::json!([{
                "name": "password",
                "source": "mcp",
                "locator": "vault-fixture/get_secret"
            }]),
        )
        .await;
        assert_eq!(status, 201, "{text}");
        assert!(!text.contains("typed-from-mcp"));

        let (status, _, body) = send(
            addr,
            "POST",
            &format!("/session/{id}/type"),
            Some(r#"{"secret":"password"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        assert!(body.is_empty());
        let typed = lock(&desktop.typed).clone();
        let expected = plan_type(
            &TypeBody {
                text: None,
                keys: Vec::new(),
                secret: None,
            },
            Some(b"typed-from-mcp"),
        )
        .unwrap();
        assert_eq!(typed, expected);
        let rendered = format!("{typed:?}");
        assert!(!rendered.contains("typed-from-mcp"));

        let (status, text, _) = open_session_id(
            addr,
            serde_json::json!([{
                "name": "token",
                "source": "mcp",
                "locator": "not-a-locator"
            }]),
        )
        .await;
        assert_eq!(status, 400, "{text}");
        assert!(text.contains("server/tool"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn blank_purpose_is_rejected() {
        let addr = spawn(FakeDesktop::new()).await;
        let body = serde_json::json!({
            "purpose": " ",
            "recipes": [],
            "agent": {"name": "desk", "instructions": ""},
            "skills": []
        })
        .to_string();
        let (status, _, payload) = send(addr, "POST", "/session", Some(body)).await;
        assert_eq!(status, 400);
        assert!(String::from_utf8_lossy(&payload).contains("purpose"));
    }

    #[tokio::test]
    async fn renter_feed_uses_the_view_frame_and_hides_secrets() {
        let desktop = FakeDesktop::new();
        desktop.set_focus(Some(FocusedApp {
            wm_class: Some("gnome-terminal".into()),
            app_id: None,
            executable: None,
        }));
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(
            addr,
            serde_json::json!([{
                "name": "password",
                "source": "local",
                "value": "hunter2"
            }]),
        )
        .await;
        let feed_path = format!("/session/{id}/feed");

        let (status, _, body) = send(addr, "GET", &feed_path, None).await;
        assert_eq!(status, 200);
        let early: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(early["snapshot"].is_null());
        assert_eq!(desktop.views.load(Ordering::SeqCst), 0);

        let (status, _, view_body) = send(addr, "POST", &format!("/session/{id}/view"), None).await;
        assert_eq!(status, 200);
        assert_eq!(desktop.views.load(Ordering::SeqCst), 1);

        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/type"),
            Some(r#"{"text":"cargo test --locked hunter2"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/type"),
            Some(r#"{"secret":"password"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (status, _, _) = send(
            addr,
            "POST",
            &format!("/session/{id}/mouse"),
            Some(r#"{"action":"click","x":4,"y":9,"button":"left"}"#.into()),
        )
        .await;
        assert_eq!(status, 204);
        let (status, stream_body) = send_key(
            addr,
            "POST",
            &format!("/session/{id}/stream"),
            Some("org_fixture_ok"),
            Some(
                "\
{\"kind\":\"prompt\",\"prompt\":\"count the sheep\"}
{\"kind\":\"file\",\"name\":\"notes.txt\",\"content_base64\":\"U0VDUkVURklMRUJZVEVT\"}
{\"kind\":\"secret\",\"name\":\"token\",\"source\":\"local\",\"value\":\"stream-secret-value\"}
"
                .into(),
            ),
        )
        .await;
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&stream_body));

        let (status, _, body) = send(addr, "GET", &feed_path, None).await;
        assert_eq!(status, 200);
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(!text.contains("hunter2"));
        assert!(!text.contains("stream-secret-value"));
        assert!(!text.contains("SECRETFILEBYTES"));
        assert!(!text.contains("content_base64"));
        assert!(!text.contains("org_fixture_ok"));
        let feed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(feed["snapshot"]["mime"], "image/png");
        assert!(feed["snapshot"]["seq"].as_u64().unwrap() >= 1);
        let thinking = feed["thinking"].as_array().unwrap();
        let kinds: Vec<&str> = thinking
            .iter()
            .map(|row| row["kind"].as_str().unwrap())
            .collect();
        for kind in ["focus", "type", "mouse", "prompt", "file", "secret"] {
            assert!(kinds.contains(&kind), "{kind} missing from {kinds:?}");
        }
        let lines: Vec<&str> = thinking
            .iter()
            .map(|row| row["text"].as_str().unwrap())
            .collect();
        assert!(lines.contains(&"gnome-terminal"));
        assert!(lines.contains(&"cargo test --locked ***"));
        assert!(lines.contains(&"password"));
        assert!(lines.contains(&"click 4,9 left"));
        assert!(lines.contains(&"count the sheep"));
        assert!(lines.contains(&"notes.txt"));
        assert_eq!(feed["files"][0]["handle"], "notes.txt");
        assert!(feed["files"][0].get("content_base64").is_none());
        let secret_handles: Vec<&str> = feed["secrets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["handle"].as_str().unwrap())
            .collect();
        assert!(secret_handles.contains(&"password"));
        assert!(secret_handles.contains(&"token"));

        let (status, headers, snap) =
            send(addr, "GET", &format!("{feed_path}/snapshot"), None).await;
        assert_eq!(status, 200);
        assert_eq!(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("image/png")
        );
        assert_eq!(snap, view_body);
        assert_eq!(desktop.views.load(Ordering::SeqCst), 1);

        let (status, _, _) = send(addr, "DELETE", &format!("/session/{id}"), None).await;
        assert_eq!(status, 204);
        let (status, _, _) = send(addr, "GET", &feed_path, None).await;
        assert_eq!(status, 404);
    }

    #[tokio::test]
    async fn watch_notices_a_changed_frame() {
        let desktop = FakeDesktop::new();
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let response = dispatch(
            addr,
            "GET",
            &format!("/session/{id}/watch?mode=notice"),
            None,
            None,
        )
        .await;
        assert_eq!(response.status(), 200);
        let mut incoming = response.into_body();
        let first = read_sse(&mut incoming, Duration::from_secs(1))
            .await
            .expect("first notice");
        assert!(first.contains("\"kind\":\"notice\""));
        assert!(first.contains("\"seq\":1"));
        assert!(read_sse(&mut incoming, Duration::from_millis(120))
            .await
            .is_none());
        *lock(&desktop.frame) = encode_rgba_png(1, 1, &[1, 2, 3, 255]).unwrap();
        let second = read_sse(&mut incoming, Duration::from_secs(1))
            .await
            .expect("second notice");
        assert!(second.contains("\"seq\":2"));
        let (status, _, body) = send(addr, "GET", &format!("/session/{id}/feed"), None).await;
        assert_eq!(status, 200);
        let feed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(feed["snapshot"]["seq"], 2);
        let (status, _, snap) =
            send(addr, "GET", &format!("/session/{id}/feed/snapshot"), None).await;
        assert_eq!(status, 200);
        assert_eq!(
            snap.as_ref(),
            encode_rgba_png(1, 1, &[1, 2, 3, 255]).unwrap().as_slice()
        );
    }

    #[tokio::test]
    async fn root_refusal_is_forbidden() {
        let desktop = FakeDesktop::new();
        desktop.root.store(true, Ordering::SeqCst);
        let addr = spawn(desktop).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, _, payload) = send(addr, "POST", &format!("/session/{id}/view"), None).await;
        assert_eq!(status, 403);
        assert!(String::from_utf8_lossy(&payload).contains("root"));
    }

    async fn read_sse(body: &mut hyper::body::Incoming, timeout: Duration) -> Option<String> {
        let mut buf = Vec::new();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(end) = find_event_end(&buf) {
                let event: Vec<u8> = buf.drain(..end).collect();
                return Some(String::from_utf8(event).unwrap());
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let frame = tokio::time::timeout(remaining, body.frame()).await.ok()??;
            let frame = frame.ok()?;
            if let Ok(data) = frame.into_data() {
                buf.extend_from_slice(&data);
            }
        }
    }

    fn find_event_end(buf: &[u8]) -> Option<usize> {
        buf.windows(2)
            .position(|pair| pair == b"\n\n")
            .map(|index| index + 2)
    }

    async fn spawn_with_door(door: Arc<dyn crate::prompt::PromptDoor>) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(AppState::with_prompt_door(
            FakeDesktop::new(),
            Duration::from_millis(20),
            door,
        ));
        spawn_companion_watch(Arc::clone(&state));
        tokio::spawn(async move {
            axum::serve(listener, router(state)).await.unwrap();
        });
        addr
    }

    #[tokio::test]
    async fn prompt_key_is_forwarded_and_the_auditor_records_the_pass() {
        let door = crate::prompt::RecordingDoor::ok("baa");
        let addr = spawn_with_door(door.clone()).await;
        let (status, _, body) = send(addr, "GET", "/audit", None).await;
        assert_eq!(status, 200);
        assert_eq!(body.as_ref(), b"[]");

        let (status, _, body) = send(
            addr,
            "POST",
            "/prompt",
            Some(r#"{"prompt":"count the sheep","api_key":"org_fixture_ok"}"#.into()),
        )
        .await;
        assert_eq!(status, 200);
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(!text.contains("org_fixture_ok"));
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["response"], "baa");
        assert_eq!(value["passed_through"], true);

        let calls = door.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].api_key, "org_fixture_ok");
        assert_eq!(calls[0].prompt, "count the sheep");
        assert_eq!(
            calls[0].model.as_deref(),
            Some(crate::prompt::DEFAULT_CATALOG_ID)
        );

        let (status, _, body) = send(addr, "GET", "/audit", None).await;
        assert_eq!(status, 200);
        let audit_text = String::from_utf8(body.to_vec()).unwrap();
        assert!(!audit_text.contains("org_fixture_ok"));
        let audit: serde_json::Value = serde_json::from_str(&audit_text).unwrap();
        assert_eq!(audit[0]["prompt"], "count the sheep");
        assert_eq!(audit[0]["passed_through"], true);
        assert_eq!(audit[0]["outcome"], "forwarded");
        assert_eq!(audit[0]["response"], "baa");
        assert_eq!(audit[0]["model"], crate::prompt::DEFAULT_CATALOG_ID);
    }

    #[tokio::test]
    async fn prompt_rejects_a_missing_or_invalid_api_key() {
        let door = crate::prompt::RecordingDoor::ok("baa");
        let addr = spawn_with_door(door.clone()).await;

        let (status, _, body) = send(
            addr,
            "POST",
            "/prompt",
            Some(r#"{"prompt":"count the sheep"}"#.into()),
        )
        .await;
        assert_eq!(status, 401);
        assert!(String::from_utf8_lossy(&body).contains("api key is required"));

        let (status, _, body) = send(
            addr,
            "POST",
            "/prompt",
            Some(r#"{"prompt":"count the sheep","api_key":"hm_dev_fixture_tail"}"#.into()),
        )
        .await;
        assert_eq!(status, 401);
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("hm_dev_"));
        assert!(!text.contains("fixture_tail"));
        assert!(door.calls().is_empty());

        let (_, _, body) = send(addr, "GET", "/audit", None).await;
        let audit_text = String::from_utf8(body.to_vec()).unwrap();
        assert!(!audit_text.contains("fixture_tail"));
        assert!(!audit_text.contains("org_fixture_ok"));
        let audit: serde_json::Value = serde_json::from_str(&audit_text).unwrap();
        assert_eq!(audit.as_array().unwrap().len(), 2);
        assert!(audit
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["passed_through"] == false && row["outcome"] == "rejected"));
    }

    #[tokio::test]
    async fn unconfigured_prompt_door_does_not_invent_a_response() {
        let addr = spawn(FakeDesktop::new()).await;
        let (status, _, body) = send(
            addr,
            "POST",
            "/prompt",
            Some(r#"{"prompt":"count the sheep","api_key":"org_fixture_ok"}"#.into()),
        )
        .await;
        assert_eq!(status, 502);
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("not configured"));
        assert!(!text.contains("org_fixture_ok"));
        let (_, _, body) = send(addr, "GET", "/audit", None).await;
        let audit: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(audit[0]["passed_through"], false);
        assert_eq!(audit[0]["outcome"], "door_failed");
        assert_eq!(audit[0]["prompt"], "count the sheep");
        assert!(!String::from_utf8_lossy(&body).contains("org_fixture_ok"));
    }

    #[tokio::test]
    async fn supervisor_fixture_hides_the_key_on_the_http_door() {
        let fixture = crate::prompt::ChatFixture::spawn(
            200,
            r#"{"choices":[{"message":{"content":"echo org_fixture_ok"}}]}"#,
        );
        let door = Arc::new(crate::prompt::SupervisorDoor::connect(&fixture.url).unwrap());
        let addr = spawn_with_door(door).await;

        let (status, _, body) = send(
            addr,
            "POST",
            "/prompt",
            Some(r#"{"prompt":"count the sheep"}"#.into()),
        )
        .await;
        assert_eq!(status, 401);
        assert!(String::from_utf8_lossy(&body).contains("api key is required"));

        let (status, _, body) = send(
            addr,
            "POST",
            "/prompt",
            Some(r#"{"prompt":"count the sheep","api_key":"hm_site_fixture_tail"}"#.into()),
        )
        .await;
        assert_eq!(status, 401);
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("hm_site_"));
        assert!(!text.contains("fixture_tail"));
        assert!(fixture.hits().is_empty());

        let (status, _, body) = send(
            addr,
            "POST",
            "/prompt",
            Some(r#"{"prompt":"count the sheep","api_key":"org_fixture_ok"}"#.into()),
        )
        .await;
        assert_eq!(status, 200);
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(!text.contains("org_fixture_ok"));
        assert!(text.contains("echo ***"));
        let hits = fixture.hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].api_key, "org_fixture_ok");
        assert_eq!(hits[0].path, "/v1/chat/completions");

        let (status, _, body) = send(addr, "GET", "/audit", None).await;
        assert_eq!(status, 200);
        let audit_text = String::from_utf8(body.to_vec()).unwrap();
        assert!(!audit_text.contains("org_fixture_ok"));
        assert!(!audit_text.contains("fixture_tail"));
        let audit: serde_json::Value = serde_json::from_str(&audit_text).unwrap();
        assert_eq!(audit.as_array().unwrap().len(), 3);
        assert_eq!(audit[2]["outcome"], "forwarded");
        assert_eq!(audit[2]["passed_through"], true);
        assert_eq!(audit[2]["response"], "echo ***");
        assert_eq!(audit[2]["prompt"], "count the sheep");
        assert!(audit
            .as_array()
            .unwrap()
            .iter()
            .take(2)
            .all(|row| row["outcome"] == "rejected" && row["passed_through"] == false));
    }

    async fn send_key(
        addr: SocketAddr,
        method: &str,
        path: &str,
        api_key: Option<&str>,
        body: Option<String>,
    ) -> (u16, Bytes) {
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (mut sender, conn) = handshake(TokioIo::new(stream)).await.unwrap();
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let payload = body.unwrap_or_default();
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", addr.to_string())
            .header("content-type", "application/x-ndjson");
        if let Some(api_key) = api_key {
            request = request.header("x-api-key", api_key);
        }
        let request = request.body(Full::new(Bytes::from(payload))).unwrap();
        let response = sender.send_request(request).await.unwrap();
        let status = response.status().as_u16();
        let bytes = tokio::time::timeout(Duration::from_secs(2), response.into_body().collect())
            .await
            .expect("body timeout")
            .unwrap()
            .to_bytes();
        (status, bytes)
    }

    fn stream_body() -> String {
        "\
{\"kind\":\"prompt\",\"prompt\":\"count the sheep\",\"api_key\":\"org_fixture_ok\"}
{\"kind\":\"file\",\"name\":\"note.txt\",\"content_base64\":\"aGVsbG8=\"}
{\"kind\":\"secret\",\"name\":\"token\",\"source\":\"local\",\"value\":\"stream-secret-value\"}
"
        .into()
    }

    #[tokio::test]
    async fn stream_rejects_a_key_before_it_accepts_input() {
        let addr = spawn(FakeDesktop::new()).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let path = format!("/session/{id}/stream");
        let canary = stream_body();

        let (status, body) = send_key(addr, "POST", &path, None, Some(canary.clone())).await;
        assert_eq!(status, 401);
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("api key is required"));
        assert!(!text.contains("org_fixture_ok"));
        assert!(!text.contains("stream-secret-value"));
        assert!(!text.contains("count the sheep"));

        let (status, body) = send_key(addr, "POST", &path, Some("   "), Some(canary.clone())).await;
        assert_eq!(status, 401);
        assert!(String::from_utf8_lossy(&body).contains("api key is required"));

        for key in [
            "hm_dev_fixture_tail",
            "hm_rtr_fixture_tail",
            "hm_site_fixture_tail",
        ] {
            let (status, body) =
                send_key(addr, "POST", &path, Some(key), Some(canary.clone())).await;
            assert_eq!(status, 401);
            let text = String::from_utf8(body.to_vec()).unwrap();
            let prefix = key.split("_fixture").next().unwrap();
            let prefix = format!("{prefix}_");
            assert!(text.contains(&prefix));
            assert!(!text.contains("fixture_tail"));
            assert!(!text.contains("stream-secret-value"));
            assert!(!text.contains("count the sheep"));
        }

        let (status, body) =
            send_key(addr, "GET", &format!("/session/{id}/inbox"), None, None).await;
        assert_eq!(status, 200);
        let inbox: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(inbox["prompts"].as_array().unwrap().is_empty());
        assert!(inbox["files"].as_array().unwrap().is_empty());
        assert!(inbox["secrets"].as_array().unwrap().is_empty());
        assert!(!String::from_utf8_lossy(&body).contains("stream-secret-value"));
        assert!(!String::from_utf8_lossy(&body).contains("org_fixture_ok"));

        let (_, _, body) = send(addr, "GET", "/audit", None).await;
        assert_eq!(body.as_ref(), b"[]");
    }

    #[tokio::test]
    async fn stream_delivers_a_prompt_and_a_file_onto_an_open_session() {
        let addr = spawn(FakeDesktop::new()).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let path = format!("/session/{id}/stream");
        let (status, body) = send_key(
            addr,
            "POST",
            &path,
            Some("org_fixture_ok"),
            Some(stream_body()),
        )
        .await;
        assert_eq!(status, 200);
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(!text.contains("org_fixture_ok"));
        assert!(!text.contains("stream-secret-value"));
        assert!(text.contains("\"kind\":\"prompt\""));
        assert!(text.contains("\"name\":\"note.txt\""));
        assert!(text.contains("\"handle\":\"token\""));

        let (status, body) =
            send_key(addr, "GET", &format!("/session/{id}/inbox"), None, None).await;
        assert_eq!(status, 200);
        let inbox_text = String::from_utf8(body.to_vec()).unwrap();
        assert!(!inbox_text.contains("org_fixture_ok"));
        assert!(!inbox_text.contains("stream-secret-value"));
        let inbox: serde_json::Value = serde_json::from_str(&inbox_text).unwrap();
        assert_eq!(inbox["prompts"][0]["prompt"], "count the sheep");
        assert!(inbox["prompts"][0].get("model").is_none());
        assert_eq!(inbox["files"][0]["name"], "note.txt");
        assert_eq!(inbox["files"][0]["bytes"], 5);
        assert_eq!(inbox["files"][0]["content_base64"], "aGVsbG8=");
        assert_eq!(inbox["secrets"][0]["handle"], "token");
    }

    #[tokio::test]
    async fn unknown_model_is_rejected_before_the_door() {
        let door = crate::prompt::RecordingDoor::ok("baa");
        let addr = spawn_with_door(door.clone()).await;
        let (status, _, body) = send(
            addr,
            "POST",
            "/prompt",
            Some(
                r#"{"prompt":"count the sheep","api_key":"org_fixture_ok","model":"not-a-model"}"#
                    .into(),
            ),
        )
        .await;
        assert_eq!(status, 400);
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("unknown model"));
        assert!(!text.contains("org_fixture_ok"));
        assert!(door.calls().is_empty());
        let (_, _, body) = send(addr, "GET", "/audit", None).await;
        let audit: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(audit[0]["outcome"], "rejected");
        assert!(audit[0].get("door").is_none());
    }

    #[tokio::test]
    async fn stream_prompt_reaches_the_door_and_stays_on_the_inbox() {
        let door = crate::prompt::RecordingDoor::ok("baa");
        let addr = spawn_with_door(door.clone()).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let (status, body) = send_key(
            addr,
            "POST",
            &format!("/session/{id}/stream"),
            Some("org_fixture_ok"),
            Some(
                "{\"kind\":\"prompt\",\"prompt\":\"count the sheep\"}\n{\"kind\":\"file\",\"name\":\"note.txt\",\"content_base64\":\"aGVsbG8=\"}\n"
                    .into(),
            ),
        )
        .await;
        assert_eq!(status, 200);
        assert!(!String::from_utf8_lossy(&body).contains("org_fixture_ok"));
        assert_eq!(door.calls().len(), 1);
        assert_eq!(door.calls()[0].prompt, "count the sheep");
        assert_eq!(
            door.calls()[0].model.as_deref(),
            Some(crate::prompt::DEFAULT_CATALOG_ID)
        );
        let (status, body) =
            send_key(addr, "GET", &format!("/session/{id}/inbox"), None, None).await;
        assert_eq!(status, 200);
        let inbox: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(inbox["prompts"][0]["prompt"], "count the sheep");
        assert!(inbox["prompts"][0].get("model").is_none());
        assert_eq!(inbox["files"][0]["name"], "note.txt");
        let (_, _, body) = send(addr, "GET", "/audit", None).await;
        let audit: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(audit[0]["outcome"], "forwarded");
        assert_eq!(audit[0]["model"], crate::prompt::DEFAULT_CATALOG_ID);
    }
}
