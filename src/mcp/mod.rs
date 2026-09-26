//! Local MCP tool attach for a desktop session (role A).
//!
//! This is not vault `SecretSource::Mcp` (role B). Config matches the CLI:
//! `mcp.json` + `mcp-profiles.json` under the Hypermesh config dir.

mod audit;
mod bindings;
mod config;
mod stdio;

pub use audit::{McpAuditRow, McpAuditor};
pub use bindings::{companion_mode, load_bindings, resolve_preferred, FocusedApp};
pub use config::config_dir;
pub use stdio::{attach_profile, McpBundle, ToolInfo};

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
