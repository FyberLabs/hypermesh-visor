//! Companion options. This file is not the visor vault and not the CLI key.

use std::fs;
use std::path::Path;

const KEY: &str = "eyes_follow_pointer";

pub fn companion_path(dir: &Path) -> std::path::PathBuf {
    dir.join("companion.toml")
}

/// Pupils track the pointer only when this is true. Missing file means off.
pub fn eyes_follow_pointer(dir: &Path) -> bool {
    let Ok(text) = fs::read_to_string(companion_path(dir)) else {
        return false;
    };
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix(KEY) else {
            continue;
        };
        let value = rest.trim().trim_start_matches('=').trim();
        return value == "true";
    }
    false
}

pub fn set_eyes_follow_pointer(dir: &Path, on: bool) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    let path = companion_path(dir);
    let mut lines: Vec<String> = fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().starts_with(KEY))
        .map(str::to_string)
        .collect();
    lines.push(format!("{KEY} = {}", if on { "true" } else { "false" }));
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
        let _ = fs::remove_dir_all(&dir);
    }
}
