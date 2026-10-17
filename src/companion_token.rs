//! Capability that lets the companion lift a user pause.
//!
//! The visor writes a random token to `companion.token` in the config dir,
//! mode 0600. Resume, and start or stop while the gate is paused, must present
//! it. A session client has no route that reads this file.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "companion.token";
pub const HEADER: &str = "x-hypermesh-companion";

pub fn path(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

/// Read the token, or create it. A second caller sees the same value.
pub fn ensure(dir: &Path) -> std::io::Result<String> {
    let path = path(dir);
    if let Some(existing) = read_token(&path) {
        let _ = tighten(&path);
        return Ok(existing);
    }
    fs::create_dir_all(dir)?;
    let token = random_token()?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    match options.open(&path) {
        Ok(mut file) => {
            file.write_all(token.as_bytes())?;
            file.write_all(b"\n")?;
            let _ = tighten(&path);
            Ok(token)
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => read_token(&path)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "companion token")),
        Err(err) => Err(err),
    }
}

pub fn tokens_match(presented: &str, expected: &str) -> bool {
    let presented = presented.as_bytes();
    let expected = expected.as_bytes();
    if presented.len() != expected.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in presented.iter().zip(expected.iter()) {
        diff |= left ^ right;
    }
    diff == 0
}

/// A portal screenshot URI must not be allowed to open or delete this file.
pub fn is_token_file(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some(FILE_NAME)
}

fn read_token(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let token = text.trim();
    if token.len() < 32 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(token.to_string())
}

fn tighten(path: &Path) -> std::io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

fn random_token() -> std::io::Result<String> {
    let mut bytes = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        token.push_str(&format!("{byte:02x}"));
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_file_stays_private_and_stable() {
        let dir = std::env::temp_dir().join(format!(
            "hypermesh-companion-token-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = fs::remove_dir_all(&dir);
        let first = ensure(&dir).unwrap();
        let second = ensure(&dir).unwrap();
        assert_eq!(first, second);
        assert!(tokens_match(&first, &second));
        assert!(!tokens_match(&first, "nope"));
        assert_eq!(first.len(), 64);
        let mode = fs::metadata(path(&dir)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(is_token_file(&path(&dir)));
        assert!(!is_token_file(Path::new("/tmp/shot.png")));
        let _ = fs::remove_dir_all(&dir);
    }
}
