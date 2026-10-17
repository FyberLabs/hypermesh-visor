//! Companion options. This file is not the visor vault and not the CLI key.

use std::fs;
use std::path::Path;

const EYES: &str = "eyes_follow_pointer";
const BUG_CURSOR: &str = "bug_cursor";
const SLEEP_AFTER: &str = "sleep_after_secs";
const RESUME_AFTER: &str = "automation_resume_secs";
const NOTICE_FILE: &str = "companion-notice";

/// The mascot sleeps after this many seconds without companion or session use.
pub const DEFAULT_SLEEP_AFTER_SECS: u64 = 900;
/// A paused automation continues after this many seconds without user input.
pub const DEFAULT_RESUME_AFTER_SECS: u64 = 120;

pub fn companion_path(dir: &Path) -> std::path::PathBuf {
    dir.join("companion.toml")
}

/// Pupils track the pointer only when this is true. Missing file means off.
pub fn eyes_follow_pointer(dir: &Path) -> bool {
    read_bool(dir, EYES, false)
}

pub fn set_eyes_follow_pointer(dir: &Path, on: bool) -> Result<(), String> {
    write_value(dir, EYES, if on { "true" } else { "false" })
}

/// Bug cursor outside an automation. Missing file means off.
pub fn bug_cursor(dir: &Path) -> bool {
    read_bool(dir, BUG_CURSOR, false)
}

pub fn set_bug_cursor(dir: &Path, on: bool) -> Result<(), String> {
    write_value(dir, BUG_CURSOR, if on { "true" } else { "false" })
}

pub fn sleep_after_secs(dir: &Path) -> u64 {
    read_u64(dir, SLEEP_AFTER, DEFAULT_SLEEP_AFTER_SECS)
}

pub fn set_sleep_after_secs(dir: &Path, secs: u64) -> Result<(), String> {
    write_value(dir, SLEEP_AFTER, &secs.to_string())
}

pub fn automation_resume_secs(dir: &Path) -> u64 {
    read_u64(dir, RESUME_AFTER, DEFAULT_RESUME_AFTER_SECS).max(1)
}

pub fn set_automation_resume_secs(dir: &Path, secs: u64) -> Result<(), String> {
    write_value(dir, RESUME_AFTER, &secs.max(1).to_string())
}

/// `true` when `companion-notice` contains `upgrading`. An updater writes that
/// file. This process does not.
pub fn upgrading_notice(dir: &Path) -> bool {
    fs::read_to_string(dir.join(NOTICE_FILE))
        .ok()
        .is_some_and(|text| text.trim() == "upgrading")
}

fn read_bool(dir: &Path, key: &str, default: bool) -> bool {
    match read_raw(dir, key) {
        Some(value) if value == "true" => true,
        Some(value) if value == "false" => false,
        _ => default,
    }
}

fn read_u64(dir: &Path, key: &str, default: u64) -> u64 {
    read_raw(dir, key)
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn read_raw(dir: &Path, key: &str) -> Option<String> {
    let text = fs::read_to_string(companion_path(dir)).ok()?;
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix(key) else {
            continue;
        };
        let rest = rest.trim();
        if !rest.starts_with('=') {
            continue;
        }
        return Some(rest.trim_start_matches('=').trim().to_string());
    }
    None
}

fn write_value(dir: &Path, key: &str, value: &str) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    let path = companion_path(dir);
    let mut lines: Vec<String> = fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().starts_with(key))
        .map(str::to_string)
        .collect();
    lines.push(format!("{key} = {value}"));
    let mut body = lines.join("\n");
    if !body.ends_with('\n') {
        body.push('\n');
    }
    fs::write(&path, body).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eyes_follow_defaults_off_and_round_trips() {
        let dir = std::env::temp_dir().join(format!(
            "hypermesh-eyes-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = fs::remove_dir_all(&dir);
        assert!(!eyes_follow_pointer(&dir));
        set_eyes_follow_pointer(&dir, false).unwrap();
        assert!(!eyes_follow_pointer(&dir));
        set_eyes_follow_pointer(&dir, true).unwrap();
        assert!(eyes_follow_pointer(&dir));
        set_eyes_follow_pointer(&dir, false).unwrap();
        assert!(!eyes_follow_pointer(&dir));
        assert!(!bug_cursor(&dir));
        set_bug_cursor(&dir, true).unwrap();
        assert!(bug_cursor(&dir));
        assert!(!eyes_follow_pointer(&dir));
        assert_eq!(sleep_after_secs(&dir), DEFAULT_SLEEP_AFTER_SECS);
        assert_eq!(automation_resume_secs(&dir), DEFAULT_RESUME_AFTER_SECS);
        set_sleep_after_secs(&dir, 60).unwrap();
        set_automation_resume_secs(&dir, 30).unwrap();
        assert_eq!(sleep_after_secs(&dir), 60);
        assert_eq!(automation_resume_secs(&dir), 30);
        assert!(bug_cursor(&dir));
        assert!(!upgrading_notice(&dir));
        fs::write(dir.join("companion-notice"), "upgrading\n").unwrap();
        assert!(upgrading_notice(&dir));
        let _ = fs::remove_dir_all(&dir);
    }
}
