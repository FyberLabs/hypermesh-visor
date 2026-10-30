//! Graphical review uses the same bounded, content-pinned trust API as the terminal.
use crate::mcp_dialog::{args, choose, dialog, text};
use serde_json::{json, Value};
pub fn run() -> Result<(), String> {
    let result = screen(crate::visor::trust_request, dialog);
    if let Err(e) = &result {
        let _ = dialog(&args("error", "MCP trust", e), None);
    }
    result
}
fn screen(
    mut request: impl FnMut(&str, Option<Value>) -> Result<Value, String>,
    mut ui: impl FnMut(&[String], Option<&str>) -> Result<Option<String>, String>,
) -> Result<(), String> {
    loop {
        let grants = request("/mcp/trust", None)?;
        let grants = grants.as_array().ok_or("Invalid trust response")?;
        let mut rows = vec!["Review a profile".into()];
        rows.extend(grants.iter().map(|g| {
            format!(
                "{} — {} (select to revoke)",
                g["profile"].as_str().unwrap_or("?"),
                g["status"].as_str().unwrap_or("?")
            )
        }));
        let Some(action)=choose(&mut ui,"MCP trust","Review source provenance or revoke future session access. Existing sessions keep their attachments.",&rows)? else{return Ok(())};
        if action > 0 {
            let profile = grants[action - 1]["profile"]
                .as_str()
                .ok_or("Invalid profile")?;
            if ui(
                &args(
                    "entry",
                    "Revoke MCP trust",
                    &format!("Type REVOKE to block future sessions for {profile}."),
                ),
                None,
            )?
            .as_deref()
                == Some("REVOKE")
            {
                request(
                    "/mcp/trust",
                    Some(json!({"action":"revoke","profile":profile})),
                )?;
            }
            continue;
        }
        let profiles = request("/mcp/profiles", None)?;
        let mut names: Vec<String> = profiles["profiles"]
            .as_array()
            .ok_or("Invalid profiles")?
            .iter()
            .filter_map(|p| p["name"].as_str().map(str::to_owned))
            .collect();
        if names.is_empty() {
            names.push(profiles["active"].as_str().unwrap_or("default").into());
        }
        let Some(index) = choose(
            &mut ui,
            "Review MCP profile",
            "Choose the visor profile whose server membership you want to review.",
            &names,
        )?
        else {
            continue;
        };
        let sources = request("/mcp/sources", None)?;
        let sources = sources.as_array().ok_or("Invalid sources")?;
        let available: Vec<_> = sources
            .iter()
            .filter(|s| s["status"] == "available_for_review")
            .collect();
        let mut rows = vec![
            "Daemon configuration (user plus explicitly selected project)".into(),
            "Choose an MCP configuration file".into(),
        ];
        rows.extend(available.iter().map(|s| {
            format!(
                "{} — {}",
                s["label"].as_str().unwrap_or("?"),
                s["path"].as_str().unwrap_or("?")
            )
        }));
        let Some(source) = choose(
            &mut ui,
            "MCP discovery source",
            "Selecting a source only previews it; nothing starts.",
            &rows,
        )?
        else {
            continue;
        };
        let mut selection = json!({"profile":names[index]});
        if source == 1 {
            let Some(path) = ui(
                &[
                    "--file-selection".into(),
                    "--title=Choose MCP configuration".into(),
                ],
                None,
            )?
            else {
                continue;
            };
            selection["project_path"] = json!(path);
        } else if source > 1 {
            selection["source_id"] = available[source - 2]["id"].clone();
        }
        let preview = request("/mcp/discovery", Some(selection))?;
        let id = preview["review_id"].as_str().ok_or("Invalid review ID")?;
        let decision: Result<bool, String> = (|| {
            let description = crate::mcp_trust::review_description(&preview)?;
            if text(
                &mut ui,
                "MCP trust — source provenance",
                &description,
                false,
            )?
            .is_none()
            {
                return Ok(false);
            }
            Ok(ui(&args("entry","Approve MCP trust","Review the complete source files privately. Argument/environment/header values and URL paths/credentials are hidden here. Type TRUST to approve this exact snapshot for future sessions. This permits commands and connections as you; it does not start them now."),None)?.as_deref()==Some("TRUST"))
        })();
        let approved = matches!(decision, Ok(true));
        let result = request(
            "/mcp/trust",
            Some(json!({"action":if approved{"approve"}else{"cancel"},"review_id":id})),
        );
        decision?;
        result?;
        if approved {
            ui(&args("info","MCP trust approved","Approval persists while source contents match after restart. Existing sessions keep their original attachments."),None)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn approval_requires_provenance_and_exact_confirmation_and_only_sends_id() {
        for confirmation in [None, Some("yes"), Some("TRUST")] {
            let mut answers = vec![Some("0"), Some("0"), Some("0"), Some(""), confirmation];
            if confirmation == Some("TRUST") {
                answers.push(Some(""));
            }
            answers.push(None);
            let mut answers = answers.into_iter();
            let mut decisions = Vec::new();
            screen(|path,body|{
                Ok(match path {
                    "/mcp/profiles"=>json!({"active":"a","profiles":[{"name":"a"}]}),
                    "/mcp/sources"=>json!([]),
                    "/mcp/discovery"=>json!({"review_id":"opaque","profile":"a","profiles_path":"/private/profiles","servers":[]}),
                    _=>{if let Some(body)=body{decisions.push(body);}json!([])}
                })
            },|_,_|Ok(answers.next().unwrap().map(str::to_owned))).unwrap();
            assert_eq!(
                decisions,
                vec![
                    json!({"action":if confirmation==Some("TRUST"){"approve"}else{"cancel"},"review_id":"opaque"})
                ]
            );
        }
    }
    #[test]
    fn dialog_failure_cancels_staged_review_and_never_approves() {
        let mut count = 0;
        let mut cancelled = false;
        let result = screen(
            |path, body| {
                Ok(match path {
                    "/mcp/profiles" => json!({"active":"a","profiles":[]}),
                    "/mcp/sources" => json!([]),
                    "/mcp/discovery" => json!({"review_id":"opaque","servers":[]}),
                    _ => {
                        if let Some(b) = body {
                            assert_eq!(b["action"], "cancel");
                            cancelled = true;
                        }
                        json!([])
                    }
                })
            },
            |_, _| {
                count += 1;
                if count == 4 {
                    Err("dialog failed".into())
                } else {
                    Ok(Some("0".into()))
                }
            },
        );
        assert!(result.is_err());
        assert!(cancelled);
    }
}
