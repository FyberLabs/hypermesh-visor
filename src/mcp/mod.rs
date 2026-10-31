//! Local MCP tool attach for a desktop session (role A).
//!
//! This is not vault `SecretSource::Mcp` (role B). Config matches the CLI:
//! `mcp.json` + `mcp-profiles.json` under the Hypermesh config dir.

mod audit;
mod bindings;
mod config;
mod discovery;
mod http;
mod protocol;
mod subscription;
pub mod trust;
pub mod profiles;
pub mod servers;
mod trust_disk;
pub mod sources;
mod jsonc;
mod secret_fetch;
mod stdio;

pub use audit::{McpAuditRow, McpAuditor};
#[allow(unused_imports)] // `load_bindings` stays public; the poll path uses `BindingCache`.
pub use bindings::{
    companion_mode, explain_preferred, load_bindings, BindingCache, FocusedApp, MatchDecision,
};
pub use config::{config_dir, active_profile, load_profile_resolution};
pub use secret_fetch::fetch_secret_bytes;
pub use stdio::{attach_profile, attach_specs, McpBundle, ToolInfo};

use std::fmt;

#[derive(Debug)]
pub enum McpError {
    Message(String),
}

impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for McpError {}

impl McpError {
    pub fn message(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }
}

pub use protocol::Negotiated as NegotiatedProtocol;
pub use stdio::RoutingStatus;
