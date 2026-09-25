use std::time::Instant;

use uuid::Uuid;

use crate::harness::Harness;
use crate::vault::Vault;

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
}

impl Session {
    pub fn create(id: Uuid, harness: Harness, vault: Vault) -> Self {
        Self {
            id,
            harness,
            vault,
            verb: None,
            touched: Instant::now(),
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
