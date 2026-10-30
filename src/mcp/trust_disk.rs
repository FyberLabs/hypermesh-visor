//! Private, atomic trust receipts. Never writes server definitions or secrets.
use super::McpError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fingerprint {
    pub path: PathBuf,
    pub sha256: Option<String>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub review_id: Uuid,
    pub revoked: bool,
    pub source_kind: String,
    pub sources: Vec<Fingerprint>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    pub version: u32,
    pub profiles: BTreeMap<String, Receipt>,
}
const LIMIT: u64 = 128 * 1024;
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn error() -> McpError {
    McpError::message(
        "MCP trust storage unavailable or unsafe; repair its private files before opening sessions",
    )
}
fn directory(base: &Path) -> PathBuf {
    base.join("mcp-trust")
}
fn private(meta: &fs::Metadata) -> bool {
    meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0
}
fn check_dir(path: &Path) -> Result<(), McpError> {
    let meta = fs::symlink_metadata(path).map_err(|_| error())?;
    if !meta.is_dir() || !private(&meta) {
        return Err(error());
    }
    Ok(())
}
fn read_file(path: &Path) -> Result<Vec<u8>, McpError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| error())?;
    let meta = file.metadata().map_err(|_| error())?;
    if !meta.is_file() || !private(&meta) || meta.nlink() != 1 {
        return Err(error());
    }
    let mut data = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut data)
        .map_err(|_| error())?;
    if data.len() as u64 > LIMIT {
        return Err(error());
    }
    Ok(data)
}
pub fn read(base: &Path) -> Result<Option<Vec<u8>>, McpError> {
    let dir = directory(base);
    match fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(error()),
        Ok(_) => check_dir(&dir)?,
    }
    // An existing directory with a missing ledger is damage, not a reset to default trust.
    read_file(&dir.join("approvals.json")).map(Some)
}
pub fn decode(base: &Path, bytes: &[u8]) -> Result<Ledger, McpError> {
    let ledger: Ledger = serde_json::from_slice(bytes).map_err(|_| error())?;
    if ledger.version != 1 || ledger.profiles.len() > 8 {
        return Err(error());
    }
    for (name, r) in &ledger.profiles {
        if name.is_empty()
            || name.len() > 512
            || !(2..=3).contains(&r.sources.len())
            || !matches!(
                r.source_kind.as_str(),
                "selected_project" | "cursor_global" | "vscode_user"
            )
        {
            return Err(error());
        }
        if r.sources[0].path != base.join("mcp.json")
            || r.sources[1].path != base.join("mcp-profiles.json")
        {
            return Err(error());
        }
        for source in &r.sources {
            if !source.path.is_absolute()
                || source.path.as_os_str().len() > 4096
                || source
                    .sha256
                    .as_ref()
                    .is_some_and(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                return Err(error());
            }
        }
    }
    Ok(ledger)
}
pub fn save(base: &Path, expected: Option<&[u8]>, ledger: &Ledger) -> Result<Vec<u8>, McpError> {
    let dir = directory(base);
    fs::create_dir_all(base).map_err(|_| error())?;
    let created = match fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(_) => return Err(error()),
    };
    check_dir(&dir)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(dir.join("lock"))
        .map_err(|_| error())?;
    let meta = lock.metadata().map_err(|_| error())?;
    if !meta.is_file() || !private(&meta) || meta.nlink() != 1 {
        return Err(error());
    }
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(McpError::message("MCP trust is being updated; retry"));
    }
    let current = if created { None } else { read(base)? };
    if current.as_deref() != expected {
        return Err(McpError::message(
            "MCP trust changed in another process; refresh and review again",
        ));
    }
    let data = serde_json::to_vec_pretty(ledger).map_err(|_| error())?;
    if data.len() as u64 > LIMIT {
        return Err(error());
    }
    decode(base, &data)?;
    let path = dir.join(format!(".{}", Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|_| error())?;
        file.write_all(&data)
            .and_then(|_| file.sync_all())
            .map_err(|_| error())?;
        fs::rename(&path, dir.join("approvals.json")).map_err(|_| error())?;
        File::open(&dir)
            .and_then(|d| d.sync_all())
            .map_err(|_| error())?;
        Ok(data)
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}
