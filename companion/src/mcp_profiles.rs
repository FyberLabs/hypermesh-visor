//! Native GTK dialogs via Zenity; no shell, browser server, or secret editing.
use serde_json::{json, Value};
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn dialog(args: &[String], input: Option<&str>) -> Result<Option<String>, String> {
    let mut child = Command::new("zenity")
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|_| "Cannot open MCP profiles. Install zenity and run in your desktop session.")?;
    if let Some(input) = input {
        if child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .is_err()
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Profile dialog input failed".into());
        }
    }
    let result = child
        .wait_with_output()
        .map_err(|_| "Profile dialog failed")?;
    match result.status.code() {
        Some(0) => String::from_utf8(result.stdout)
            .map(|s| Some(s.trim_end_matches('\n').into()))
            .map_err(|_| "Invalid dialog response".into()),
        Some(1) => Ok(None),
        _ => Err("Profile dialog failed; no save confirmed".into()),
    }
}
pub fn run() -> Result<(), String> {
    let result = screen(crate::visor::trust_request, dialog);
    if let Err(e) = &result {
        let _ = dialog(
            &args(
                "error",
                "MCP profiles",
                &format!("{e}\nNo further changes were submitted."),
            ),
            None,
        );
    }
    result
}
fn args(kind: &str, title: &str, text: &str) -> Vec<String> {
    vec![
        format!("--{kind}"),
        format!("--title={title}"),
        "--no-markup".into(),
        format!("--text={text}"),
        "--width=650".into(),
    ]
}
fn choose(
    ui: &mut impl FnMut(&[String], Option<&str>) -> Result<Option<String>, String>,
    title: &str,
    text: &str,
    rows: &[String],
) -> Result<Option<usize>, String> {
    let mut a = args("list", title, text);
    a.extend([
        "--column=ID".into(),
        "--column=Choice".into(),
        "--hide-column=1".into(),
        "--print-column=1".into(),
        "--".into(),
    ]);
    for (i, row) in rows.iter().enumerate() {
        a.push(i.to_string());
        a.push(row.clone());
    }
    let Some(answer) = ui(&a, None)? else {
        return Ok(None);
    };
    let i = answer
        .parse::<usize>()
        .map_err(|_| "Invalid profile selection")?;
    if i >= rows.len() {
        return Err("Invalid profile selection".into());
    }
    Ok(Some(i))
}
fn screen(
    mut request: impl FnMut(&str, Option<Value>) -> Result<Value, String>,
    mut ui: impl FnMut(&[String], Option<&str>) -> Result<Option<String>, String>,
) -> Result<(), String> {
    loop {
        let snapshot = request("/mcp/profiles", None)?;
        let profiles = snapshot["profiles"]
            .as_array()
            .ok_or("Invalid profiles response")?;
        let mut rows = vec!["Create a profile".into()];
        rows.extend(profiles.iter().map(|p| {
            format!(
                "{}{} — {} server IDs{}",
                if p["name"] == snapshot["active"] {
                    "Active: "
                } else {
                    ""
                },
                p["name"].as_str().unwrap_or("?"),
                p["servers"].as_array().map_or(0, Vec::len),
                if p["gateway"] == true {
                    " (Docker gateway)"
                } else {
                    ""
                }
            )
        }));
        let Some(index)=choose(&mut ui,"MCP profiles",
            "Choose a profile to edit or create one. Saving membership or gateway changes requires a new MCP trust review. Existing sessions keep their attachments.",&rows)? else{return Ok(())};
        let create = index == 0;
        let (name, existing) = if create {
            let Some(name) = ui(&args("entry", "New MCP profile", "Profile name"), None)? else {
                continue;
            };
            (name, json!({"servers":[],"gateway":false}))
        } else {
            let p = profiles[index - 1].clone();
            (p["name"].as_str().ok_or("Invalid profile name")?.into(), p)
        };
        let action = if create {
            0
        } else {
            let Some(a) = choose(
                &mut ui,
                "Profile action",
                &name,
                &[
                    "Edit server membership and gateway mode".into(),
                    "Make active for new sessions".into(),
                    "Delete profile".into(),
                ],
            )?
            else {
                continue;
            };
            a
        };
        let edit = match action {
            0 => {
                let Some(mode) = choose(
                    &mut ui,
                    "MCP profile mode",
                    if existing["gateway"] == true {
                        "Current mode: Docker gateway"
                    } else {
                        "Current mode: individual servers"
                    },
                    &["Individual servers".into(), "Docker gateway only".into()],
                )?
                else {
                    continue;
                };
                let servers = if mode == 0 {
                    let initial = existing["servers"]
                        .as_array()
                        .ok_or("Invalid server membership")?
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n");
                    let a = vec![
                        "--text-info".into(),
                        "--editable".into(),
                        "--title=Server IDs — one per line".into(),
                        "--width=650".into(),
                        "--height=400".into(),
                    ];
                    let Some(text) = ui(&a, Some(&initial))? else {
                        continue;
                    };
                    text.lines()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                } else {
                    existing["servers"]
                        .as_array()
                        .ok_or("Invalid server membership")?
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                };
                json!({"action":"save","revision":snapshot["revision"],"name":name,"servers":servers,"gateway":mode==1,"create":create})
            }
            1 => json!({"action":"activate","revision":snapshot["revision"],"name":name}),
            _ => json!({"action":"delete","revision":snapshot["revision"],"name":name}),
        };
        let message=match action {
            0=>format!("Save profile {name}?\nServer membership: {}\nGateway only: {}\nExisting private overrides are preserved and hidden. Use MCP trust to review before opening new sessions.",edit["servers"],edit["gateway"]),
            1=>format!("Make {name} active for future sessions? This does not approve a blocked profile."),
            _=>format!("Delete profile {name}? Its trust remains revoked. Existing sessions stay open."),
        };
        if ui(
            &args("question", "Confirm MCP profile change", &message),
            None,
        )?
        .is_none()
        {
            continue;
        }
        request("/mcp/profiles", Some(edit))?;
        ui(
            &args(
                "info",
                "MCP profile saved",
                if action == 1 {
                    "Active profile saved. Existing trust decisions still apply."
                } else {
                    "Profile saved. Open MCP trust in the companion menu to review changed profiles before using them."
                },
            ),
            None,
        )?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> Value {
        json!({"revision":"original","active":"default","profiles":[{"name":"default","servers":["one"],"gateway":false,"has_overrides":true}]})
    }
    #[test]
    fn create_requires_all_dialogs_and_confirmation_before_write() {
        let answers = vec![
            Some("0"),
            Some("new"),
            Some("0"),
            Some("one\ntwo"),
            Some(""),
            Some(""),
            None,
        ];
        let mut answers = answers.into_iter();
        let mut saved = Vec::new();
        screen(
            |_, body| {
                if let Some(v) = body {
                    saved.push(v);
                }
                Ok(snapshot())
            },
            |_, _| Ok(answers.next().unwrap().map(str::to_owned)),
        )
        .unwrap();
        assert_eq!(
            saved,
            vec![
                json!({"action":"save","revision":"original","name":"new","servers":["one","two"],"gateway":false,"create":true})
            ]
        );
    }
    #[test]
    fn cancelling_any_create_step_never_writes() {
        for stop in 0..5 {
            let mut answers = vec![Some("0"), Some("new"), Some("0"), Some("one"), Some("")];
            answers[stop] = None;
            answers.truncate(stop + 1);
            answers.push(None);
            let mut answers = answers.into_iter();
            screen(
                |_, body| {
                    assert!(body.is_none());
                    Ok(snapshot())
                },
                |_, _| Ok(answers.next().unwrap().map(str::to_owned)),
            )
            .unwrap();
        }
    }
    #[test]
    fn edit_preserves_revision_and_activation_is_a_separate_action() {
        let mut answers = vec![Some("1"), Some("1"), Some(""), Some(""), None].into_iter();
        let mut saved = None;
        screen(
            |_, body| {
                if body.is_some() {
                    saved = body;
                }
                Ok(snapshot())
            },
            |a, _| {
                if a.iter().any(|a| a == "--list") {
                    assert!(a.iter().any(|a| a == "--"));
                }
                Ok(answers.next().unwrap().map(str::to_owned))
            },
        )
        .unwrap();
        assert_eq!(
            saved.unwrap(),
            json!({"action":"activate","revision":"original","name":"default"})
        );
    }
}
