use uuid::Uuid;

use crate::harness::Harness;
use crate::vault::Vault;

/// One open desktop session. Closing it drops the harness and the vault.
pub struct Session {
    id: Uuid,
    harness: Harness,
    vault: Vault,
}

impl Session {
    pub fn create(id: Uuid, harness: Harness, vault: Vault) -> Self {
        Self { id, harness, vault }
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
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id())
            .field("purpose", &self.harness().purpose)
            .field("agent", &self.harness().agent.name)
            .field("vault", self.vault())
            .finish()
    }
}
