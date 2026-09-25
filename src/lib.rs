//! Local Linux daemon the supervisor starts so a caller can see and drive a desktop.
//! The session verbs are the stable surface. Version one implements them on Linux.

#[cfg(not(target_os = "linux"))]
compile_error!("hypermesh-visor version one is Linux only");

mod api;
mod bind;
mod desktop;
mod harness;
mod input;
mod session;
mod vault;

pub use api::{serve, AppState};
pub use bind::parse_listen;
pub use desktop::{current_euid, Desktop, FixtureDesktop, LinuxDesktop};
