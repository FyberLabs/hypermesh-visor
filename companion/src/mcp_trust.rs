//! Human-driven terminal screen, launched from the companion's MCP trust menu.
use serde_json::{json, Value};
use std::io::{self, BufRead, IsTerminal, Read, Write};

pub fn run() -> Result<(), String> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("MCP trust requires an interactive terminal".into());
    }
    let result = screen(
        &mut io::stdin().lock(),
        &mut io::stdout().lock(),
        crate::visor::trust_request,
    );
    if let Err(error) = &result {
        eprintln!("{error}\nPress Enter to close this screen.");
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);
    }
    result
}

fn text(value: &Value) -> String {
    // Configuration is untrusted display data; never emit terminal escapes.
    value
        .as_str()
        .map(|s| s.chars().flat_map(char::escape_default).collect())
        .unwrap_or_else(|| {
            value
                .to_string()
                .chars()
                .flat_map(char::escape_default)
                .collect()
        })
}
fn input(
    reader: &mut impl BufRead,
    out: &mut impl Write,
    label: &str,
) -> Result<Option<String>, String> {
    write!(out, "{label}")
        .and_then(|_| out.flush())
        .map_err(|_| "Terminal output failed")?;
    let mut line = String::new();
    let n = (&mut *reader)
        .take(4098)
        .read_line(&mut line)
        .map_err(|_| "Terminal input failed")?;
    if n == 0 {
        return Ok(None);
    }
    if n > 4097 || !line.ends_with('\n') {
        return Err("Input too long or incomplete".into());
    }
    Ok(Some(line.trim().into()))
}
fn screen(
    reader: &mut impl BufRead,
    out: &mut impl Write,
    mut request: impl FnMut(&str, Option<Value>) -> Result<Value, String>,
) -> Result<(), String> {
    writeln!(out,"MCP discovery and trust\nReview sources without starting servers. Approvals and revocations persist across visor restarts; changed source files require review.\nExisting sessions keep their attachments; close them to stop a previously trusted server.").map_err(|_| "Terminal output failed")?;
    loop {
        let grants = request("/mcp/trust", None)?;
        writeln!(out, "\nTrust decisions:").map_err(|_| "Terminal output failed")?;
        for grant in grants
            .as_array()
            .ok_or("Invalid trusted-profile response")?
        {
            writeln!(
                out,
                "  {} — {} ({} servers)",
                text(&grant["profile"]),
                text(&grant["status"]),
                text(&grant["server_count"])
            )
            .map_err(|_| "Terminal output failed")?;
        }
        let Some(action) = input(
            reader,
            out,
            "[r] Review a profile / [v] Revoke a profile / [q] Quit: ",
        )?
        else {
            return Ok(());
        };
        match action.as_str() {
            "q" | "" => return Ok(()),
            "v" => {
                let Some(profile) = input(reader, out, "Profile to revoke: ")? else {
                    return Ok(());
                };
                if !grants
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|g| g["profile"] == profile)
                {
                    writeln!(out, "No trusted snapshot with that profile.")
                        .map_err(|_| "Terminal output failed")?;
                    continue;
                }
                if input(
                    reader,
                    out,
                    "Type REVOKE to block future sessions for this profile: ",
                )?
                .as_deref()
                    == Some("REVOKE")
                {
                    request(
                        "/mcp/trust",
                        Some(json!({"action":"revoke","profile":profile})),
                    )?;
                }
            }
            "r" => {
                let Some(profile) =
                    input(reader, out, "Profile (blank uses the active profile): ")?
                else {
                    return Ok(());
                };
                let sources = request("/mcp/sources", None)?;
                writeln!(out, "Available source locations (not yet trusted):")
                    .map_err(|_| "Terminal output failed")?;
                for source in sources.as_array().ok_or("Invalid sources response")? {
                    writeln!(
                        out,
                        "  {}: {} [{}]",
                        text(&source["id"]),
                        text(&source["path"]),
                        text(&source["status"])
                    )
                    .map_err(|_| "Terminal output failed")?;
                }
                let Some(path) = input(
                    reader,
                    out,
                    "Source ID or absolute MCP file/directory (blank uses daemon configuration): ",
                )?
                else {
                    return Ok(());
                };
                let source_id = if sources.as_array().unwrap().iter().any(|v| v["id"] == path) {
                    Some(path.clone())
                } else {
                    None
                };
                let preview = request(
                    "/mcp/discovery",
                    Some(
                        json!({"profile":if profile.is_empty(){None}else{Some(profile)},"source_id":source_id,"project_path":if path.is_empty() || source_id.is_some(){None}else{Some(path)}}),
                    ),
                )?;
                let id = preview
                    .get("review_id")
                    .and_then(Value::as_str)
                    .ok_or("Invalid review response")?
                    .to_string();
                let result = review(reader, out, &preview);
                let approve = matches!(result, Ok(true));
                let decision = request(
                    "/mcp/trust",
                    Some(json!({"action":if approve{"approve"}else{"cancel"},"review_id":id})),
                );
                result?;
                decision?;
                writeln!(out,"{}",if approve {"Approved snapshot for future sessions using this profile. The approval survives restart while source contents match. Revocation also persists."} else {"Cancelled. No trust change and no server started."}).map_err(|_| "Terminal output failed")?;
            }
            _ => {
                writeln!(out, "Choose r, v, or q.").map_err(|_| "Terminal output failed")?;
            }
        }
    }
}
fn review(
    reader: &mut impl BufRead,
    out: &mut impl Write,
    preview: &Value,
) -> Result<bool, String> {
    writeln!(
        out,
        "\nProfile: {}\nProfile/override source: {}",
        text(&preview["profile"]),
        text(&preview["profiles_path"])
    )
    .map_err(|_| "Terminal output failed")?;
    for server in preview["servers"]
        .as_array()
        .ok_or("Invalid discovery response")?
    {
        writeln!(out,"\n{} — {}\n  Source: {} ({})\n  Transport: {}  Executable: {}  Endpoint origin: {}\n  Enabled: {}  Working directory: {}\n  Arguments: {} (values hidden)\n  Environment names: {}\n  Header names: {}\n  Profile override fields: {}",
            text(&server["name"]),text(&server["status"]),text(&server["path"]),text(&server["source"]),text(&server["transport"]),text(&server["command"]),text(&server["endpoint"]),text(&server["enabled"]),text(&server["cwd"]),text(&server["argument_count"]),text(&server["environment_names"]),text(&server["header_names"]),text(&server["overrides"])).map_err(|_| "Terminal output failed")?;
    }
    writeln!(out,"\nOnly selected entries will attach. User definitions win duplicates.\nArguments, environment/header values, and URL paths/credentials are hidden.\nReview the full source files privately before trusting them. Programs and files they reference can change.\nTrust permits future sessions to run these commands or connect to these endpoints as you.\nApproval does not start servers now. This review expires in five minutes.").map_err(|_| "Terminal output failed")?;
    Ok(input(
        reader,
        out,
        "Type TRUST to approve this exact snapshot; anything else cancels: ",
    )?
    .as_deref()
        == Some("TRUST"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancel_and_eof_never_approve_and_display_escapes_config() {
        for ending in ["\nq\n", ""] {
            let mut calls = Vec::new();
            let mut output = Vec::new();
            screen(&mut io::Cursor::new(format!("r\n\n\n{ending}")),&mut output,|path,body| {
                calls.push(body.clone());
                Ok(if path=="/mcp/discovery" { json!({"review_id":"id","profile":"test\u{1b}[2J","profiles_path":"/tmp/config","servers":[]}) } else if body.is_none() { json!([]) } else { json!({}) })
            }).unwrap();
            assert!(calls.iter().flatten().any(|c| c["action"] == "cancel"));
            assert!(!calls.iter().flatten().any(|c| c["action"] == "approve"));
            assert!(!output.contains(&0x1b));
        }
    }
    #[test]
    fn known_source_selection_and_blocked_status_are_visible() {
        let mut selection = None;
        let mut output = Vec::new();
        screen(&mut io::Cursor::new("r\ndefault\nvscode_user\nTRUST\nq\n"), &mut output, |path, body| {
            if path == "/mcp/discovery" {
                selection = body;
                return Ok(json!({"review_id":"opaque","profile":"default","servers":[]}));
            }
            if path == "/mcp/sources" {
                return Ok(json!([{"id":"vscode_user","path":"/config/Code/User/mcp.json","status":"available_for_review"}]));
            }
            Ok(if body.is_none() {json!([{"profile":"default","status":"needs_review","server_count":0}])} else {json!({})})
        }).unwrap();
        assert_eq!(selection.unwrap()["source_id"], "vscode_user");
        assert!(String::from_utf8(output).unwrap().contains("needs_review"));
    }
    #[test]
    fn explicit_confirmation_submits_only_the_review_id() {
        let mut approved = None;
        screen(&mut io::Cursor::new("r\ndefault\n/tmp/project\nTRUST\nq\n"),&mut Vec::new(),|path,body| {
            if body.as_ref().is_some_and(|b| b["action"]=="approve") { approved=body.clone(); }
            Ok(if path=="/mcp/discovery" { json!({"review_id":"opaque","profile":"default","profiles_path":"config","servers":[]}) } else if body.is_none(){json!([])} else {json!({})})
        }).unwrap();
        assert_eq!(
            approved,
            Some(json!({"action":"approve","review_id":"opaque"}))
        );
    }
}
