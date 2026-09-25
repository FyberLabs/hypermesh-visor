//! CLI login files. Same paths and TOML shape as hypermesh-cli.
//! This is not the visor session vault. The vault lives only in the visor process.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::pages::{API_BASE, CHAT_BASE};

const FORBIDDEN_PREFIXES: &[&str] = &["hm_dev_", "hm_rtr_", "hm_site_"];

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

/// Write the renter API key the CLI reads. Refuses host, router, and site keys.
pub fn write_login(
    dir: &Path,
    api_key: &str,
    tenant_id: &str,
    renter_user_id: &str,
) -> Result<(), StoreError> {
    let api_key = api_key.trim();
    let tenant_id = tenant_id.trim();
    let renter_user_id = renter_user_id.trim();
    if api_key.is_empty() {
        return Err(StoreError("api key is required".into()));
    }
    for prefix in FORBIDDEN_PREFIXES {
        if api_key.starts_with(prefix) {
            return Err(StoreError(format!(
                "{prefix} is not a renter identity; use an org API key (purpose: renter)"
            )));
        }
    }
    if tenant_id.is_empty() {
        return Err(StoreError("tenant id is required".into()));
    }
    let key_toml = toml_quote(api_key)?;
    let tenant_toml = toml_quote(tenant_id)?;
    fs::create_dir_all(dir).map_err(|err| StoreError(err.to_string()))?;
    let mut mode = fs::metadata(dir).map_err(|err| StoreError(err.to_string()))?;
    let mut perms = mode.permissions();
    perms.set_mode(0o700);
    fs::set_permissions(dir, perms).map_err(|err| StoreError(err.to_string()))?;

    let mut config = format!(
        "api_base = {api}\nchat_base = {chat}\ntenant_id = {tenant}\n",
        api = toml_quote(API_BASE)?,
        chat = toml_quote(CHAT_BASE)?,
        tenant = tenant_toml,
    );
    if !renter_user_id.is_empty() {
        config.push_str(&format!(
            "renter_user_id = {}\n",
            toml_quote(renter_user_id)?
        ));
    }
    fs::write(config_path(dir), config).map_err(|err| StoreError(err.to_string()))?;

    let cred = format!("api_key = {key_toml}\n");
    let path = credentials_path(dir);
    fs::write(&path, cred).map_err(|err| StoreError(err.to_string()))?;
    mode = fs::metadata(&path).map_err(|err| StoreError(err.to_string()))?;
    let mut perms = mode.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(&path, perms).map_err(|err| StoreError(err.to_string()))?;
    Ok(())
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
    fn writes_cli_files_and_rejects_host_keys() {
        let dir = std::env::temp_dir().join(format!(
            "hypermesh-cred-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = fs::remove_dir_all(&dir);
        write_login(
            &dir,
            "abcd1234.secretvalue",
            "22222222-2222-2222-2222-222222222222",
            "11111111-1111-1111-1111-111111111111",
        )
        .unwrap();
        let cred = fs::read_to_string(credentials_path(&dir)).unwrap();
        assert_eq!(cred, "api_key = \"abcd1234.secretvalue\"\n");
        let mode = fs::metadata(credentials_path(&dir)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let config = fs::read_to_string(config_path(&dir)).unwrap();
        assert!(config.contains("tenant_id = \"22222222-2222-2222-2222-222222222222\""));
        assert!(config.contains("renter_user_id = \"11111111-1111-1111-1111-111111111111\""));
        assert!(!config.contains("secretvalue"));
        assert!(write_login(&dir, "hm_dev_nope", "t", "").is_err());
        assert!(write_login(&dir, "hm_rtr_nope", "t", "").is_err());
        assert!(write_login(&dir, "hm_site_nope", "t", "").is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
