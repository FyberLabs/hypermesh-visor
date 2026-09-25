//! Native-app session for Hypermesh.
//!
//! Sign-in is authorization code with PKCE on an ephemeral loopback port, or
//! the device authorization grant when there is no browser. The refresh token
//! is the only persisted credential, and it lives in the OS keychain under
//! service `hypermesh`, account `session`.

mod oauth;
mod pkce;
mod store;

pub use oauth::{
    accept_callback, authorization_url, bind_loopback, callback_code, display_available,
    exchange_code, first_tenant, logout, open_system_browser, poll_device, prepare_browser_login,
    redirect_uri, refresh, refresh_session, revoke, sign_in, start_device, store_refresh,
    BrowserLogin, Clock, DeviceCodes, Endpoints, SystemClock, Tokens, ISSUER, LOGIN_TIMEOUT,
    PUBLIC_CLIENT_ID, SCOPE, SIGNED_IN_SENTENCE,
};
pub use pkce::{challenge_s256, new_state, new_verifier};
pub use store::{KeyringStore, MemoryStore, SessionStore, ACCOUNT, SERVICE};

#[derive(Debug)]
pub enum AuthError {
    Message(String),
    Oauth(String),
    Expired,
    Denied,
    StateMismatch,
    Timeout,
    NoKeychain,
    NoSession,
    NoRefreshToken,
    SessionEnded,
    BrowserUnavailable(String),
}

impl AuthError {
    pub fn message(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message(text) => f.write_str(text),
            Self::Oauth(error) => write!(f, "sign-in failed ({error})"),
            Self::Expired => f.write_str("sign-in expired before it was approved"),
            Self::Denied => f.write_str("sign-in was declined"),
            Self::StateMismatch => f.write_str("sign-in state did not match"),
            Self::Timeout => f.write_str("sign-in timed out waiting for the browser"),
            Self::NoKeychain => f.write_str(
                "No system keychain is available. Hypermesh will not store your session in a file.",
            ),
            Self::NoSession => f.write_str("not signed in"),
            Self::NoRefreshToken => f.write_str(
                "Keycloak did not return a refresh token. The public client must issue a refresh token for this sign-in session.",
            ),
            Self::SessionEnded => f.write_str("Your sign-in ended. Sign in again."),
            Self::BrowserUnavailable(detail) => {
                write!(f, "could not open a browser ({detail})")
            }
        }
    }
}

impl std::error::Error for AuthError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_keychain_does_not_offer_a_file() {
        let text = AuthError::NoKeychain.to_string();
        assert!(text.contains("keychain"));
        assert!(text.contains("will not store your session in a file"));
    }

    #[test]
    fn missing_refresh_token_does_not_ask_for_an_offline_token() {
        let text = AuthError::NoRefreshToken.to_string();
        assert!(text.contains("refresh token"));
        assert!(!text.contains("offline"));
        assert_eq!(SCOPE, "openid");
    }
}

/// The visor and the companion read this same entry. The CLI does too.
pub fn refresh_token(store: &impl SessionStore) -> Result<Option<String>, AuthError> {
    store.refresh_token()
}
