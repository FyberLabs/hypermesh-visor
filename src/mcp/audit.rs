//! MCP tool-invoke audit. Records server, tool, and approve/deny only.
//! Never records env, arguments, or secret values.

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct McpAuditRow {
    pub server: String,
    pub tool: String,
    /// `approve` or `deny`.
    pub decision: String,
    /// `ok`, `denied`, or `failed`.
    pub outcome: String,
}

#[derive(Debug, Default)]
pub struct McpAuditor {
    rows: Vec<McpAuditRow>,
}

impl McpAuditor {
    pub fn record(&mut self, row: McpAuditRow) {
        self.rows.push(row);
    }

    pub fn rows(&self) -> &[McpAuditRow] {
        &self.rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_has_no_secret_fields() {
        let raw = serde_json::to_string(&McpAuditRow {
            server: "fixture".into(),
            tool: "ping".into(),
            decision: "approve".into(),
            outcome: "ok".into(),
        })
        .unwrap();
        assert!(raw.contains("fixture"));
        assert!(raw.contains("ping"));
        assert!(!raw.contains("env"));
        assert!(!raw.contains("arguments"));
        assert!(!raw.contains("secret"));
    }
}
