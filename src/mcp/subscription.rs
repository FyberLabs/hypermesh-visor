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
            && super::protocol::is_notification(value, "notifications/tools/list_changed")
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_valid_negotiated_notifications_invalidate_catalogs() {
        let changes = Changes::default();
        let notification = json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"});
        changes.notify(&notification);
        assert_eq!(changes.generation(), 0);
        changes.enabled.store(true, Ordering::Release);
        for patch in [json!({"id":null}), json!({"id":1}), json!({"result":{}}),
            json!({"error":null}), json!({"params":null}), json!({"params":[]}),
            json!({"params":"private"}), json!({"jsonrpc":"1.0"}), json!({"method":"other"})] {
            let mut invalid = notification.clone();
            invalid.as_object_mut().unwrap().extend(patch.as_object().unwrap().clone());
            changes.notify(&invalid);
            assert_eq!(changes.generation(), 0);
        }
        changes.notify(&notification);
        changes.notify(&json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed","params":{"_meta":{}}}));
        assert_eq!(changes.generation(), 2);
    }
}
