//! Graphical user server editor. Private fields are explicitly replaced, never fetched.
use crate::mcp_dialog::{args, choose, dialog, text};
use serde_json::{json, Value};
pub fn run() -> Result<(), String> {
    let result = screen(crate::visor::trust_request, dialog);
    if let Err(e) = &result {
        let _ = dialog(&args("error", "MCP servers", e), None);
    }
    result
}
fn screen(
    mut request: impl FnMut(&str, Option<Value>) -> Result<Value, String>,
    mut ui: impl FnMut(&[String], Option<&str>) -> Result<Option<String>, String>,
) -> Result<(), String> {
    loop {
        let snapshot = request("/mcp/servers", None)?;
        let servers = snapshot["servers"]
            .as_array()
            .ok_or("Invalid server response")?;
        let mut rows = vec!["Create a server".into()];
        rows.extend(servers.iter().map(|s| {
            format!(
                "{} — {} ({})",
                s["name"].as_str().unwrap_or("?"),
                s["transport"].as_str().unwrap_or("?"),
                if s["enabled"] == true {
                    "enabled"
                } else {
                    "disabled"
                }
            )
        }));
        let Some(index)=choose(&mut ui,"MCP servers",
            "Edit user server definitions. Private values stay hidden unless you replace them. Saving blocks current profile approvals; review them again in MCP trust.",&rows)? else{return Ok(())};
        let create = index == 0;
        let (name, server) = if create {
            let Some(name) = ui(&args("entry", "New MCP server", "Server ID"), None)? else {
                continue;
            };
            (name, json!({}))
        } else {
            (
                servers[index - 1]["name"]
                    .as_str()
                    .ok_or("Invalid server name")?
                    .to_string(),
                servers[index - 1].clone(),
            )
        };
        let action = if create {
            0
        } else {
            let Some(action)=choose(&mut ui,"Server action",&format!("{name}\nArguments: {}. Environment names: {}. Header names: {}. Endpoint origin: {}.",
                server["argument_count"],server["environment_names"],server["header_names"],server["endpoint"]),&[
                "Edit transport and command / replace endpoint".into(),
                "Enable or disable".into(),
                "Replace argument list".into(),
                "Replace environment map".into(),
                "Replace header map".into(),
                "Edit working directory".into(),
                "Delete server".into(),
            ])? else{continue};
            action
        };
        let mut fields = json!({});
        match action {
            0 => {
                let Some(mode) = choose(
                    &mut ui,
                    "Server transport",
                    "Choose transport. Other fields and private values are preserved.",
                    &["stdio".into(), "http".into(), "sse".into()],
                )?
                else {
                    continue;
                };
                fields["type"] = json!(["stdio", "http", "sse"][mode]);
                let (field, title, initial) = if mode == 0 {
                    (
                        "command",
                        "Executable — one command, no shell arguments",
                        server["command"].as_str().unwrap_or(""),
                    )
                } else {
                    ("url", "Full HTTP endpoint — replaces the hidden URL", "")
                };
                let Some(value) = text(&mut ui, title, initial, true)? else {
                    continue;
                };
                if value.trim().is_empty() {
                    return Err("Command or endpoint cannot be empty".into());
                }
                fields[field] = json!(value.trim());
                if create {
                    fields["enabled"] = json!(true);
                }
            }
            1 => {
                let Some(mode) = choose(
                    &mut ui,
                    "Server enabled",
                    "Choose whether the server is eligible for attachment after trust review.",
                    &["Enabled".into(), "Disabled".into()],
                )?
                else {
                    continue;
                };
                fields["enabled"] = json!(mode == 0);
            }
            2 | 3 | 4 => {
                let (key, initial) = match action {
                    2 => ("args", "[]"),
                    3 => ("env", "{}"),
                    _ => ("headers", "{}"),
                };
                let Some(raw) = text(
                    &mut ui,
                    &format!("Replace {key} as JSON — all old values will be removed"),
                    initial,
                    true,
                )?
                else {
                    continue;
                };
                let value: Value = serde_json::from_str(&raw)
                    .map_err(|_| "Invalid replacement JSON; nothing saved")?;
                let valid = if key == "args" {
                    value
                        .as_array()
                        .is_some_and(|v| v.iter().all(Value::is_string))
                } else {
                    value
                        .as_object()
                        .is_some_and(|v| v.values().all(Value::is_string))
                };
                if !valid {
                    return Err("Use a JSON array of strings for arguments or a JSON object of string values for environment/headers".into());
                }
                fields[key] = value;
            }
            5 => {
                let Some(cwd) = text(
                    &mut ui,
                    "Working directory — blank removes the override",
                    server["cwd"].as_str().unwrap_or(""),
                    true,
                )?
                else {
                    continue;
                };
                fields["cwd"] = if cwd.trim().is_empty() {
                    Value::Null
                } else {
                    json!(cwd.trim())
                };
            }
            _ => {}
        }
        let body = if action == 6 {
            json!({"action":"delete","revision":snapshot["revision"],"name":name})
        } else {
            json!({"action":"save","revision":snapshot["revision"],"name":name,"create":create,"fields":fields})
        };
        // Never put private replacement values in argv, notifications, or errors.
        let keys = fields
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if ui(&args("question","Confirm MCP server change",&format!("{} server {name}?\nChanged fields: {keys}\nUntouched fields remain unchanged. Current profile approvals will be blocked. Use MCP profiles for membership, then MCP trust to approve. Existing sessions retain their attachments.",
            if action==6{"Delete"}else{"Save"})),None)?.is_none(){continue;}
        request("/mcp/servers", Some(body))?;
        ui(&args("info","MCP server saved","Open MCP trust from the companion menu to review the profiles you want to use. No server was started by this editor."),None)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> Value {
        json!({"revision":"old","servers":[{"name":"a","transport":"stdio","enabled":true,"command":"never-run","argument_count":1,"environment_names":["TOKEN"],"header_names":[]}]})
    }
    #[test]
    fn private_replacements_never_enter_dialog_arguments_and_require_confirmation() {
        let mut responses = vec![
            Some("1"),
            Some("3"),
            Some(r#"{"TOKEN":"new-private-value"}"#),
            Some(""),
            Some(""),
            None,
        ]
        .into_iter();
        let mut saved = None;
        screen(
            |_, body| {
                if body.is_some() {
                    saved = body;
                }
                Ok(snapshot())
            },
            |args, input| {
                assert!(!args.join(" ").contains("new-private-value"));
                assert!(!input.unwrap_or("").contains("new-private-value"));
                Ok(responses.next().unwrap().map(str::to_owned))
            },
        )
        .unwrap();
        assert_eq!(
            saved.unwrap()["fields"]["env"]["TOKEN"],
            "new-private-value"
        );
    }
    #[test]
    fn cancel_or_invalid_replacement_never_writes() {
        for answer in [None, Some("not JSON")] {
            let mut responses = vec![Some("1"), Some("2"), answer, None].into_iter();
            let _ = screen(
                |_, body| {
                    assert!(body.is_none());
                    Ok(snapshot())
                },
                |_, _| Ok(responses.next().unwrap().map(str::to_owned)),
            );
        }
    }
    #[test]
    fn cancelling_final_confirmation_preserves_existing_values() {
        let mut responses = vec![Some("1"), Some("3"), Some("{}"), None, None].into_iter();
        screen(
            |_, body| {
                assert!(body.is_none());
                Ok(snapshot())
            },
            |_, _| Ok(responses.next().unwrap().map(str::to_owned)),
        )
        .unwrap();
    }
}
