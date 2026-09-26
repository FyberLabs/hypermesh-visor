use std::time::Instant;

use uuid::Uuid;

use crate::harness::Harness;
use crate::mcp::McpBundle;
use crate::vault::{SecretRequest, Vault, VaultError};

/// The verb the session is performing. The desktop companion reads this.
/// It is not a copy of the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    View,
    Watch,
    Listen,
    Mouse,
    Type,
}

impl Verb {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Watch => "watch",
            Self::Listen => "listen",
            Self::Mouse => "mouse",
            Self::Type => "type",
        }
    }
}

/// One open desktop session. Closing it drops the harness, vault, and MCP children.
pub struct Session {
    id: Uuid,
    harness: Harness,
    vault: Vault,
    mcp: McpBundle,
    verb: Option<Verb>,
    touched: Instant,
    prompts: Vec<HeldPrompt>,
    files: Vec<HeldFile>,
    secret_handles: Vec<String>,
    activity: Vec<Activity>,
    last_focus: Option<String>,
}

/// One line of terminal or IDE activity. `text` is already redacted.
const ACTIVITY_CAP: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Activity {
    pub kind: &'static str,
    pub text: String,
}

/// A prompt delivered on the open stream. `model` is set only when the caller sent one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeldPrompt {
    pub prompt: String,
    pub model: Option<String>,
}

/// A file delivered on the open stream. The name is a handle, not a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeldFile {
    pub name: String,
    pub bytes: Vec<u8>,
}

impl Session {
    pub fn create(id: Uuid, harness: Harness, vault: Vault, mcp: McpBundle) -> Self {
        Self {
            id,
            harness,
            vault,
            mcp,
            verb: None,
            touched: Instant::now(),
            prompts: Vec::new(),
            files: Vec::new(),
            secret_handles: Vec::new(),
            activity: Vec::new(),
            last_focus: None,
        }
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn harness(&self) -> &Harness {
        &self.harness
    }

    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    pub fn mcp(&self) -> &McpBundle {
        &self.mcp
    }

    pub fn mcp_mut(&mut self) -> &mut McpBundle {
        &mut self.mcp
    }

    pub fn purpose(&self) -> &str {
        &self.harness.purpose
    }

    pub fn verb(&self) -> Option<Verb> {
        self.verb
    }

    pub fn touched(&self) -> Instant {
        self.touched
    }

    pub fn mark_verb(&mut self, verb: Verb) {
        self.verb = Some(verb);
        self.touched = Instant::now();
    }

    pub(crate) fn held_prompts(&self) -> &[HeldPrompt] {
        &self.prompts
    }

    pub(crate) fn held_files(&self) -> &[HeldFile] {
        &self.files
    }

    pub(crate) fn secret_handles(&self) -> &[String] {
        &self.secret_handles
    }

    pub(crate) fn push_prompt(&mut self, prompt: String, model: Option<String>) {
        self.note("prompt", &prompt);
        self.prompts.push(HeldPrompt { prompt, model });
    }

    pub(crate) fn push_file(&mut self, name: String, bytes: Vec<u8>) {
        self.note("file", &name);
        self.files.push(HeldFile { name, bytes });
    }

    /// Puts a secret in the session vault and records its name as a handle.
    pub(crate) fn push_secret(&mut self, request: &SecretRequest) -> Result<String, VaultError> {
        let handle = self.vault.insert(request)?;
        self.secret_handles.push(handle.clone());
        self.note("secret", &handle);
        Ok(handle)
    }

    pub(crate) fn activity(&self) -> &[Activity] {
        &self.activity
    }

    pub(crate) fn redact(&self, text: &str) -> String {
        self.vault.redact(text)
    }

    /// Records terminal or IDE activity. Secret values are replaced first.
    /// File bytes are not accepted here.
    pub(crate) fn note(&mut self, kind: &'static str, text: &str) {
        let redacted = self.redact(text);
        let mut clean = String::new();
        for ch in redacted.chars() {
            if clean.chars().count() >= 280 {
                break;
            }
            if ch.is_control() {
                clean.push(' ');
            } else {
                clean.push(ch);
            }
        }
        let clean = clean.trim().to_string();
        if clean.is_empty() {
            return;
        }
        if self.activity.len() >= ACTIVITY_CAP {
            self.activity.remove(0);
        }
        self.activity.push(Activity { kind, text: clean });
        self.touched = Instant::now();
    }

    /// Focused terminal or IDE. The same label is not repeated.
    pub(crate) fn note_focus(&mut self, label: &str) {
        let label = label.trim();
        if label.is_empty() || self.last_focus.as_deref() == Some(label) {
            return;
        }
        self.last_focus = Some(label.to_string());
        self.note("focus", label);
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id())
            .field("purpose", &self.harness().purpose)
            .field("verb", &self.verb.map(Verb::as_str))
            .field("agent", &self.harness().agent.name)
            .field("vault", self.vault())
            .field("mcp_profile", &self.mcp.profile)
            .field("mcp_servers", &self.mcp.server_names())
            .finish()
    }
}
