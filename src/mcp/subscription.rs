//! Coalesced catalog invalidation shared by transport readers and the API worker.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Default)]
pub struct Changes {
    pub enabled: AtomicBool,
    pub stopped: AtomicBool,
    pub failed: AtomicBool,
    generation: AtomicU64,
}
impl Changes {
    pub fn notify(&self, value: &serde_json::Value) {
        if self.enabled.load(Ordering::Acquire)
            && value["jsonrpc"] == "2.0"
            && value.get("id").is_none()
            && value["method"] == "notifications/tools/list_changed"
        {
            self.invalidate();
        }
    }
    pub fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}
