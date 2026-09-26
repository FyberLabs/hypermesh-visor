//! v0 prompt pass-through.
//!
//! A prompt and an API key come in. The orchestrator forwards that prompt,
//! unchanged, through one [`PromptDoor`]. The auditor records what was sent,
//! whether it passed through, and the outcome. The response comes back.
//!
//! The cloud agent that serves chat today is the renter supervisor in
//! hypermesh-host (`POST /v1/chat/completions`, header `X-Api-Key`). This
//! module is the visor seam in front of that door. The desktop daemon does
//! not configure a door, so it does not call that supervisor.

use std::fmt;
use std::sync::{Arc, Mutex};

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
}
