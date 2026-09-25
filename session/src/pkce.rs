//! PKCE S256 (RFC 7636) and the OAuth `state` value.

use std::fs::File;
use std::io::Read;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};

use crate::AuthError;

/// 32 random bytes, base64url without padding: 43 characters from the PKCE alphabet.
pub fn new_verifier() -> Result<String, AuthError> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes(32)?))
}

pub fn new_state() -> Result<String, AuthError> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes(24)?))
}

pub fn challenge_s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn random_bytes(n: usize) -> Result<Vec<u8>, AuthError> {
    let mut buf = vec![0u8; n];
    let mut file = File::open("/dev/urandom").map_err(|err| {
        AuthError::message(format!("could not read randomness for sign-in: {err}"))
    })?;
    file.read_exact(&mut buf).map_err(|err| {
        AuthError::message(format!("could not read randomness for sign-in: {err}"))
    })?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s256_matches_the_rfc7636_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            challenge_s256(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_verifier_is_pkce_and_matches_its_challenge() {
        let verifier = new_verifier().unwrap();
        assert!(verifier.len() >= 43 && verifier.len() <= 128);
        assert!(verifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')));
        let again = challenge_s256(&verifier);
        assert_eq!(again, challenge_s256(&verifier));
        assert_ne!(again, verifier);
    }
}
