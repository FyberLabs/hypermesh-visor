use std::time::Instant;

use uuid::Uuid;

use crate::harness::Harness;
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

/// One open desktop session. Closing it drops the harness and the vault.
pub struct Session {
    id: Uuid,
    harness: Harness,
    vault: Vault,
    verb: Option<Verb>,
    touched: Instant,
    prompts: Vec<HeldPrompt>,
    files: Vec<HeldFile>,
    secret_handles: Vec<String>,
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
    pub fn create(id: Uuid, harness: Harness, vault: Vault) -> Self {
        Self {
            id,
            harness,
            vault,
            verb: None,
            touched: Instant::now(),
            prompts: Vec::new(),
            files: Vec::new(),
            secret_handles: Vec::new(),
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
        self.prompts.push(HeldPrompt { prompt, model });
        self.touched = Instant::now();
    }

    pub(crate) fn push_file(&mut self, name: String, bytes: Vec<u8>) {
        self.files.push(HeldFile { name, bytes });
        self.touched = Instant::now();
    }

    /// Puts a secret in the session vault and records its name as a handle.
    pub(crate) fn push_secret(&mut self, request: &SecretRequest) -> Result<String, VaultError> {
        let handle = self.vault.insert(request)?;
        self.secret_handles.push(handle.clone());
        self.touched = Instant::now();
        Ok(handle)
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
            .finish()
    }
}
