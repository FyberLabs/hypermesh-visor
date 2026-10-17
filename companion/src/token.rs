//! The companion token the visor requires before a user pause can be lifted.
//! The visor creates `companion.token` in the config dir. This process only reads it.

use std::fs;
use std::path::Path;

pub fn read(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join("companion.token")).ok()?;
    let token = text.trim();
    if token.len() < 32 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_hex_token_and_ignores_anything_else() {
        let dir = std::env::temp_dir().join(format!(
            "hypermesh-companion-read-token-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert!(read(&dir).is_none());
        fs::write(dir.join("companion.token"), "abcd\n").unwrap();
        assert!(read(&dir).is_none());
        let token = "ab".repeat(32);
        fs::write(dir.join("companion.token"), format!("{token}\n")).unwrap();
        assert_eq!(read(&dir).as_deref(), Some(token.as_str()));
        let _ = fs::remove_dir_all(&dir);
    }
}
