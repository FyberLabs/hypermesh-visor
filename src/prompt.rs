//! Prompt pass-through.
//!
//! A prompt and an API key come in. [`PromptPass::submit`] checks the key,
//! rejects an empty prompt, asks the orchestrator which catalog id and which
//! door to use, then forwards that prompt once. The auditor records the pass.
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

pub use crate::orchestrator::{DoorConfig, DEFAULT_CATALOG_ID};

/// Host, router, and site credentials are not renter API keys.
/// Same prefixes the CLI rejects.
pub const FORBIDDEN_RENTER_PREFIXES: &[&str] = &["hm_dev_", "hm_rtr_", "hm_site_"];

/// One prompt handed to a cloud-agent door.
///
/// `model` is set by [`PromptPass`] before `forward`. The door does not invent it.
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

/// One door the orchestrator can call. A mixture-of-experts wrapper is also a door.
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

pub(crate) fn door_authority(chat_base: &str) -> Result<String, String> {
    let endpoint = completions_url(chat_base)?;
    let rest = endpoint
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or("");
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() {
        return Err("supervisor url must be an http(s) URL".into());
    }
    Ok(authority.to_string())
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
    UnknownModel,
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
            Self::UnknownModel => write!(f, "unknown model"),
            Self::Door(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for PromptError {}

/// A secret or PII finding. The matched value is not a field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AuditFinding {
    pub kind: String,
    pub place: String,
}

/// The other expert's door and redacted text. The caller does not receive this.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExpertAside {
    pub door: String,
    pub response: String,
}

/// What the auditor keeps. The API key is not a field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AuditRecord {
    pub prompt: String,
    pub model: Option<String>,
    pub passed_through: bool,
    pub outcome: String,
    pub response: Option<String>,
    pub reason: Option<String>,
    /// URL authority of the door that was selected. Absent when the router did not run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub door: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<AuditFinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other_expert: Option<ExpertAside>,
}

struct Auditor {
    records: Vec<AuditRecord>,
    pending: Vec<AuditFinding>,
    needles: Vec<String>,
}

impl Default for Auditor {
    fn default() -> Self {
        Self {
            records: Vec::new(),
            pending: Vec::new(),
            needles: Vec::new(),
        }
    }
}

impl Auditor {
    fn record(&mut self, mut record: AuditRecord) {
        for finding in std::mem::take(&mut self.pending) {
            push_finding(&mut record.findings, finding);
        }
        self.records.push(record);
    }
}

/// Orchestrator plus auditor. One submit is one audit row.
pub struct PromptPass {
    orchestrator: crate::orchestrator::Orchestrator,
    auditor: Mutex<Auditor>,
    gate: Mutex<()>,
}

impl PromptPass {
    pub fn new(door: Arc<dyn PromptDoor>) -> Self {
        Self::from_orchestrator(crate::orchestrator::Orchestrator::single(
            DEFAULT_CATALOG_ID,
            door,
        ))
    }

    pub fn unconfigured() -> Self {
        Self::from_orchestrator(crate::orchestrator::Orchestrator::unconfigured(
            DEFAULT_CATALOG_ID,
        ))
    }

    pub(crate) fn from_orchestrator(orchestrator: crate::orchestrator::Orchestrator) -> Self {
        Self {
            orchestrator,
            auditor: Mutex::new(Auditor::default()),
            gate: Mutex::new(()),
        }
    }

    pub fn open(config: &DoorConfig) -> Result<Self, String> {
        Ok(Self::from_orchestrator(crate::orchestrator::connect(
            config,
        )?))
    }

    pub fn submit(
        &self,
        prompt: &str,
        api_key: &str,
        model: Option<&str>,
    ) -> Result<String, PromptError> {
        let _guard = self
            .gate
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.submit_inner(prompt, api_key, model)
    }

    #[cfg(test)]
    pub(crate) fn model_checks(&self) -> Vec<&'static str> {
        self.orchestrator.checks()
    }

    /// Parked stream prompts enter here. Secret values are needles for the
    /// audit row only; they are not copied onto that row.
    pub(crate) fn drive_parked(
        &self,
        prompts: &[(String, Option<String>)],
        file_texts: &[String],
        api_key: &str,
        secret_values: &[String],
    ) {
        let _guard = self
            .gate
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.arm_needles(secret_values);
        for (prompt, model) in prompts {
            self.flag_inbox(prompt);
            let _ = self.submit_inner(prompt, api_key, model.as_deref());
        }
        for text in file_texts {
            self.flag_inbox(text);
        }
        self.flush_flags();
        self.clear_needles();
    }

    fn submit_inner(
        &self,
        prompt: &str,
        api_key: &str,
        model: Option<&str>,
    ) -> Result<String, PromptError> {
        let sent = prompt.trim().to_string();
        let requested = normalize_model(model);
        let key = match validate_api_key(api_key) {
            Ok(key) => key.to_string(),
            Err(err) => {
                let key = api_key.trim();
                let mut row = rejected(&self.redact_text(&sent, key), None, &err);
                row.findings = findings_in(&sent, "prompt", key, &self.needles());
                self.record(row);
                return Err(err);
            }
        };
        if sent.is_empty() {
            let err = PromptError::EmptyPrompt;
            self.record(rejected(&sent, requested, &err));
            return Err(err);
        }
        let selected = match self.orchestrator.select(requested.clone()) {
            crate::orchestrator::Choice::Model(id) => id,
            crate::orchestrator::Choice::Unknown => {
                let err = PromptError::UnknownModel;
                self.record(rejected(&self.redact_text(&sent, &key), requested, &err));
                return Err(err);
            }
        };
        let call = ForwardedPrompt {
            prompt: sent.clone(),
            api_key: key.clone(),
            model: Some(selected.clone()),
        };
        // Model selection has already chosen `selected`. Routing and an expert
        // wrapper, when one is marked, happen inside this single forward.
        match self.orchestrator.dispatch(&selected, &call) {
            crate::orchestrator::Dispatch::Unmapped => {
                let message = "model is not routed".to_string();
                self.record(self.failed_row(&sent, &key, Some(selected), None, &message));
                Err(PromptError::Door(message))
            }
            crate::orchestrator::Dispatch::Finished {
                result,
                door,
                other_expert,
            } => match result {
                Ok(response) => {
                    let stored =
                        self.store_success(&sent, &key, selected, door, response, other_expert);
                    Ok(stored)
                }
                Err(message) => {
                    let message = self.redact_text(&message, &key);
                    self.record(self.failed_row(&sent, &key, Some(selected), door, &message));
                    Err(PromptError::Door(message))
                }
            },
        }
    }

    fn store_success(
        &self,
        sent: &str,
        key: &str,
        model: String,
        door: Option<String>,
        response: String,
        other_expert: Option<ExpertAside>,
    ) -> String {
        let mut findings = findings_in(sent, "prompt", key, &self.needles());
        findings.extend(findings_in(&response, "completion", key, &self.needles()));
        let response = self.redact_text(&response, key);
        let other_expert = other_expert.map(|aside| {
            findings.extend(findings_in(
                &aside.response,
                "completion",
                key,
                &self.needles(),
            ));
            ExpertAside {
                door: aside.door,
                response: self.redact_text(&aside.response, key),
            }
        });
        self.record(AuditRecord {
            prompt: self.redact_text(sent, key),
            model: Some(model),
            passed_through: true,
            outcome: "forwarded".into(),
            response: Some(response.clone()),
            reason: None,
            door,
            findings,
            other_expert,
        });
        response
    }

    fn failed_row(
        &self,
        sent: &str,
        key: &str,
        model: Option<String>,
        door: Option<String>,
        message: &str,
    ) -> AuditRecord {
        let findings = findings_in(sent, "prompt", key, &self.needles());
        AuditRecord {
            prompt: self.redact_text(sent, key),
            model,
            passed_through: false,
            outcome: "door_failed".into(),
            response: None,
            reason: Some(self.redact_text(message, key)),
            door,
            findings,
            other_expert: None,
        }
    }

    pub fn audit(&self) -> Vec<AuditRecord> {
        let auditor = self
            .auditor
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        auditor.records.clone()
    }

    fn flag_inbox(&self, text: &str) {
        let found = findings_in(text, "inbox", "", &self.needles());
        if found.is_empty() {
            return;
        }
        let mut auditor = self
            .auditor
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        auditor.pending.extend(found);
    }

    fn flush_flags(&self) {
        let mut auditor = self
            .auditor
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if auditor.pending.is_empty() {
            return;
        }
        let findings = std::mem::take(&mut auditor.pending);
        auditor.records.push(AuditRecord {
            prompt: String::new(),
            model: None,
            passed_through: false,
            outcome: "rejected".into(),
            response: None,
            reason: Some("flagged".into()),
            door: None,
            findings,
            other_expert: None,
        });
    }

    fn arm_needles(&self, secret_values: &[String]) {
        let mut auditor = self
            .auditor
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        auditor.needles = secret_values
            .iter()
            .filter(|value| value.chars().count() >= 8)
            .cloned()
            .collect();
    }

    fn clear_needles(&self) {
        let mut auditor = self
            .auditor
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        for needle in &mut auditor.needles {
            needle.clear();
        }
        auditor.needles.clear();
    }

    fn needles(&self) -> Vec<String> {
        self.auditor
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .needles
            .clone()
    }

    fn redact_text(&self, text: &str, key: &str) -> String {
        redact_sensitive(text, key, &self.needles())
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
        door: None,
        findings: Vec::new(),
        other_expert: None,
    }
}

fn push_finding(findings: &mut Vec<AuditFinding>, finding: AuditFinding) {
    if !findings.iter().any(|had| had == &finding) {
        findings.push(finding);
    }
}

fn findings_in(text: &str, place: &str, key: &str, needles: &[String]) -> Vec<AuditFinding> {
    let mut findings = Vec::new();
    if has_secret(text, key, needles) {
        findings.push(AuditFinding {
            kind: "secret".into(),
            place: place.into(),
        });
    }
    if has_pii(text) {
        findings.push(AuditFinding {
            kind: "pii".into(),
            place: place.into(),
        });
    }
    findings
}

fn has_secret(text: &str, key: &str, needles: &[String]) -> bool {
    if !key.is_empty() && text.contains(key) {
        return true;
    }
    if needles
        .iter()
        .any(|needle| !needle.is_empty() && text.contains(needle))
    {
        return true;
    }
    !secret_spans(text).is_empty()
}

fn redact_sensitive(text: &str, key: &str, needles: &[String]) -> String {
    let mut out = if key.is_empty() {
        text.to_string()
    } else {
        text.replace(key, "***")
    };
    for needle in needles {
        if needle.chars().count() >= 8 {
            out = out.replace(needle, "***");
        }
    }
    let spans = secret_spans(&out);
    if spans.is_empty() {
        return out;
    }
    let mut redacted = String::new();
    let mut last = 0;
    for (start, end) in spans {
        if start < last || end > out.len() {
            continue;
        }
        redacted.push_str(&out[last..start]);
        redacted.push_str("***");
        last = end;
    }
    redacted.push_str(&out[last..]);
    redacted
}

fn secret_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    for prefix in ["hm_dev_", "hm_rtr_", "hm_site_", "sk-"] {
        let mut rest = text;
        let mut base = 0;
        while let Some(index) = rest.find(prefix) {
            let start = base + index;
            let after = start + prefix.len();
            let tail = text[after..]
                .find(|ch: char| !ch.is_ascii_alphanumeric())
                .unwrap_or(text.len() - after);
            let end = after + tail;
            let token_len = end - after;
            let long_enough = if prefix == "sk-" {
                token_len >= 8
            } else {
                token_len >= 1
            };
            if long_enough && end > start {
                spans.push((start, end));
            }
            let next = after.max(start + 1);
            base = next;
            rest = &text[next..];
        }
    }
    for keyword in [
        "api_key", "apikey", "api-key", "secret", "password", "token",
    ] {
        let lower = text.to_ascii_lowercase();
        let mut rest = lower.as_str();
        let mut base = 0;
        while let Some(index) = rest.find(keyword) {
            let start = base + index;
            let after_key = start + keyword.len();
            let bytes = text.as_bytes();
            let mut cursor = after_key;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor < bytes.len() && (bytes[cursor] == b'=' || bytes[cursor] == b':') {
                cursor += 1;
                while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
                let value_start = cursor;
                while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
                if cursor - value_start >= 4 {
                    spans.push((value_start, cursor));
                }
            }
            let next = after_key.max(start + 1);
            base = next;
            rest = &lower[next..];
        }
    }
    spans.sort_unstable();
    spans.dedup();
    merge_spans(spans)
}

fn merge_spans(spans: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    let mut merged = Vec::new();
    for (start, end) in spans {
        if let Some((_, last_end)) = merged.last_mut() {
            if start <= *last_end {
                *last_end = (*last_end).max(end);
                continue;
            }
        }
        merged.push((start, end));
    }
    merged
}

fn has_pii(text: &str) -> bool {
    has_email(text) || has_digits_shape(text, &[3, 2, 4]) || has_digits_shape(text, &[3, 3, 4])
}

fn has_email(text: &str) -> bool {
    let bytes = text.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'@' || index == 0 {
            continue;
        }
        let mut start = index;
        while start > 0 && is_email_local(bytes[start - 1]) {
            start -= 1;
        }
        if start == index {
            continue;
        }
        let mut end = index + 1;
        let mut saw_dot = false;
        while end < bytes.len() && is_email_domain(bytes[end]) {
            if bytes[end] == b'.' {
                saw_dot = true;
            }
            end += 1;
        }
        if !saw_dot || end < index + 3 || bytes[end - 1] == b'.' {
            continue;
        }
        if let Some(dot) = text[index + 1..end].rfind('.') {
            let tld = &text[index + 1 + dot + 1..end];
            if tld.len() >= 2 && tld.bytes().all(|byte| byte.is_ascii_alphabetic()) {
                return true;
            }
        }
    }
    false
}

fn is_email_local(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn is_email_domain(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-'
}

fn has_digits_shape(text: &str, groups: &[usize]) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() < groups.iter().sum::<usize>() + groups.len() - 1 {
        return false;
    }
    for start in 0..bytes.len() {
        if start > 0 && bytes[start - 1].is_ascii_digit() {
            continue;
        }
        let mut cursor = start;
        let mut matched = true;
        for (index, group) in groups.iter().enumerate() {
            if index > 0 {
                if cursor >= bytes.len() || bytes[cursor] != b'-' {
                    matched = false;
                    break;
                }
                cursor += 1;
            }
            for _ in 0..*group {
                if cursor >= bytes.len() || !bytes[cursor].is_ascii_digit() {
                    matched = false;
                    break;
                }
                cursor += 1;
            }
            if !matched {
                break;
            }
        }
        if matched && (cursor == bytes.len() || !bytes[cursor].is_ascii_digit()) {
            return true;
        }
    }
    false
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

    pub(crate) fn fail(message: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            response: String::new(),
            fail: Some(message.into()),
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
        assert_eq!(calls[0].model.as_deref(), Some(DEFAULT_CATALOG_ID));
        assert!(!format!("{:?}", calls[0]).contains(FIXTURE_KEY));

        let audit = pass.audit();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].prompt, FIXTURE_PROMPT);
        assert_eq!(audit[0].model.as_deref(), Some(DEFAULT_CATALOG_ID));
        assert!(audit[0].passed_through);
        assert_eq!(audit[0].outcome, "forwarded");
        assert_eq!(audit[0].response.as_deref(), Some("baa"));
        assert!(audit[0].door.is_none());
        assert!(audit[0].other_expert.is_none());
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
        assert!(audit.iter().all(|row| row.model.is_none()));
        assert!(audit.iter().all(|row| row.door.is_none()));
        let rendered = format!("{audit:?}");
        assert!(!rendered.contains("fixture_tail"));
        assert!(!rendered.contains(FIXTURE_KEY));
    }

    #[test]
    fn omitted_model_uses_the_flagged_default_and_a_blank_model_does_too() {
        let door = RecordingDoor::ok("ok");
        let pass = PromptPass::from_orchestrator(crate::orchestrator::Orchestrator::single(
            "whisper-small",
            door.clone(),
        ));
        pass.submit(FIXTURE_PROMPT, FIXTURE_KEY, None).unwrap();
        pass.submit(FIXTURE_PROMPT, FIXTURE_KEY, Some("  "))
            .unwrap();
        let calls = door.calls();
        assert_eq!(calls[0].model.as_deref(), Some("whisper-small"));
        assert_eq!(calls[1].model.as_deref(), Some("whisper-small"));
        let audit = pass.audit();
        assert_eq!(audit[0].model.as_deref(), Some("whisper-small"));
        assert_eq!(audit[1].model.as_deref(), Some("whisper-small"));
        assert!(audit.iter().all(|row| row.outcome == "forwarded"));
    }

    #[test]
    fn unknown_model_is_rejected_after_catalog_and_live_net_checks() {
        let door = RecordingDoor::ok("ok");
        let pass = PromptPass::from_orchestrator(
            crate::orchestrator::Orchestrator::single(DEFAULT_CATALOG_ID, door.clone()).with_net(
                &[DEFAULT_CATALOG_ID, "whisper-small"],
                &[DEFAULT_CATALOG_ID, "live-only"],
            ),
        );

        let known = pass
            .submit(FIXTURE_PROMPT, FIXTURE_KEY, Some(DEFAULT_CATALOG_ID))
            .unwrap();
        assert_eq!(known, "ok");
        assert_eq!(pass.model_checks(), ["catalog", "live_net"]);

        let missing_net = pass
            .submit(FIXTURE_PROMPT, FIXTURE_KEY, Some("whisper-small"))
            .unwrap_err();
        assert_eq!(missing_net, PromptError::UnknownModel);
        assert_eq!(pass.model_checks(), ["catalog", "live_net"]);

        let missing_catalog = pass
            .submit(FIXTURE_PROMPT, FIXTURE_KEY, Some("live-only"))
            .unwrap_err();
        assert_eq!(missing_catalog, PromptError::UnknownModel);

        let unknown = pass
            .submit(FIXTURE_PROMPT, FIXTURE_KEY, Some("not-a-model"))
            .unwrap_err();
        assert_eq!(unknown, PromptError::UnknownModel);
        assert_eq!(unknown.to_string(), "unknown model");
        assert_eq!(pass.model_checks(), ["catalog", "live_net"]);

        assert_eq!(door.calls().len(), 1);
        assert_eq!(door.calls()[0].model.as_deref(), Some(DEFAULT_CATALOG_ID));
        let audit = pass.audit();
        assert_eq!(audit.len(), 4);
        assert_eq!(audit[0].outcome, "forwarded");
        assert_eq!(audit[0].model.as_deref(), Some(DEFAULT_CATALOG_ID));
        assert!(audit.iter().skip(1).all(|row| row.outcome == "rejected"));
        assert!(audit.iter().skip(1).all(|row| row.door.is_none()));
        assert_eq!(audit[3].model.as_deref(), Some("not-a-model"));
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
        assert!(pass.audit()[0].model.is_none());
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
        assert_eq!(audit[0].model.as_deref(), Some(DEFAULT_CATALOG_ID));
        assert!(!audit[0].passed_through);
        assert_eq!(audit[0].outcome, "door_failed");
        assert!(audit[0].door.is_none());
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
        assert_eq!(posted["model"], DEFAULT_CATALOG_ID);
        assert!(!hits[0].body.contains(FIXTURE_KEY));

        pass.submit(FIXTURE_PROMPT, FIXTURE_KEY, Some("whisper-small"))
            .unwrap();
        let unknown = pass
            .submit(FIXTURE_PROMPT, FIXTURE_KEY, Some("not-a-model"))
            .unwrap_err();
        assert_eq!(unknown, PromptError::UnknownModel);
        let hits = fixture.hits();
        assert_eq!(hits.len(), 2);
        let posted: serde_json::Value = serde_json::from_str(&hits[1].body).unwrap();
        assert_eq!(posted["model"], "whisper-small");

        let audit = pass.audit();
        assert_eq!(audit.len(), 5);
        assert_eq!(audit[0].outcome, "rejected");
        assert_eq!(audit[1].outcome, "rejected");
        assert!(audit[0].model.is_none());
        assert!(audit[1].model.is_none());
        assert!(!audit[0].passed_through);
        assert!(!audit[1].passed_through);
        assert_eq!(audit[2].outcome, "forwarded");
        assert!(audit[2].passed_through);
        assert_eq!(audit[2].response.as_deref(), Some("echo ***"));
        assert_eq!(audit[2].prompt, FIXTURE_PROMPT);
        assert_eq!(audit[2].model.as_deref(), Some(DEFAULT_CATALOG_ID));
        assert_eq!(audit[3].model.as_deref(), Some("whisper-small"));
        assert_eq!(audit[4].outcome, "rejected");
        assert_eq!(audit[4].model.as_deref(), Some("not-a-model"));
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
        assert_eq!(audit[0].model.as_deref(), Some(DEFAULT_CATALOG_ID));
        assert!(!format!("{audit:?}").contains(FIXTURE_KEY));
    }

    fn two_hosts() -> (
        Arc<RecordingDoor>,
        Arc<RecordingDoor>,
        crate::orchestrator::Orchestrator,
    ) {
        let first = RecordingDoor::fail("host-a down");
        let second = RecordingDoor::ok("from-b");
        let orchestrator = crate::orchestrator::Orchestrator::from_named(
            DEFAULT_CATALOG_ID,
            vec![
                ("host-a".into(), first.clone()),
                ("host-b".into(), second.clone()),
            ],
            vec![
                (DEFAULT_CATALOG_ID.into(), "host-a".into()),
                (DEFAULT_CATALOG_ID.into(), "host-b".into()),
            ],
            Vec::new(),
            Vec::new(),
        );
        (first, second, orchestrator)
    }

    #[test]
    fn unmapped_model_fails_closed_without_calling_a_door() {
        let first = RecordingDoor::ok("from-a");
        let second = RecordingDoor::ok("from-b");
        let pass = PromptPass::from_orchestrator(crate::orchestrator::Orchestrator::from_named(
            DEFAULT_CATALOG_ID,
            vec![
                ("host-a".into(), first.clone()),
                ("host-b".into(), second.clone()),
            ],
            vec![("whisper-small".into(), "host-a".into())],
            Vec::new(),
            Vec::new(),
        ));
        let err = pass.submit(FIXTURE_PROMPT, FIXTURE_KEY, None).unwrap_err();
        assert_eq!(err.to_string(), "model is not routed");
        assert!(first.calls().is_empty());
        assert!(second.calls().is_empty());
        let audit = pass.audit();
        assert_eq!(audit[0].outcome, "door_failed");
        assert_eq!(audit[0].model.as_deref(), Some(DEFAULT_CATALOG_ID));
        assert!(audit[0].door.is_none());
        assert!(!audit[0].passed_through);
    }

    #[test]
    fn a_failed_mapped_door_retries_only_a_host_the_rules_already_allow() {
        let (first, second, orchestrator) = two_hosts();
        let pass = PromptPass::from_orchestrator(orchestrator);
        let response = pass.submit(FIXTURE_PROMPT, FIXTURE_KEY, None).unwrap();
        assert_eq!(response, "from-b");
        assert_eq!(first.calls().len(), 1);
        assert_eq!(second.calls().len(), 1);
        assert_eq!(first.calls()[0].model.as_deref(), Some(DEFAULT_CATALOG_ID));
        assert_eq!(second.calls()[0].prompt, FIXTURE_PROMPT);
        let audit = pass.audit();
        assert_eq!(audit[0].outcome, "forwarded");
        assert_eq!(audit[0].door.as_deref(), Some("host-b"));
        assert_eq!(audit[0].response.as_deref(), Some("from-b"));

        let only = RecordingDoor::fail("only host down");
        let spare = RecordingDoor::ok("spare");
        let pass = PromptPass::from_orchestrator(crate::orchestrator::Orchestrator::from_named(
            DEFAULT_CATALOG_ID,
            vec![
                ("host-a".into(), only.clone()),
                ("host-b".into(), spare.clone()),
            ],
            vec![(DEFAULT_CATALOG_ID.into(), "host-a".into())],
            Vec::new(),
            Vec::new(),
        ));
        let err = pass.submit(FIXTURE_PROMPT, FIXTURE_KEY, None).unwrap_err();
        assert!(err.to_string().contains("only host down"));
        assert_eq!(only.calls().len(), 1);
        assert!(spare.calls().is_empty());
        assert_eq!(pass.audit()[0].door.as_deref(), Some("host-a"));
        assert_eq!(pass.audit()[0].outcome, "door_failed");
    }

    #[test]
    fn forbidden_keys_do_not_call_a_routed_door() {
        let (first, second, orchestrator) = two_hosts();
        let pass = PromptPass::from_orchestrator(orchestrator);
        for key in [
            "",
            "  ",
            "hm_dev_fixture_tail",
            "hm_rtr_fixture_tail",
            "hm_site_fixture_tail",
        ] {
            let _ = pass.submit(FIXTURE_PROMPT, key, None).unwrap_err();
        }
        assert!(first.calls().is_empty());
        assert!(second.calls().is_empty());
        assert!(pass.audit().iter().all(|row| row.door.is_none()));
        assert!(pass.audit().iter().all(|row| row.outcome == "rejected"));
        assert!(!format!("{:?}", pass.audit()).contains("fixture_tail"));
    }

    #[test]
    fn a_second_url_does_not_turn_a_blank_first_url_into_a_call() {
        let pass = PromptPass::open(&DoorConfig {
            default_model: DEFAULT_CATALOG_ID.into(),
            primary_url: None,
            second_url: Some("http://127.0.0.1:9".into()),
            routes: Vec::new(),
            experts: Vec::new(),
            answers: Vec::new(),
        })
        .unwrap();
        let err = pass.submit(FIXTURE_PROMPT, FIXTURE_KEY, None).unwrap_err();
        assert!(err.to_string().contains("not configured"));
        assert_eq!(pass.audit()[0].outcome, "door_failed");
        assert_eq!(pass.audit()[0].model.as_deref(), Some(DEFAULT_CATALOG_ID));
    }

    #[test]
    fn one_expert_failure_fails_the_prompt_and_hides_the_success() {
        let first = RecordingDoor::ok("alpha-should-not-return");
        let second = RecordingDoor::fail("expert down");
        let pass = PromptPass::from_orchestrator(crate::orchestrator::Orchestrator::from_named(
            DEFAULT_CATALOG_ID,
            vec![
                ("host-a".into(), first.clone()),
                ("host-b".into(), second.clone()),
            ],
            Vec::new(),
            vec![DEFAULT_CATALOG_ID.into()],
            Vec::new(),
        ));
        let err = pass.submit(FIXTURE_PROMPT, FIXTURE_KEY, None).unwrap_err();
        assert!(err.to_string().contains("expert down"));
        assert!(!err.to_string().contains("alpha-should-not-return"));
        assert_eq!(first.calls().len(), 1);
        assert_eq!(second.calls().len(), 1);
        let audit = pass.audit();
        assert_eq!(audit[0].outcome, "door_failed");
        assert!(audit[0].response.is_none());
        assert!(audit[0].other_expert.is_none());
        let rendered = format!("{audit:?}");
        assert!(!rendered.contains("alpha-should-not-return"));
    }

    #[test]
    fn the_caller_sees_the_first_expert_until_a_rule_names_another() {
        let first = RecordingDoor::ok("from-first");
        let second = RecordingDoor::ok(format!("from-second {FIXTURE_KEY}"));
        let pass = PromptPass::from_orchestrator(crate::orchestrator::Orchestrator::from_named(
            DEFAULT_CATALOG_ID,
            vec![
                ("host-a".into(), first.clone()),
                ("host-b".into(), second.clone()),
            ],
            Vec::new(),
            vec![DEFAULT_CATALOG_ID.into()],
            Vec::new(),
        ));
        let response = pass.submit(FIXTURE_PROMPT, FIXTURE_KEY, None).unwrap();
        assert_eq!(response, "from-first");
        assert!(!response.contains("from-second"));
        assert_eq!(first.calls().len(), 1);
        assert_eq!(second.calls().len(), 1);
        assert_eq!(first.calls()[0].prompt, second.calls()[0].prompt);
        assert_eq!(first.calls()[0].model, second.calls()[0].model);
        assert_eq!(first.calls()[0].api_key, FIXTURE_KEY);
        let audit = pass.audit();
        assert_eq!(audit[0].response.as_deref(), Some("from-first"));
        assert_eq!(audit[0].door.as_deref(), Some("host-a"));
        let other = audit[0].other_expert.as_ref().unwrap();
        assert_eq!(other.door, "host-b");
        assert_eq!(other.response, "from-second ***");
        assert!(!format!("{audit:?}").contains(FIXTURE_KEY));

        let named = PromptPass::from_orchestrator(crate::orchestrator::Orchestrator::from_named(
            DEFAULT_CATALOG_ID,
            vec![
                ("host-a".into(), first.clone()),
                ("host-b".into(), second.clone()),
            ],
            Vec::new(),
            vec![DEFAULT_CATALOG_ID.into()],
            vec![(DEFAULT_CATALOG_ID.into(), "host-b".into())],
        ));
        let response = named.submit(FIXTURE_PROMPT, FIXTURE_KEY, None).unwrap();
        assert_eq!(response, "from-second ***");
        assert_eq!(named.audit()[0].door.as_deref(), Some("host-b"));
        assert_eq!(
            named.audit()[0].other_expert.as_ref().unwrap().door,
            "host-a"
        );
        assert_eq!(
            named.audit()[0].other_expert.as_ref().unwrap().response,
            "from-first"
        );
    }

    #[test]
    fn an_expert_wrapper_refuses_an_empty_key_before_either_door() {
        let first = RecordingDoor::ok("from-first");
        let second = RecordingDoor::ok("from-second");
        let fan = crate::orchestrator::ExpertFan::pair(
            "host-a",
            first.clone(),
            "host-b",
            second.clone(),
            false,
        );
        let err = fan
            .forward(&ForwardedPrompt {
                prompt: FIXTURE_PROMPT.into(),
                api_key: "  ".into(),
                model: Some(DEFAULT_CATALOG_ID.into()),
            })
            .unwrap_err();
        assert!(err.contains("api key is required"));
        assert!(first.calls().is_empty());
        assert!(second.calls().is_empty());
    }

    #[test]
    fn a_forbidden_key_calls_neither_expert() {
        let first = RecordingDoor::ok("from-first");
        let second = RecordingDoor::ok("from-second");
        let pass = PromptPass::from_orchestrator(crate::orchestrator::Orchestrator::from_named(
            DEFAULT_CATALOG_ID,
            vec![
                ("host-a".into(), first.clone()),
                ("host-b".into(), second.clone()),
            ],
            Vec::new(),
            vec![DEFAULT_CATALOG_ID.into()],
            Vec::new(),
        ));
        let err = pass
            .submit(FIXTURE_PROMPT, "hm_site_fixture_tail", None)
            .unwrap_err();
        assert!(matches!(
            err,
            PromptError::InvalidKey { prefix: "hm_site_" }
        ));
        assert!(first.calls().is_empty());
        assert!(second.calls().is_empty());
        assert!(pass.audit()[0].other_expert.is_none());
        assert!(pass.audit()[0].door.is_none());
        assert!(!format!("{:?}", pass.audit()).contains("fixture_tail"));
    }

    #[test]
    fn the_auditor_flags_a_secret_and_pii_without_storing_the_secret() {
        let door = RecordingDoor::ok("reach ada@example.com token=sk-fixturesecretvalue");
        let pass = PromptPass::new(door.clone());
        let prompt = "email ada@example.com and token=sk-fixturesecretvalue";
        pass.submit(prompt, FIXTURE_KEY, None).unwrap();
        let audit = pass.audit();
        assert!(audit[0]
            .findings
            .iter()
            .any(|finding| finding.kind == "pii" && finding.place == "prompt"));
        assert!(audit[0]
            .findings
            .iter()
            .any(|finding| finding.kind == "secret" && finding.place == "prompt"));
        assert!(audit[0]
            .findings
            .iter()
            .any(|finding| finding.kind == "pii" && finding.place == "completion"));
        assert!(audit[0]
            .findings
            .iter()
            .any(|finding| finding.kind == "secret" && finding.place == "completion"));
        let json = serde_json::to_string(&audit).unwrap();
        assert!(!json.contains("sk-fixturesecretvalue"));
        assert!(json.contains("ada@example.com"));
        assert!(!json.contains("\"value\""));
        assert_eq!(door.calls()[0].prompt, prompt);
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
