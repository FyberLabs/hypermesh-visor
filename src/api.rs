use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::Engine;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::desktop::{AudioRead, Desktop, DesktopError, AUDIO_FORMAT};
use crate::harness::{Agent, Harness, HarnessError, Recipe, Skill};
use crate::input::{plan_type, MouseOp, PlanError, TypeBody};
use crate::session::{Session, Verb};
use crate::vault::{SecretRequest, Vault, VaultError};

pub struct AppState {
    sessions: Mutex<HashMap<Uuid, Session>>,
    desktop: Arc<dyn Desktop>,
    watch_poll: Duration,
}

impl AppState {
    pub fn new(desktop: Arc<dyn Desktop>, watch_poll: Duration) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            desktop,
            watch_poll,
        }
    }

    fn close_all(&self) {
        lock(&self.sessions).clear();
    }

    fn has_session(&self, id: Uuid) -> bool {
        lock(&self.sessions).contains_key(&id)
    }

    fn mark_verb(&self, id: Uuid, verb: Verb) -> bool {
        let mut sessions = lock(&self.sessions);
        let Some(session) = sessions.get_mut(&id) else {
            return false;
        };
        session.mark_verb(verb);
        true
    }

    /// Purpose and current verb for the desktop companion.
    /// The CLI auth key is not a session secret and is not included.
    fn companion_session(&self) -> Option<CompanionSession> {
        let sessions = lock(&self.sessions);
        let session = sessions.values().max_by_key(|session| session.touched())?;
        Some(CompanionSession {
            id: session.id(),
            purpose: session.purpose().to_string(),
            verb: session.verb().map(Verb::as_str),
        })
    }
}

#[derive(Serialize)]
struct CompanionBody {
    open: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<CompanionSession>,
}

#[derive(Serialize)]
struct CompanionSession {
    id: Uuid,
    purpose: String,
    verb: Option<&'static str>,
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/companion", get(companion))
        .route("/session", post(open_session))
        .route("/session/:id", delete(close_session))
        .route("/session/:id/view", post(view))
        .route("/session/:id/watch", get(watch))
        .route("/session/:id/listen", get(listen))
        .route("/session/:id/mouse", post(mouse))
        .route("/session/:id/type", post(type_keys))
        .with_state(state)
}

pub async fn serve(addr: SocketAddr, state: Arc<AppState>) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    println!("listening {local}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let app = router(Arc::clone(&state));
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            state.close_all();
        })
        .await
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
}

#[derive(Serialize)]
struct OpenedSession {
    id: Uuid,
    purpose: String,
    recipes: Vec<Recipe>,
    agent: Agent,
    skills: Vec<Skill>,
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
    let vault = Vault::open(&req.secrets).map_err(ApiError::from_vault)?;
    let id = Uuid::new_v4();
    let opened = OpenedSession {
        id,
        purpose: harness.purpose.clone(),
        recipes: harness.recipes.clone(),
        agent: harness.agent.clone(),
        skills: harness.skills.clone(),
    };
    lock(&state.sessions).insert(id, Session::create(id, harness, vault));
    Ok((StatusCode::CREATED, Json(opened)))
}

async fn companion(State(state): State<Arc<AppState>>) -> Json<CompanionBody> {
    let session = state.companion_session();
    Json(CompanionBody {
        open: session.is_some(),
        session,
    })
}

async fn close_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    if lock(&state.sessions).remove(&id).is_none() {
        return Err(ApiError::missing());
    }
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
    let frame = tokio::task::spawn_blocking(move || desktop.view())
        .await
        .map_err(|_| ApiError::internal("desktop task failed"))?
        .map_err(ApiError::from_desktop)?;
    if !state.has_session(id) {
        return Err(ApiError::missing());
    }
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
        let viewed = tokio::task::spawn_blocking(move || desktop.view()).await;
        let frame = match viewed {
            Ok(Ok(frame)) => frame,
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
        let digest = hash_frame(&frame.bytes);
        if last != Some(digest) {
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

async fn mouse(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(op): Json<MouseOp>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    if !state.mark_verb(id, Verb::Mouse) {
        return Err(ApiError::missing());
    }
    let desktop = Arc::clone(&state.desktop);
    let state2 = Arc::clone(&state);
    tokio::task::spawn_blocking(move || {
        if !state2.has_session(id) {
            return Err(ApiError::missing());
        }
        desktop.mouse(&op).map_err(ApiError::from_desktop)
    })
    .await
    .map_err(|_| ApiError::internal("desktop task failed"))??;
    Ok(StatusCode::NO_CONTENT)
}

async fn type_keys(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(mut body): Json<TypeBody>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let secret = {
        let mut sessions = lock(&state.sessions);
        let session = sessions.get_mut(&id).ok_or_else(ApiError::missing)?;
        session.mark_verb(Verb::Type);
        match &body.secret {
            Some(name) => Some(
                session
                    .vault()
                    .get(name)
                    .ok_or_else(|| ApiError::bad(format!("unknown secret \"{name}\"")))?,
            ),
            None => None,
        }
    };
    let strokes = plan_type(&body, secret.as_ref().map(|value| value.as_slice()))
        .map_err(ApiError::from_plan)?;
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
        if !state2.has_session(id) {
            return Err(ApiError::missing());
        }
        desktop.type_input(&strokes).map_err(ApiError::from_desktop)
    })
    .await
    .map_err(|_| ApiError::internal("desktop task failed"))??;
    Ok(StatusCode::NO_CONTENT)
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
        let status = if matches!(err, VaultError::NotImplemented(_)) {
            StatusCode::NOT_IMPLEMENTED
        } else {
            StatusCode::BAD_REQUEST
        };
        Self {
            status,
            message: err.to_string(),
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
        views: AtomicUsize,
        audio_opens: AtomicUsize,
        root: AtomicBool,
    }

    impl FakeDesktop {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                frame: Mutex::new(encode_rgba_png(1, 1, &[9, 8, 7, 255]).unwrap()),
                audio: Mutex::new(b"pcm-bytes".to_vec()),
                mice: Mutex::new(Vec::new()),
                typed: Mutex::new(Vec::new()),
                views: AtomicUsize::new(0),
                audio_opens: AtomicUsize::new(0),
                root: AtomicBool::new(false),
            })
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
            lock(&self.mice).push(op.clone());
            Ok(())
        }

        fn type_input(&self, strokes: &[Stroke]) -> Result<(), DesktopError> {
            lock(&self.typed).extend_from_slice(strokes);
            Ok(())
        }
    }

    async fn spawn(desktop: Arc<FakeDesktop>) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(AppState::new(desktop, Duration::from_millis(20)));
        tokio::spawn(async move {
            axum::serve(listener, router(state)).await.unwrap();
        });
        addr
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
        let response = dispatch(addr, method, path, body).await;
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
    ) -> hyper::Response<hyper::body::Incoming> {
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (mut sender, conn) = handshake(TokioIo::new(stream)).await.unwrap();
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let payload = body.unwrap_or_default();
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", addr.to_string())
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(payload)))
            .unwrap();
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
        for source in ["url", "mcp", "chain", "ipfs"] {
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
    async fn watch_notices_a_changed_frame() {
        let desktop = FakeDesktop::new();
        let addr = spawn(Arc::clone(&desktop)).await;
        let (_, _, id) = open_session_id(addr, serde_json::json!([])).await;
        let response = dispatch(
            addr,
            "GET",
            &format!("/session/{id}/watch?mode=notice"),
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
}
