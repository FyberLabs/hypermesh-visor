//! Local Linux daemon the supervisor starts so a caller can see and drive a desktop.
//! The session verbs are the stable surface. Version one implements them on Linux.

#[cfg(not(target_os = "linux"))]
compile_error!("hypermesh-visor version one is Linux only");

mod api;
mod bind;
mod desktop;
mod harness;
mod input;
mod prompt;
mod orchestrator;
mod session;
mod stream;
mod vault;

pub use api::{serve, AppState};
pub use bind::parse_listen;
pub use desktop::{current_euid, Desktop, FixtureDesktop, LinuxDesktop};
pub use hypermesh_session::{refresh_token, MemoryStore, SessionStore};
pub use prompt::{open_supervisor_door, DoorConfig, PromptDoor, DEFAULT_CATALOG_ID};

#[cfg(test)]
mod session_key {
    use super::*;
    use hypermesh_session::SessionStore;

    #[test]
    fn visor_reads_the_shared_refresh_token() {
        let store = MemoryStore::new();
        assert_eq!(refresh_token(&store).unwrap(), None);
        store.put_refresh_token("refresh-1").unwrap();
        assert_eq!(refresh_token(&store).unwrap().as_deref(), Some("refresh-1"));
        let shown = format!("{store:?}");
        assert!(!shown.contains("refresh-1"));
    }
}
