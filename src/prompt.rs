//! v0 prompt pass-through.
//!
//! A prompt and an API key come in. The orchestrator forwards that prompt,
//! unchanged, through one [`PromptDoor`]. The auditor records what was sent,
//! whether it passed through, and the outcome. The response comes back.
//!
//! The cloud agent that serves chat today is the renter supervisor in
//! hypermesh-host. That process posts `POST /v1/chat/completions` with
//! `X-Api-Key` and its lease headers. [`SupervisorDoor`] posts the prompt
//! and the org API key to that chat path. The desktop daemon uses it only
//! when a supervisor URL is set. The default door stays unconfigured.

use std::fmt;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

/// Host, router, and site credentials are not renter API keys.
/// Same prefixes the CLI rejects.
pub const FORBIDDEN_RENTER_PREFIXES: &[&str] = &["hm_dev_", "hm_rtr_", "hm_site_"];

/// One prompt handed to the cloud-agent door.
///
/// `model` is the caller's string. v0 does not choose or rewrite it.
#[derive(Clone)]
pub struct ForwardedPrompt {
    pub prompt: String,
    pub api_key: String,
    pub model: Option<String>,
}

impl fmt::Debug for ForwardedPrompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ForwardedPrompt")
            .field("prompt", &self.prompt)
            .field("api_key", &"***")
            .field("model", &self.model)
            .finish()
    }
}

/// The single door v0 knows how to call.
///
/// Implementations must not log `api_key`.
pub trait PromptDoor: Send + Sync {
    fn forward(&self, call: &ForwardedPrompt) -> Result<String, String>;
}

/// Used by the desktop daemon. It refuses the call instead of inventing a reply.
pub struct UnconfiguredDoor;

impl PromptDoor for UnconfiguredDoor {
    fn forward(&self, _call: &ForwardedPrompt) -> Result<String, String> {
        Err("prompt door is not configured".into())
    }
}

/// Renter supervisor chat path. Same path `hypermesh-host` posts to.
pub const CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";

const CONTROL_PLANE_STUB_PATH: &str = "/api/v1/hypermesh/renter/chat/completions";
const MAX_CHAT_BYTES: usize = 1 << 20;

/// Posts one prompt to the renter supervisor chat door.
///
/// The org API key is the `X-Api-Key` header on that request. This door does
/// not store the key, and it does not add a lease id or a tenant id.
pub struct SupervisorDoor {
    endpoint: String,
    agent: ureq::Agent,
}

impl fmt::Debug for SupervisorDoor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SupervisorDoor")
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

impl SupervisorDoor {
    pub fn connect(chat_base: &str) -> Result<Self, String> {
        let endpoint = completions_url(chat_base)?;
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .redirects(0)
            .build();
        Ok(Self { endpoint, agent })
    }
}

impl PromptDoor for SupervisorDoor {
    fn forward(&self, call: &ForwardedPrompt) -> Result<String, String> {
        let payload = ChatBody {
            model: call.model.as_deref(),
            messages: [ChatMessage {
                role: "user",
                content: &call.prompt,
            }],
        };
        let body = serde_json::to_vec(&payload).map_err(|_| "prompt could not be encoded")?;
        match self
            .agent
            .post(&self.endpoint)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .set("X-Api-Key", &call.api_key)
            .send_bytes(&body)
        {
            Ok(response) => completion_text(&read_limited(response)?),
            Err(ureq::Error::Status(code, response)) => {
                let _ = read_limited(response);
                Err(format!("supervisor chat HTTP {code}"))
            }
            Err(_) => Err("supervisor chat request failed".into()),
        }
    }
}

/// Blank or missing `chat_base` keeps the unconfigured door.
/// A set URL is the renter supervisor chat base and does not carry a secret.
pub fn open_supervisor_door(chat_base: Option<&str>) -> Result<Arc<dyn PromptDoor>, String> {
    match chat_base.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(Arc::new(UnconfiguredDoor)),
        Some(base) => Ok(Arc::new(SupervisorDoor::connect(base)?)),
    }
}

#[derive(Serialize)]
struct ChatBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    messages: [ChatMessage<'a>; 1],
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

fn completions_url(chat_base: &str) -> Result<String, String> {
    let chat_base = chat_base.trim();
    if chat_base.is_empty() {
        return Err("supervisor url is required".into());
    }
    if chat_base.contains(CONTROL_PLANE_STUB_PATH) {
        return Err("refusing control-plane chat stub".into());
    }
    let (scheme, rest) = if let Some(rest) = chat_base.strip_prefix("https://") {
        ("https", rest)
    } else if let Some(rest) = chat_base.strip_prefix("http://") {
        ("http", rest)
    } else {
        return Err("supervisor url must be an http(s) URL".into());
    };
    if rest.is_empty() || rest.starts_with('/') {
        return Err("supervisor url must be an http(s) URL".into());
    }
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    if authority.is_empty() || authority.contains('@') || authority.contains([' ', '?', '#']) {
        return Err("supervisor url must be an http(s) URL".into());
    }
    if path.contains(['?', '#']) {
        return Err("supervisor chat url must not carry a query".into());
    }
    let path = if path == "/" {
        CHAT_COMPLETIONS_PATH.to_string()
    } else if path == CHAT_COMPLETIONS_PATH {
        path
    } else {
        return Err(format!(
            "supervisor chat path must be {CHAT_COMPLETIONS_PATH}"
        ));
    };
    Ok(format!("{scheme}://{authority}{path}"))
}

fn read_limited(response: ureq::Response) -> Result<String, String> {
    let mut buf = Vec::new();
    response
        .into_reader()
        .take((MAX_CHAT_BYTES + 1) as u64)
        .read_to_end(&mut buf)
        .map_err(|_| "supervisor chat response could not be read".to_string())?;
    if buf.len() > MAX_CHAT_BYTES {
        return Err("supervisor chat response is too large".into());
    }
    String::from_utf8(buf).map_err(|_| "supervisor chat response was not utf-8".into())
}

fn completion_text(body: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|_| "supervisor chat response was not json")?;
    value
        .pointer("/choices/0/message/content")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "supervisor chat response had no message".into())
}

#[derive(Debug, PartialEq, Eq)]
pub enum PromptError {
    MissingKey,
    InvalidKey { prefix: &'static str },
    EmptyPrompt,
    Door(String),
}

impl fmt::Display for PromptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingKey => write!(f, "api key is required"),
            Self::InvalidKey { prefix } => write!(
                f,
                "{prefix} is not a renter identity; use an org API key (purpose: renter)"
            ),
            Self::EmptyPrompt => write!(f, "prompt is required"),
            Self::Door(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for PromptError {}

/// What the auditor keeps. The API key is not a field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AuditRecord {
    pub prompt: String,
    pub model: Option<String>,
    pub passed_through: bool,
    pub outcome: String,
    pub response: Option<String>,
    pub reason: Option<String>,
}

#[derive(Default)]
struct Auditor {
    records: Vec<AuditRecord>,
}

impl Auditor {
    fn record(&mut self, record: AuditRecord) {
        self.records.push(record);
    }
}

/// Orchestrator plus auditor. One submit is one audit row.
pub struct PromptPass {
    door: Arc<dyn PromptDoor>,
    auditor: Mutex<Auditor>,
}

impl PromptPass {
    pub fn new(door: Arc<dyn PromptDoor>) -> Self {
        Self {
            door,
            auditor: Mutex::new(Auditor::default()),
        }
    }

    pub fn unconfigured() -> Self {
        Self::new(Arc::new(UnconfiguredDoor))
    }

    pub fn submit(
        &self,
        prompt: &str,
        api_key: &str,
        model: Option<&str>,
    ) -> Result<String, PromptError> {
        let sent = prompt.trim().to_string();
        let model = normalize_model(model);
        let key = match validate_api_key(api_key) {
            Ok(key) => key.to_string(),
            Err(err) => {
                self.record(rejected(&sent, model, &err));
                return Err(err);
            }
        };
        if sent.is_empty() {
            let err = PromptError::EmptyPrompt;
            self.record(rejected(&sent, model, &err));
            return Err(err);
        }
        let call = ForwardedPrompt {
            prompt: sent.clone(),
            api_key: key.clone(),
            model: model.clone(),
        };
        // TODO(model-selection): v0 forwards `model` only when the caller set it.
        // Choosing a catalog id belongs here, before `door.forward`, and must
        // not change how the auditor records the pass.
        // TODO(smart-routing): v0 has a single PromptDoor. A later router can
        // pick the door from the lease or the class without rewriting the auditor.
        // TODO(moe): do not fan the prompt out. Mixture-of-experts wraps
        // PromptDoor; it does not replace this pass-through.
        match self.door.forward(&call) {
            Ok(response) => {
                let response = redact(&response, &key);
                self.record(AuditRecord {
                    prompt: sent,
                    model,
                    passed_through: true,
                    outcome: "forwarded".into(),
                    response: Some(response.clone()),
                    reason: None,
                });
                Ok(response)
            }
            Err(message) => {
                let message = redact(&message, &key);
                self.record(AuditRecord {
                    prompt: sent,
                    model,
                    passed_through: false,
                    outcome: "door_failed".into(),
                    response: None,
                    reason: Some(message.clone()),
                });
                Err(PromptError::Door(message))
            }
        }
    }

    pub fn audit(&self) -> Vec<AuditRecord> {
        let auditor = self
            .auditor
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        auditor.records.clone()
    }

    fn record(&self, record: AuditRecord) {
        let mut auditor = self
            .auditor
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        auditor.record(record);
    }
}

pub fn validate_api_key(key: &str) -> Result<&str, PromptError> {
    let key = key.trim();
    if key.is_empty() {
        return Err(PromptError::MissingKey);
    }
    for prefix in FORBIDDEN_RENTER_PREFIXES {
        if key.starts_with(prefix) {
            return Err(PromptError::InvalidKey { prefix });
        }
    }
    Ok(key)
}

fn normalize_model(model: Option<&str>) -> Option<String> {
    model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn rejected(prompt: &str, model: Option<String>, err: &PromptError) -> AuditRecord {
    AuditRecord {
        prompt: prompt.to_string(),
        model,
        passed_through: false,
        outcome: "rejected".into(),
        response: None,
        reason: Some(err.to_string()),
    }
}

fn redact(text: &str, key: &str) -> String {
    if key.is_empty() {
        return text.to_string();
    }
    text.replace(key, "***")
}

#[cfg(test)]
pub(crate) struct RecordingDoor {
    calls: Mutex<Vec<ForwardedPrompt>>,
    response: String,
    fail: Option<String>,
}

#[cfg(test)]
impl RecordingDoor {
    pub(crate) fn ok(response: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            response: response.into(),
            fail: None,
        })
    }

    pub(crate) fn calls(&self) -> Vec<ForwardedPrompt> {
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

#[cfg(test)]
impl PromptDoor for RecordingDoor {
    fn forward(&self, call: &ForwardedPrompt) -> Result<String, String> {
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(call.clone());
        if let Some(message) = &self.fail {
            return Err(message.clone());
        }
        Ok(self.response.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_KEY: &str = "org_fixture_ok";
    const FIXTURE_PROMPT: &str = "count the sheep";

    #[test]
    fn fixture_key_is_forwarded_and_the_auditor_records_the_pass() {
        let door = RecordingDoor::ok("baa");
        let pass = PromptPass::new(door.clone());
        let response = pass
            .submit(FIXTURE_PROMPT, FIXTURE_KEY, None)
            .expect("forward");
        assert_eq!(response, "baa");

        let calls = door.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].api_key, FIXTURE_KEY);
        assert_eq!(calls[0].prompt, FIXTURE_PROMPT);
        assert_eq!(calls[0].model, None);
        assert!(!format!("{:?}", calls[0]).contains(FIXTURE_KEY));

        let audit = pass.audit();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].prompt, FIXTURE_PROMPT);
        assert!(audit[0].passed_through);
        assert_eq!(audit[0].outcome, "forwarded");
        assert_eq!(audit[0].response.as_deref(), Some("baa"));
        let rendered = format!("{audit:?}");
        assert!(!rendered.contains(FIXTURE_KEY));
        let json = serde_json::to_string(&audit).unwrap();
        assert!(!json.contains(FIXTURE_KEY));
    }

    #[test]
    fn missing_and_invalid_keys_are_rejected_before_the_door() {
        let door = RecordingDoor::ok("baa");
        let pass = PromptPass::new(door.clone());

        let missing = pass.submit(FIXTURE_PROMPT, "  ", None).unwrap_err();
        assert_eq!(missing, PromptError::MissingKey);
        assert_eq!(missing.to_string(), "api key is required");

        for (key, prefix) in [
            ("hm_dev_fixture_tail", "hm_dev_"),
            ("hm_rtr_fixture_tail", "hm_rtr_"),
            ("hm_site_fixture_tail", "hm_site_"),
        ] {
            let err = pass.submit(FIXTURE_PROMPT, key, None).unwrap_err();
            assert_eq!(err, PromptError::InvalidKey { prefix });
            let text = err.to_string();
            assert!(text.contains(prefix));
            assert!(!text.contains("fixture_tail"));
        }

        assert!(door.calls().is_empty());
        let audit = pass.audit();
        assert_eq!(audit.len(), 4);
        assert!(audit.iter().all(|row| !row.passed_through));
        assert!(audit.iter().all(|row| row.outcome == "rejected"));
        let rendered = format!("{audit:?}");
        assert!(!rendered.contains("fixture_tail"));
        assert!(!rendered.contains(FIXTURE_KEY));
    }

    #[test]
    fn caller_model_is_forwarded_without_selection() {
        let door = RecordingDoor::ok("ok");
        let pass = PromptPass::new(door.clone());
        pass.submit(FIXTURE_PROMPT, FIXTURE_KEY, Some("caller-picked"))
            .unwrap();
        pass.submit(FIXTURE_PROMPT, FIXTURE_KEY, Some("  "))
            .unwrap();
        let calls = door.calls();
        assert_eq!(calls[0].model.as_deref(), Some("caller-picked"));
        assert_eq!(calls[1].model, None);
        assert_ne!(calls[0].model.as_deref(), Some("llama-3.1-8b-q4"));
        let audit = pass.audit();
        assert_eq!(audit[0].model.as_deref(), Some("caller-picked"));
        assert_eq!(audit[1].model, None);
    }

    #[test]
    fn door_response_does_not_carry_the_api_key() {
        let door = RecordingDoor::ok(format!("echo {FIXTURE_KEY}"));
        let pass = PromptPass::new(door.clone());
        let response = pass.submit(FIXTURE_PROMPT, FIXTURE_KEY, None).unwrap();
        assert_eq!(response, "echo ***");
        assert_eq!(door.calls()[0].api_key, FIXTURE_KEY);
        let audit = pass.audit();
        assert_eq!(audit[0].response.as_deref(), Some("echo ***"));
        assert!(!format!("{audit:?}").contains(FIXTURE_KEY));
    }

    #[test]
    fn empty_prompt_is_rejected_after_the_key_checks() {
        let door = RecordingDoor::ok("baa");
        let pass = PromptPass::new(door.clone());
        let err = pass.submit("  ", FIXTURE_KEY, None).unwrap_err();
        assert_eq!(err, PromptError::EmptyPrompt);
        assert!(door.calls().is_empty());
        assert!(!pass.audit()[0].passed_through);
    }

    #[test]
    fn unconfigured_door_does_not_invent_a_response() {
        let pass = PromptPass::unconfigured();
        let err = pass.submit(FIXTURE_PROMPT, FIXTURE_KEY, None).unwrap_err();
        assert!(matches!(err, PromptError::Door(_)));
        assert!(err.to_string().contains("not configured"));
        assert!(!err.to_string().contains(FIXTURE_KEY));
        let audit = pass.audit();
        assert_eq!(audit[0].prompt, FIXTURE_PROMPT);
        assert!(!audit[0].passed_through);
        assert_eq!(audit[0].outcome, "door_failed");
        assert!(!format!("{audit:?}").contains(FIXTURE_KEY));
    }

    #[test]
    fn supervisor_url_must_be_the_chat_door() {
        assert_eq!(
            completions_url("http://127.0.0.1:9").unwrap(),
            "http://127.0.0.1:9/v1/chat/completions"
        );
        assert_eq!(
            completions_url("https://chat.test.hyperme.sh/v1/chat/completions").unwrap(),
            "https://chat.test.hyperme.sh/v1/chat/completions"
        );
        assert!(
            completions_url("http://127.0.0.1:9/api/v1/hypermesh/renter/chat/completions").is_err()
        );
        assert!(completions_url("ftp://127.0.0.1:9").is_err());
        let err = completions_url("http://user:org_fixture_ok@127.0.0.1:9").unwrap_err();
        assert!(!err.contains(FIXTURE_KEY));
        assert!(
            completions_url("http://127.0.0.1:9/v1/chat/completions?k=org_fixture_ok").is_err()
        );
    }

    #[test]
    fn absent_supervisor_url_stays_unconfigured() {
        let door = open_supervisor_door(None).unwrap();
        let pass = PromptPass::new(door);
        let err = pass.submit(FIXTURE_PROMPT, FIXTURE_KEY, None).unwrap_err();
        assert!(err.to_string().contains("not configured"));
        assert!(!err.to_string().contains(FIXTURE_KEY));
        assert_eq!(pass.audit()[0].outcome, "door_failed");
    }

    #[test]
    fn supervisor_fixture_receives_the_key_and_the_auditor_hides_it() {
        let fixture = ChatFixture::spawn(
            200,
            r#"{"choices":[{"message":{"content":"echo org_fixture_ok"}}]}"#,
        );
        let door = SupervisorDoor::connect(&fixture.url).unwrap();
        let pass = PromptPass::new(Arc::new(door));

        let missing = pass.submit(FIXTURE_PROMPT, " ", None).unwrap_err();
        assert_eq!(missing, PromptError::MissingKey);
        let forbidden = pass
            .submit(FIXTURE_PROMPT, "hm_dev_fixture_tail", None)
            .unwrap_err();
        assert!(matches!(
            forbidden,
            PromptError::InvalidKey { prefix: "hm_dev_" }
        ));
        assert!(fixture.hits().is_empty());

        let response = pass
            .submit(FIXTURE_PROMPT, FIXTURE_KEY, None)
            .expect("forward");
        assert_eq!(response, "echo ***");
        assert!(!response.contains(FIXTURE_KEY));

        let hits = fixture.hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, CHAT_COMPLETIONS_PATH);
        assert_eq!(hits[0].api_key, FIXTURE_KEY);
        assert!(hits[0].lease.is_empty());
        assert!(hits[0].hypermesh_lease.is_empty());
        let posted: serde_json::Value = serde_json::from_str(&hits[0].body).unwrap();
        assert_eq!(posted["messages"][0]["content"], FIXTURE_PROMPT);
        assert!(posted.get("model").is_none());
        assert!(!hits[0].body.contains("llama-3.1-8b-q4"));
        assert!(!hits[0].body.contains(FIXTURE_KEY));

        pass.submit(FIXTURE_PROMPT, FIXTURE_KEY, Some("caller-picked"))
            .unwrap();
        let hits = fixture.hits();
        let posted: serde_json::Value = serde_json::from_str(&hits[1].body).unwrap();
        assert_eq!(posted["model"], "caller-picked");

        let audit = pass.audit();
        assert_eq!(audit.len(), 4);
        assert_eq!(audit[0].outcome, "rejected");
        assert_eq!(audit[1].outcome, "rejected");
        assert!(!audit[0].passed_through);
        assert!(!audit[1].passed_through);
        assert_eq!(audit[2].outcome, "forwarded");
        assert!(audit[2].passed_through);
        assert_eq!(audit[2].response.as_deref(), Some("echo ***"));
        assert_eq!(audit[2].prompt, FIXTURE_PROMPT);
        assert_eq!(audit[3].model.as_deref(), Some("caller-picked"));
        let rendered = format!("{audit:?}");
        assert!(!rendered.contains(FIXTURE_KEY));
        assert!(!rendered.contains("fixture_tail"));
        let json = serde_json::to_string(&audit).unwrap();
        assert!(!json.contains(FIXTURE_KEY));
        assert!(!json.contains("fixture_tail"));
    }

    #[test]
    fn supervisor_error_body_is_not_recorded() {
        let fixture = ChatFixture::spawn(403, r#"{"error":"org_fixture_ok"}"#);
        let door = SupervisorDoor::connect(&fixture.url).unwrap();
        let pass = PromptPass::new(Arc::new(door));
        let err = pass.submit(FIXTURE_PROMPT, FIXTURE_KEY, None).unwrap_err();
        assert_eq!(err.to_string(), "supervisor chat HTTP 403");
        assert!(!err.to_string().contains(FIXTURE_KEY));
        assert_eq!(fixture.hits()[0].api_key, FIXTURE_KEY);
        let audit = pass.audit();
        assert_eq!(audit[0].outcome, "door_failed");
        assert!(!audit[0].passed_through);
        assert_eq!(audit[0].reason.as_deref(), Some("supervisor chat HTTP 403"));
        assert!(!format!("{audit:?}").contains(FIXTURE_KEY));
    }
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct ChatHit {
    pub path: String,
    pub api_key: String,
    pub body: String,
    pub lease: String,
    pub hypermesh_lease: String,
}

#[cfg(test)]
pub(crate) struct ChatFixture {
    pub url: String,
    hits: Arc<Mutex<Vec<ChatHit>>>,
}

#[cfg(test)]
impl ChatFixture {
    pub(crate) fn spawn(status: u16, response_body: &str) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&hits);
        let response_body = response_body.to_string();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                let Some((path, headers, body)) = read_http(&mut stream) else {
                    continue;
                };
                recorded.lock().unwrap().push(ChatHit {
                    path,
                    api_key: headers.get("x-api-key").cloned().unwrap_or_default(),
                    lease: headers.get("x-lease-id").cloned().unwrap_or_default(),
                    hypermesh_lease: headers
                        .get("x-hypermesh-lease-id")
                        .cloned()
                        .unwrap_or_default(),
                    body,
                });
                let payload = response_body.as_bytes();
                let head = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                use std::io::Write;
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(payload);
            }
        });
        Self {
            url: format!("http://{addr}"),
            hits,
        }
    }

    pub(crate) fn hits(&self) -> Vec<ChatHit> {
        self.hits
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

#[cfg(test)]
fn read_http(
    stream: &mut std::net::TcpStream,
) -> Option<(String, std::collections::HashMap<String, String>, String)> {
    use std::io::Read;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    let header_end = loop {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > 64 * 1024 {
            return None;
        }
    };
    let header_text = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = header_text.split("\r\n");
    let path = lines
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string();
    let mut headers = std::collections::HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    let body = String::from_utf8_lossy(&body[..length.min(body.len())]).to_string();
    Some((path, headers, body))
}
