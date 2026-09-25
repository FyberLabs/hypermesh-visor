//! CLI login files. Same paths and TOML shape as hypermesh-cli.
//! This is not the visor session vault. The vault lives only in the visor process.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::pages::{API_BASE, CHAT_BASE};

#[derive(Debug)]
pub struct StoreError(pub String);

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StoreError {}

pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("HYPERMESH_CONFIG_DIR") {
        let dir = dir.to_string_lossy();
        if !dir.trim().is_empty() {
            return PathBuf::from(dir.trim());
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let xdg = xdg.to_string_lossy();
        if !xdg.trim().is_empty() {
            return PathBuf::from(xdg.trim()).join("hypermesh");
        }
    }
    let home = std::env::var_os("HOME").map(|h| PathBuf::from(h));
    match home {
        Some(home) if !home.as_os_str().is_empty() => home.join(".config").join("hypermesh"),
        _ => PathBuf::from(".config").join("hypermesh"),
    }
}

pub fn credentials_path(dir: &Path) -> PathBuf {
    dir.join("credentials")
}

pub fn config_path(dir: &Path) -> PathBuf {
    dir.join("config.toml")
}

/// Non-secret profile. The refresh token is not written here.
pub fn write_profile(dir: &Path, tenant_id: &str, renter_user_id: &str) -> Result<(), StoreError> {
    let tenant_id = tenant_id.trim();
    let renter_user_id = renter_user_id.trim();
    if tenant_id.is_empty() {
        return Err(StoreError("tenant id is required".into()));
    }
    fs::create_dir_all(dir).map_err(|err| StoreError(err.to_string()))?;
    let mode = fs::metadata(dir).map_err(|err| StoreError(err.to_string()))?;
    let mut perms = mode.permissions();
    perms.set_mode(0o700);
    fs::set_permissions(dir, perms).map_err(|err| StoreError(err.to_string()))?;
    let mut config = format!(
        "api_base = {api}\nchat_base = {chat}\ntenant_id = {tenant}\n",
        api = toml_quote(API_BASE)?,
        chat = toml_quote(CHAT_BASE)?,
        tenant = toml_quote(tenant_id)?,
    );
    if !renter_user_id.is_empty() {
        config.push_str(&format!(
            "renter_user_id = {}\n",
            toml_quote(renter_user_id)?
        ));
    }
    fs::write(config_path(dir), config).map_err(|err| StoreError(err.to_string()))?;
    forget_plaintext_credentials(dir)
}

/// Remove a leftover API-key file so the keychain session is the only copy.
pub fn forget_plaintext_credentials(dir: &Path) -> Result<(), StoreError> {
    match fs::remove_file(credentials_path(dir)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(StoreError(err.to_string())),
    }
}

fn toml_quote(value: &str) -> Result<String, StoreError> {
    if value
        .chars()
        .any(|ch| ch == '"' || ch == '\\' || ch.is_control())
    {
        return Err(StoreError(
            "refusing a credential value the CLI parser would not round-trip".into(),
        ));
    }
    Ok(format!("\"{value}\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_has_no_token_file() {
        let dir =
            std::env::temp_dir().join(format!("hypermesh-cred-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(credentials_path(&dir), "api_key = \"secret\"\n").unwrap();
        write_profile(
            &dir,
            "22222222-2222-2222-2222-222222222222",
            "11111111-1111-1111-1111-111111111111",
        )
        .unwrap();
        assert!(fs::metadata(credentials_path(&dir)).is_err());
        let config = fs::read_to_string(config_path(&dir)).unwrap();
        assert!(config.contains("tenant_id = \"22222222-2222-2222-2222-222222222222\""));
        assert!(!config.contains("secret"));
        assert!(!config.contains("refresh"));
        let _ = fs::remove_dir_all(&dir);
    }
}
