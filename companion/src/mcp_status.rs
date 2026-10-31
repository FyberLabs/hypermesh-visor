//! Read-only routing diagnostics. Server-provided tool text is never rendered.
use serde_json::Value;

pub fn run() -> Result<(), String> {
    screen(crate::visor::mcp_status_request, crate::mcp_dialog::dialog)
}

fn screen(
    mut request: impl FnMut() -> Result<Value, String>,
    mut ui: impl FnMut(&[String], Option<&str>) -> Result<Option<String>, String>,
) -> Result<(), String> {
    let report = request()
        .map(|v| render(&v))
        .unwrap_or_else(|e| format!("MCP status unavailable: {}", clean(&e)));
    crate::mcp_dialog::text(&mut ui, "MCP status", &report, false)?;
    Ok(())
}

fn clean(value: &str) -> String {
    value
        .chars()
        .take(256)
        .flat_map(char::escape_default)
        .collect()
}
fn label(value: &Value) -> String {
    clean(value.as_str().unwrap_or("unknown"))
}

fn render(snapshot: &Value) -> String {
    let session = &snapshot["session"];
    if session.is_null() {
        return "No active session. Open a visor session to inspect MCP routing.".into();
    }
    let decision = &session["mcp_match"];
    let reason = match decision["reason"].as_str() {
        Some("matched") => "A healthy server matches the focused app.",
        Some("focus_unavailable") => "Focused app identity is unavailable; using desktop fallback.",
        Some("no_bindings") => "No app bindings are configured; using desktop fallback.",
        Some("bindings_unavailable") => "App bindings could not be loaded; using desktop fallback.",
        Some("identity_mismatch") => "No binding matches the focused app; using desktop fallback.",
        Some("server_unavailable") => {
            "Matching servers cannot receive calls; using desktop fallback."
        }
        _ => "Routing explanation is unavailable.",
    };
    let mut out = format!(
        "{reason}\nBindings: {}\n",
        label(&decision["bindings_status"])
    );
    if let Some(index) = decision["binding_index"].as_u64() {
        out.push_str(&format!("Winning binding index: {index} (zero-based)\n"));
    }
    if session["mcp"].is_string() {
        out.push_str(&format!("Preferred server: {}\n", label(&session["mcp"])));
    }
    out.push_str("\nAttachments:\n");
    if let Some(servers) = session["mcp_attachments"].as_array() {
        if servers.is_empty() {
            out.push_str("No attached MCP servers.\n");
        }
        for server in servers.iter().take(32) {
            let state = match server["routing_status"].as_str() {
                Some("ready") => "Ready for direct tool calls",
                Some("task_required") => "Requires unsupported task execution",
                Some("no_tools") => "No tools advertised",
                Some("catalog_stale") => "Catalog stale; awaiting refresh",
                Some("disconnected") => "Disconnected",
                Some("refreshing") => "Refresh or reconnect in progress",
                _ => "Unknown attachment state",
            };
            out.push_str(&format!("{}: {state}\n", label(&server["name"])));
            let protocol = &server["protocol"];
            if protocol.is_object() {
                let flag = |key: &str| match protocol[key].as_bool() {
                    Some(true) => "yes",
                    Some(false) => "no",
                    None => "unknown",
                };
                out.push_str(&format!(
                    "  Last negotiation: {}; tools: {}; list changes: {}\n",
                    label(&protocol["version"]),
                    flag("tools"),
                    flag("list_changed")
                ));
            }
        }
        let omitted = session["mcp_attachments_omitted"]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(servers.len().saturating_sub(32) as u64);
        if omitted > 0 {
            out.push_str(&format!("{omitted} additional attachments omitted.\n"));
        }
    } else {
        out.push_str("Attachment details unavailable; the daemon may need updating.\n");
    }
    out.push_str("\nSnapshot only. Reopen to update. Protocol details describe the last successful negotiation; HTTP state is not a fresh liveness probe. No refresh, reconnect, or tool call is submitted.");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn report_explains_states_without_rendering_private_or_tool_data() {
        let mut servers = Vec::new();
        for state in [
            "ready",
            "task_required",
            "no_tools",
            "catalog_stale",
            "disconnected",
            "refreshing",
        ] {
            servers.push(json!({"name":"editor\nInjected","routing_status":state,"protocol":{"version":"2025-06-18","tools":true,"list_changed":false},"tools":[{"description":"SECRET"}],"url":"SECRET","headers":{"auth":"SECRET"}}));
        }
        let report = render(
            &json!({"session":{"mcp_match":{"reason":"server_unavailable","bindings_status":"ready"},"mcp_attachments":servers,"mcp_attachments_omitted":2}}),
        );
        for expected in [
            "desktop fallback",
            "Ready for direct",
            "unsupported task",
            "No tools",
            "Catalog stale",
            "Disconnected",
            "in progress",
            "tools: yes",
            "list changes: no",
            "2 additional",
            "editor\\nInjected",
        ] {
            assert!(report.contains(expected), "{expected}");
        }
        assert!(!report.contains("SECRET"));
    }
    #[test]
    fn cancel_reads_once_and_only_uses_plain_text_stdin() {
        let mut reads = 0;
        screen(
            || {
                reads += 1;
                Ok(json!({"session":null}))
            },
            |args, input| {
                assert!(!args
                    .iter()
                    .any(|a| a == "--editable" || a.contains("No active")));
                assert!(input.unwrap().contains("No active session"));
                Ok(None)
            },
        )
        .unwrap();
        assert_eq!(reads, 1);
    }
    #[test]
    fn report_bounds_untrusted_names_and_attachment_count() {
        let servers = vec![json!({"name":"x".repeat(10000),"routing_status":"ready"}); 40];
        let report = render(&json!({"session":{"mcp_attachments":servers}}));
        assert_eq!(report.matches("Ready for direct").count(), 32);
        assert!(report.contains("8 additional attachments omitted"));
        assert!(!report.contains(&"x".repeat(257)));
    }
    #[test]
    fn old_daemon_and_errors_have_explicit_reports() {
        assert!(render(&json!({"session":{"mcp_match":{"reason":"matched","bindings_status":"ready","binding_index":3},"mcp":"editor"}})).contains("daemon may need updating"));
        screen(
            || Err("offline".into()),
            |_, input| {
                assert!(input.unwrap().contains("unavailable: offline"));
                Ok(None)
            },
        )
        .unwrap();
    }
}
