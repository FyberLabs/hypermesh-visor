//! Shared desktop dialogs. Arguments carry labels only; private edits use stdin.
use std::{
    io::Write,
    process::{Command, Stdio},
};
pub(crate) fn dialog(args: &[String], input: Option<&str>) -> Result<Option<String>, String> {
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
        .map_err(|_| "Cannot open MCP controls. Install zenity and run in your desktop session.")?;
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
pub(crate) fn args(kind: &str, title: &str, text: &str) -> Vec<String> {
    vec![
        format!("--{kind}"),
        format!("--title={title}"),
        "--no-markup".into(),
        format!("--text={text}"),
        "--width=650".into(),
    ]
}
pub(crate) fn choose(
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

pub(crate) fn text(
    ui: &mut impl FnMut(&[String], Option<&str>) -> Result<Option<String>, String>,
    title: &str,
    initial: &str,
    editable: bool,
) -> Result<Option<String>, String> {
    let mut a = vec![
        "--text-info".into(),
        format!("--title={title}"),
        "--width=760".into(),
        "--height=500".into(),
    ];
    if editable {
        a.push("--editable".into());
    }
    ui(&a, Some(initial))
}
