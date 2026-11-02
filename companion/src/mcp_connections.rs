//! Explicit lifecycle controls for existing attachments; never replay tool calls.
use crate::mcp_dialog::{args, choose, dialog};
use serde_json::Value;

pub fn run() -> Result<(), String> {
    let result = screen(
        crate::visor::mcp_status_request,
        crate::visor::mcp_connection_request,
        dialog,
    );
    if let Err(error) = &result {
        let _ = dialog(&args("error", "MCP connections", error), None);
    }
    result
}

fn screen(
    mut read: impl FnMut() -> Result<Value, String>,
    mut submit: impl FnMut(&str, &str, bool) -> Result<(), String>,
    mut ui: impl FnMut(&[String], Option<&str>) -> Result<Option<String>, String>,
) -> Result<(), String> {
    let snapshot = read()?;
    let session = &snapshot["session"];
    let Some(id) = session["id"].as_str() else {
        ui(
            &args(
                "info",
                "MCP connections",
                "No active session. Open a visor session first.",
            ),
            None,
        )?;
        return Ok(());
    };
    let attachments = session["mcp_attachments"]
        .as_array()
        .ok_or("Attachment details unavailable; update the daemon.")?;
    let candidates: Vec<_> = attachments
        .iter()
        .take(16)
        .filter_map(|server| {
            let name = server["server_id"].as_str()?;
            if name.is_empty()
                || name.len() > 512
                || name.trim() != name
                || server["routing_status"] == "refreshing"
            {
                return None;
            }
            Some(name)
        })
        .collect();
    if candidates.is_empty() {
        ui(&args("info", "MCP connections", "No selectable attachments. Refreshing attachments and targets unavailable from this daemon are excluded."), None)?;
        return Ok(());
    }
    let rows: Vec<_> = candidates
        .iter()
        .map(|name| name.chars().flat_map(char::escape_default).collect())
        .collect();
    let Some(index) = choose(&mut ui, "MCP connections", "Choose an existing attachment in the displayed session. Only the bounded status list is shown; refreshing or unavailable targets are excluded.", &rows)? else { return Ok(()); };
    let server = candidates[index];
    let Some(action) = choose(
        &mut ui,
        "MCP connection action",
        &rows[index],
        &[
            "Refresh tool catalog on the current connection".into(),
            "Reconnect using this session's saved configuration".into(),
        ],
    )?
    else {
        return Ok(());
    };
    let reconnect = action == 1;
    let detail = if reconnect {
        "Reconnect closes the old connection and may restart its local server process. It uses the session's saved configuration, not new profile edits."
    } else {
        "Refresh reads tools from the existing connection. It does not restart a disconnected server."
    };
    let message = format!("{} {}?\nSession: {}\n{}\nNo tool call is replayed. Closing this dialog cancels submission.", if reconnect { "Reconnect" } else { "Refresh" }, rows[index], id.chars().take(64).flat_map(char::escape_default).collect::<String>(), detail);
    if ui(
        &args("question", "Confirm MCP connection action", &message),
        None,
    )?
    .is_none()
    {
        return Ok(());
    }
    submit(id, server, reconnect)?;
    ui(&args("info", "MCP connections", "Connection action completed. Open MCP status to inspect the current catalog and routing state."), None)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn snapshot() -> Value {
        json!({"session":{"id":"12345678-1234-1234-1234-123456789abc","mcp_attachments":[
            {"name":"clipped display label","server_id":"exact-server","routing_status":"disconnected"},
            {"name":"unaddressable","routing_status":"ready"},
            {"server_id":"busy","routing_status":"refreshing"}
        ]}})
    }
    #[test]
    fn cancel_at_each_stage_never_submits() {
        for cancel in 0..3 {
            let mut step = 0;
            screen(
                || Ok(snapshot()),
                |_, _, _| panic!("submitted after cancel"),
                |_, _| {
                    let result = if step == cancel {
                        None
                    } else {
                        Some("0".into())
                    };
                    step += 1;
                    Ok(result)
                },
            )
            .unwrap();
        }
    }
    #[test]
    fn confirmed_actions_target_original_session_and_exact_id_once() {
        for reconnect in [false, true] {
            let mut step = 0;
            let mut calls = 0;
            screen(
                || Ok(snapshot()),
                |id, name, action| {
                    assert_eq!(id, "12345678-1234-1234-1234-123456789abc");
                    assert_eq!(name, "exact-server");
                    assert_eq!(action, reconnect);
                    calls += 1;
                    Ok(())
                },
                |args, _| {
                    if step == 0 {
                        assert!(!args.iter().any(|a| a == "busy" || a == "unaddressable"));
                    }
                    let answer = if step == 1 && reconnect { "1" } else { "0" };
                    step += 1;
                    Ok(Some(answer.into()))
                },
            )
            .unwrap();
            assert_eq!(calls, 1);
        }
    }
    #[test]
    fn uncertain_result_is_not_retried() {
        let mut calls = 0;
        assert!(screen(
            || Ok(snapshot()),
            |_, _, _| {
                calls += 1;
                Err("outcome unknown".into())
            },
            |_, _| Ok(Some("0".into()))
        )
        .is_err());
        assert_eq!(calls, 1);
    }
    #[test]
    fn missing_session_or_targets_never_submit() {
        for snapshot in [
            json!({"session":null}),
            json!({"session":{"id":"old","mcp_attachments":[{"name":"clipped"}]}}),
        ] {
            screen(
                || Ok(snapshot.clone()),
                |_, _, _| panic!("unexpected write"),
                |_, _| Ok(None),
            )
            .unwrap();
        }
    }
}
