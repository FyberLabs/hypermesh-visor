//! Companion options. This file is not the visor vault and not the CLI key.

use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use toml::Value;

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
    let file_secs = read_u64(dir, RESUME_AFTER, DEFAULT_RESUME_AFTER_SECS);
    resume_secs_from(
        std::env::var("HYPERMESH_AUTOMATION_RESUME_SECS")
            .ok()
            .as_deref(),
        file_secs,
    )
}

/// A positive `HYPERMESH_AUTOMATION_RESUME_SECS` shortens idle resume for the
/// display harness. Empty, zero, and non-numeric values are ignored.
fn resume_secs_override(value: Option<&str>) -> Option<u64> {
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }
    match raw.parse::<u64>() {
        Ok(secs) if secs >= 1 => Some(secs),
        _ => None,
    }
}

fn resume_secs_from(override_value: Option<&str>, file_secs: u64) -> u64 {
    resume_secs_override(override_value).unwrap_or(file_secs.max(1))
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

/// How often the window may stat `companion-notice`.
pub const NOTICE_REFRESH: Duration = Duration::from_secs(1);

/// Cached upgrading notice. The file is stat'ed at most once per refresh, and
/// read only when the mtime changes.
#[derive(Debug, Default)]
pub struct NoticeCache {
    checked_at: Option<Instant>,
    mtime: Option<SystemTime>,
    upgrading: bool,
}

impl NoticeCache {
    pub fn current(&mut self, dir: &Path, now: Instant) -> bool {
        let due = self
            .checked_at
            .map(|at| now.saturating_duration_since(at) >= NOTICE_REFRESH)
            .unwrap_or(true);
        if !due {
            return self.upgrading;
        }
        let path = dir.join(NOTICE_FILE);
        let mtime = fs::metadata(&path).and_then(|meta| meta.modified()).ok();
        refresh_cached_notice(self, now, NOTICE_REFRESH, mtime, || upgrading_notice(dir))
    }
}

/// Apply one refresh decision. `load` runs on the first fill and when `mtime` changes
/// after the interval. A missing file is `mtime == None` and is not read again until
/// that changes.
pub fn refresh_cached_notice(
    cache: &mut NoticeCache,
    now: Instant,
    interval: Duration,
    mtime: Option<SystemTime>,
    load: impl FnOnce() -> bool,
) -> bool {
    if cache
        .checked_at
        .is_some_and(|at| now.saturating_duration_since(at) < interval)
    {
        return cache.upgrading;
    }
    if cache.checked_at.is_some() && cache.mtime == mtime {
        cache.checked_at = Some(now);
        return cache.upgrading;
    }
    let upgrading = load();
    cache.upgrading = upgrading;
    cache.mtime = mtime;
    cache.checked_at = Some(now);
    upgrading
}

fn read_bool(dir: &Path, key: &str, default: bool) -> bool {
    match table(dir).get(key) {
        Some(Value::Boolean(value)) => *value,
        Some(_) => {
            warn_invalid(key, "expected a boolean");
            default
        }
        None => default,
    }
}

fn read_u64(dir: &Path, key: &str, default: u64) -> u64 {
    match table(dir).get(key) {
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        Some(_) => {
            warn_invalid(key, "expected a non-negative integer");
            default
        }
        None => default,
    }
}

fn table(dir: &Path) -> toml::Table {
    let Ok(text) = fs::read_to_string(companion_path(dir)) else {
        return toml::Table::new();
    };
    if text.trim().is_empty() {
        return toml::Table::new();
    }
    match text.parse::<toml::Table>() {
        Ok(table) => table,
        Err(err) => {
            warn_invalid("file", &err.to_string());
            toml::Table::new()
        }
    }
}

fn warn_invalid(key: &str, detail: &str) {
    static WARNED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let mut warned = WARNED.lock().unwrap_or_else(|poison| poison.into_inner());
    if warned.iter().any(|item| item == key) {
        return;
    }
    warned.push(key.to_string());
    eprintln!("hypermesh-companion: companion.toml {key}: {detail}; using the default");
}

fn write_value(dir: &Path, key: &str, value: &str) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    let path = companion_path(dir);
    let text = fs::read_to_string(&path).unwrap_or_default();
    let mut root = if text.trim().is_empty() {
        toml::Table::new()
    } else {
        text.parse::<toml::Table>().map_err(|err| err.to_string())?
    };
    let wrapped = format!("value = {value}");
    let mut parsed_table: toml::Table = wrapped
        .parse()
        .map_err(|err: toml::de::Error| err.to_string())?;
    let parsed = parsed_table
        .remove("value")
        .ok_or_else(|| "setting value".to_string())?;
    root.insert(key.to_string(), parsed);
    let mut body = toml::to_string(&root).map_err(|err| err.to_string())?;
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
        let dir =
            std::env::temp_dir().join(format!("hypermesh-eyes-{}-{}", std::process::id(), line!()));
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

    #[test]
    fn toml_comments_and_underscores_are_read() {
        let dir =
            std::env::temp_dir().join(format!("hypermesh-toml-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("companion.toml"),
            "sleep_after_secs = 1_800 # thirty minutes\nautomation_resume_secs = 600 # ten minutes\nbug_cursor = true\n",
        )
        .unwrap();
        assert_eq!(sleep_after_secs(&dir), 1_800);
        assert_eq!(automation_resume_secs(&dir), 600);
        assert!(bug_cursor(&dir));
        assert_eq!(DEFAULT_RESUME_AFTER_SECS, 120);
        assert_eq!(resume_secs_override(None), None);
        assert_eq!(resume_secs_override(Some("")), None);
        assert_eq!(resume_secs_override(Some("nope")), None);
        assert_eq!(resume_secs_override(Some("0")), None);
        assert_eq!(resume_secs_override(Some("2")), Some(2));
        assert_eq!(resume_secs_override(Some(" 15 ")), Some(15));
        assert_eq!(resume_secs_from(Some("2"), 600), 2);
        assert_eq!(resume_secs_from(Some("0"), 600), 600);
        assert_eq!(resume_secs_from(None, 0), 1);
        fs::write(dir.join("companion.toml"), "sleep_after_secs = \"nope\"\n").unwrap();
        assert_eq!(sleep_after_secs(&dir), DEFAULT_SLEEP_AFTER_SECS);
        fs::write(dir.join("companion.toml"), "sleep_after_secs = [\n").unwrap();
        assert_eq!(sleep_after_secs(&dir), DEFAULT_SLEEP_AFTER_SECS);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_trailing_table_keeps_its_keys_when_a_root_setting_changes() {
        let dir = std::env::temp_dir().join(format!(
            "hypermesh-toml-extra-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("companion.toml"),
            "bug_cursor = false\n\n[extra]\nkept = 1\n",
        )
        .unwrap();
        set_bug_cursor(&dir, true).unwrap();
        let table: toml::Table = fs::read_to_string(dir.join("companion.toml"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(table.get("bug_cursor"), Some(&Value::Boolean(true)));
        let extra = table.get("extra").and_then(Value::as_table).unwrap();
        assert_eq!(extra.get("kept"), Some(&Value::Integer(1)));
        assert!(extra.get("bug_cursor").is_none());
        assert!(bug_cursor(&dir));

        fs::write(dir.join("companion.toml"), "[extra]\nkept = 1\n").unwrap();
        set_eyes_follow_pointer(&dir, true).unwrap();
        let table: toml::Table = fs::read_to_string(dir.join("companion.toml"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            table.get("eyes_follow_pointer"),
            Some(&Value::Boolean(true))
        );
        let extra = table.get("extra").and_then(Value::as_table).unwrap();
        assert_eq!(extra.get("kept"), Some(&Value::Integer(1)));
        assert!(extra.get("eyes_follow_pointer").is_none());
        assert!(eyes_follow_pointer(&dir));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_upgrading_notice_is_reread_only_after_the_interval_when_mtime_changes() {
        let mut cache = NoticeCache::default();
        let start = Instant::now();
        let mtime = SystemTime::UNIX_EPOCH;
        let mut loads = 0;
        let first = refresh_cached_notice(&mut cache, start, NOTICE_REFRESH, Some(mtime), || {
            loads += 1;
            true
        });
        assert!(first);
        assert_eq!(loads, 1);
        let again = refresh_cached_notice(
            &mut cache,
            start + Duration::from_millis(40),
            NOTICE_REFRESH,
            Some(mtime + Duration::from_secs(5)),
            || {
                loads += 1;
                false
            },
        );
        assert!(again);
        assert_eq!(loads, 1);
        let same = refresh_cached_notice(
            &mut cache,
            start + NOTICE_REFRESH,
            NOTICE_REFRESH,
            Some(mtime),
            || {
                loads += 1;
                false
            },
        );
        assert!(same);
        assert_eq!(loads, 1);
        let changed = refresh_cached_notice(
            &mut cache,
            start + NOTICE_REFRESH + NOTICE_REFRESH,
            NOTICE_REFRESH,
            Some(mtime + Duration::from_secs(5)),
            || {
                loads += 1;
                false
            },
        );
        assert!(!changed);
        assert_eq!(loads, 2);
        let missing = refresh_cached_notice(
            &mut cache,
            start + NOTICE_REFRESH * 3,
            NOTICE_REFRESH,
            None,
            || {
                loads += 1;
                false
            },
        );
        assert!(!missing);
        assert_eq!(loads, 3);
        let still_missing = refresh_cached_notice(
            &mut cache,
            start + NOTICE_REFRESH * 4,
            NOTICE_REFRESH,
            None,
            || {
                loads += 1;
                true
            },
        );
        assert!(!still_missing);
        assert_eq!(loads, 3);
    }
}
