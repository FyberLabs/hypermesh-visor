//! Authorization-code + PKCE on a loopback port, and the device authorization grant.
//!
//! The public client has no secret. The listener takes one callback and then closes.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use crate::pkce::{challenge_s256, new_state, new_verifier};
use crate::store::SessionStore;
use crate::AuthError;

pub const PUBLIC_CLIENT_ID: &str = "hypermesh-native";
pub const ISSUER: &str = "https://auth.test.hyperme.sh/realms/controlplane";
/// SSO-session scope only. Offline tokens outlive the Keycloak session.
pub const SCOPE: &str = "openid";
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(180);
pub const SIGNED_IN_SENTENCE: &str = "You're signed in to Hypermesh. You can close this tab.";

#[derive(Clone, Debug)]
pub struct Endpoints {
    pub authorize_url: String,
    pub token_url: String,
    pub device_url: String,
    pub revoke_url: String,
    pub client_id: String,
}

impl Endpoints {
    pub fn panopticon() -> Self {
        Self::from_issuer(ISSUER, PUBLIC_CLIENT_ID)
    }

    pub fn from_issuer(issuer: &str, client_id: &str) -> Self {
        let issuer = issuer.trim_end_matches('/');
        Self {
            authorize_url: format!("{issuer}/protocol/openid-connect/auth"),
            token_url: format!("{issuer}/protocol/openid-connect/token"),
            device_url: format!("{issuer}/protocol/openid-connect/auth/device"),
            revoke_url: format!("{issuer}/protocol/openid-connect/revoke"),
            client_id: client_id.to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct BrowserLogin {
    pub state: String,
    pub verifier: String,
    pub challenge: String,
    pub redirect_uri: String,
    pub authorize_url: String,
}

pub fn redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}/callback")
}

pub fn bind_loopback() -> Result<(TcpListener, String), AuthError> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|err| AuthError::message(format!("could not bind 127.0.0.1: {err}")))?;
    let port = listener
        .local_addr()
        .map_err(|err| AuthError::message(err.to_string()))?
        .port();
    Ok((listener, redirect_uri(port)))
}

pub fn prepare_browser_login(
    endpoints: &Endpoints,
    redirect: &str,
) -> Result<BrowserLogin, AuthError> {
    let verifier = new_verifier()?;
    let challenge = challenge_s256(&verifier);
    let state = new_state()?;
    let authorize_url = authorization_url(endpoints, redirect, &state, &challenge);
    Ok(BrowserLogin {
        state,
        verifier,
        challenge,
        redirect_uri: redirect.to_string(),
        authorize_url,
    })
}

pub fn authorization_url(
    endpoints: &Endpoints,
    redirect: &str,
    state: &str,
    challenge: &str,
) -> String {
    format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        endpoints.authorize_url,
        encode_query(&endpoints.client_id),
        encode_query(redirect),
        encode_query(SCOPE),
        encode_query(state),
        encode_query(challenge),
    )
}

#[derive(Clone)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Duration,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("access_token", &"[redacted]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[redacted]"),
            )
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

pub fn exchange_code(
    endpoints: &Endpoints,
    redirect: &str,
    code: &str,
    verifier: &str,
) -> Result<Tokens, AuthError> {
    post_token(
        endpoints,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect),
            ("client_id", endpoints.client_id.as_str()),
            ("code_verifier", verifier),
        ],
    )
}

pub fn refresh(endpoints: &Endpoints, refresh_token: &str) -> Result<Tokens, AuthError> {
    post_token(
        endpoints,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", endpoints.client_id.as_str()),
        ],
    )
}

/// Refresh the in-memory access token. A normal refresh token is bound to the
/// Keycloak SSO session. When that session has ended, the keychain entry is
/// removed so the next command asks the user to sign in again.
pub fn refresh_session<S: SessionStore>(
    store: &S,
    endpoints: &Endpoints,
) -> Result<Tokens, AuthError> {
    let Some(refresh_token) = store.refresh_token()? else {
        return Err(AuthError::NoSession);
    };
    match refresh(endpoints, &refresh_token) {
        Ok(tokens) => {
            if let Some(next) = tokens.refresh_token.as_deref() {
                if next != refresh_token {
                    store.put_refresh_token(next)?;
                }
            }
            Ok(tokens)
        }
        Err(AuthError::Oauth(error)) if error == "invalid_grant" => {
            let _ = store.delete();
            Err(AuthError::SessionEnded)
        }
        Err(other) => Err(other),
    }
}

pub fn revoke(endpoints: &Endpoints, refresh_token: &str) -> Result<(), AuthError> {
    match ureq::post(&endpoints.revoke_url)
        .timeout(Duration::from_secs(30))
        .send_form(&[
            ("token", refresh_token),
            ("token_type_hint", "refresh_token"),
            ("client_id", endpoints.client_id.as_str()),
        ]) {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(code, response)) => {
            let error = oauth_error_code(&read_body(response));
            if error.as_deref() == Some("invalid_token") || code == 400 && error.is_none() {
                return Ok(());
            }
            if code == 400 && error.as_deref() == Some("unsupported_token_type") {
                return Ok(());
            }
            Err(AuthError::message(format!(
                "could not revoke the session ({})",
                error.unwrap_or_else(|| format!("HTTP {code}"))
            )))
        }
        Err(_) => Err(AuthError::message(
            "could not revoke the session. It is still in the keychain.",
        )),
    }
}

/// Save the refresh token and nothing else. Refuses to continue when the
/// token endpoint did not issue one.
pub fn store_refresh<S: SessionStore>(store: &S, tokens: &Tokens) -> Result<(), AuthError> {
    let Some(refresh_token) = tokens.refresh_token.as_deref() else {
        return Err(AuthError::NoRefreshToken);
    };
    if refresh_token == tokens.access_token {
        return Err(AuthError::NoRefreshToken);
    }
    store.put_refresh_token(refresh_token)
}

pub fn logout<S: SessionStore>(store: &S, endpoints: &Endpoints) -> Result<(), AuthError> {
    if let Some(refresh_token) = store.refresh_token()? {
        revoke(endpoints, &refresh_token)?;
    }
    store.delete()
}

#[derive(Clone, Debug)]
pub struct DeviceCodes {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub interval: Duration,
    pub expires_in: Duration,
}

pub fn start_device(endpoints: &Endpoints) -> Result<DeviceCodes, AuthError> {
    let json = post_form(
        &endpoints.device_url,
        &[
            ("client_id", endpoints.client_id.as_str()),
            ("scope", SCOPE),
        ],
    )?;
    let device_code = required_str(&json, "device_code")?;
    let user_code = required_str(&json, "user_code")?;
    let verification_uri = required_str(&json, "verification_uri")?;
    let interval = json
        .get("interval")
        .and_then(|value| value.as_u64())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(5));
    let expires_in = json
        .get("expires_in")
        .and_then(|value| value.as_u64())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .ok_or_else(|| AuthError::message("device authorization returned no expires_in"))?;
    Ok(DeviceCodes {
        device_code,
        user_code,
        verification_uri,
        verification_uri_complete: json
            .get("verification_uri_complete")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        interval,
        expires_in,
    })
}

pub trait Clock {
    fn now(&self) -> Instant;
    fn wait(&mut self, duration: Duration);
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wait(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
}

pub fn poll_device<C: Clock>(
    endpoints: &Endpoints,
    device_code: &str,
    mut interval: Duration,
    expires_in: Duration,
    clock: &mut C,
) -> Result<Tokens, AuthError> {
    if interval.is_zero() {
        interval = Duration::from_secs(5);
    }
    let deadline = clock.now() + expires_in;
    loop {
        if clock.now() >= deadline {
            return Err(AuthError::Expired);
        }
        let remaining = deadline.saturating_duration_since(clock.now());
        let step = interval.min(remaining);
        if step.is_zero() {
            return Err(AuthError::Expired);
        }
        clock.wait(step);
        if clock.now() >= deadline {
            return Err(AuthError::Expired);
        }
        match post_token(
            endpoints,
            &[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_code),
                ("client_id", endpoints.client_id.as_str()),
            ],
        ) {
            Ok(tokens) => return Ok(tokens),
            Err(AuthError::Oauth(error)) if error == "authorization_pending" => continue,
            Err(AuthError::Oauth(error)) if error == "slow_down" => {
                interval += Duration::from_secs(5);
                continue;
            }
            Err(AuthError::Oauth(error)) if error == "expired_token" => {
                return Err(AuthError::Expired)
            }
            Err(AuthError::Oauth(error)) if error == "access_denied" => {
                return Err(AuthError::Denied)
            }
            Err(other) => return Err(other),
        }
    }
}

pub fn display_available() -> bool {
    env_set("DISPLAY") || env_set("WAYLAND_DISPLAY")
}

pub fn open_system_browser(url: &str) -> Result<(), AuthError> {
    // xdg-open hands the URL to the user's browser. It is not an embedded webview.
    std::process::Command::new("xdg-open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|err| AuthError::BrowserUnavailable(err.to_string()))
}

pub fn sign_in<S: SessionStore>(
    store: &S,
    endpoints: &Endpoints,
    force_device: bool,
    has_display: bool,
    mut open_browser: impl FnMut(&str) -> Result<(), AuthError>,
    mut on_device: impl FnMut(&DeviceCodes),
    mut on_access: impl FnMut(&str) -> Result<(), AuthError>,
) -> Result<(), AuthError> {
    if force_device || !has_display {
        return device_sign_in(store, endpoints, &mut on_device, &mut on_access);
    }
    match browser_sign_in(store, endpoints, &mut open_browser, &mut on_access) {
        Err(AuthError::BrowserUnavailable(_)) => {
            device_sign_in(store, endpoints, &mut on_device, &mut on_access)
        }
        other => other,
    }
}

fn browser_sign_in<S: SessionStore>(
    store: &S,
    endpoints: &Endpoints,
    open_browser: &mut impl FnMut(&str) -> Result<(), AuthError>,
    on_access: &mut impl FnMut(&str) -> Result<(), AuthError>,
) -> Result<(), AuthError> {
    let (listener, redirect) = bind_loopback()?;
    let login = prepare_browser_login(endpoints, &redirect)?;
    open_browser(&login.authorize_url)?;
    let code = accept_callback(listener, &login.state, LOGIN_TIMEOUT)?;
    let tokens = exchange_code(endpoints, &login.redirect_uri, &code, &login.verifier)?;
    finish_session(store, &tokens, on_access)
}

fn device_sign_in<S: SessionStore>(
    store: &S,
    endpoints: &Endpoints,
    on_device: &mut impl FnMut(&DeviceCodes),
    on_access: &mut impl FnMut(&str) -> Result<(), AuthError>,
) -> Result<(), AuthError> {
    let codes = start_device(endpoints)?;
    on_device(&codes);
    let tokens = poll_device(
        endpoints,
        &codes.device_code,
        codes.interval,
        codes.expires_in,
        &mut SystemClock,
    )?;
    finish_session(store, &tokens, on_access)
}

fn finish_session<S: SessionStore>(
    store: &S,
    tokens: &Tokens,
    on_access: &mut impl FnMut(&str) -> Result<(), AuthError>,
) -> Result<(), AuthError> {
    store_refresh(store, tokens)?;
    on_access(&tokens.access_token)
}

pub fn first_tenant(api_base: &str, access_token: &str) -> Result<String, AuthError> {
    let url = format!("{}/api/v1/tenants", api_base.trim_end_matches('/'));
    let response = ureq::get(&url)
        .timeout(Duration::from_secs(30))
        .set("Authorization", &format!("Bearer {access_token}"))
        .call()
        .map_err(|_| AuthError::message("tenant lookup failed"))?;
    let json = parse_json(&read_body(response))?;
    json.get("tenants")
        .and_then(|value| value.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("id"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| AuthError::message("no tenant on the signed-in account"))
}

pub fn accept_callback(
    listener: TcpListener,
    expected_state: &str,
    timeout: Duration,
) -> Result<String, AuthError> {
    listener
        .set_nonblocking(true)
        .map_err(|err| AuthError::message(err.to_string()))?;
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() > deadline {
            return Err(AuthError::Timeout);
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let request = read_http(&mut stream).unwrap_or_default();
                let path = request_path(&request);
                if !path.starts_with("/callback") {
                    write_html(&mut stream, 404, "Not found.");
                    continue;
                }
                let outcome = callback_code(&path, expected_state);
                let (status, page) = match &outcome {
                    Ok(_) => (200, SIGNED_IN_SENTENCE.to_string()),
                    Err(AuthError::StateMismatch) => (
                        400,
                        "Hypermesh couldn't sign you in. The sign-in state did not match. You can close this tab.".into(),
                    ),
                    Err(AuthError::Denied) => (
                        400,
                        "Hypermesh couldn't sign you in. The request was declined. You can close this tab.".into(),
                    ),
                    Err(_) => (
                        400,
                        "Hypermesh couldn't sign you in. You can close this tab.".into(),
                    ),
                };
                write_html(&mut stream, status, &page);
                return outcome;
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(err) => return Err(AuthError::message(err.to_string())),
        }
    }
}

pub fn callback_code(path: &str, expected_state: &str) -> Result<String, AuthError> {
    let state = query_param(path, "state").unwrap_or_default();
    if state != expected_state {
        return Err(AuthError::StateMismatch);
    }
    if let Some(error) = query_param(path, "error").filter(|value| !value.is_empty()) {
        if error == "access_denied" {
            return Err(AuthError::Denied);
        }
        return Err(AuthError::Oauth(error));
    }
    let code = query_param(path, "code").unwrap_or_default();
    if code.is_empty() {
        return Err(AuthError::message("login redirect had no code"));
    }
    Ok(code)
}

fn post_token(endpoints: &Endpoints, fields: &[(&str, &str)]) -> Result<Tokens, AuthError> {
    debug_assert!(fields.iter().all(|(name, _)| *name != "client_secret"));
    let json = post_form(&endpoints.token_url, fields)?;
    let access_token = required_str(&json, "access_token")?;
    let refresh_token = json
        .get("refresh_token")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let expires_in = json
        .get("expires_in")
        .and_then(|value| value.as_u64())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(60));
    Ok(Tokens {
        access_token,
        refresh_token,
        expires_in,
    })
}

fn post_form(url: &str, fields: &[(&str, &str)]) -> Result<serde_json::Value, AuthError> {
    if fields.iter().any(|(name, _)| *name == "client_secret") {
        return Err(AuthError::message("public client refuses a client secret"));
    }
    match ureq::post(url)
        .timeout(Duration::from_secs(30))
        .send_form(fields)
    {
        Ok(response) => parse_json(&read_body(response)),
        Err(ureq::Error::Status(code, response)) => {
            let body = read_body(response);
            if let Some(error) = oauth_error_code(&body) {
                return Err(AuthError::Oauth(error));
            }
            Err(AuthError::message(format!(
                "token request failed with HTTP {code}"
            )))
        }
        Err(_) => Err(AuthError::message("token request failed")),
    }
}

fn read_body(response: ureq::Response) -> String {
    response.into_string().unwrap_or_default()
}

fn parse_json(body: &str) -> Result<serde_json::Value, AuthError> {
    serde_json::from_str(body).map_err(|_| AuthError::message("token response was not JSON"))
}

fn oauth_error_code(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    json.get("error")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn required_str(json: &serde_json::Value, key: &str) -> Result<String, AuthError> {
    json.get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| AuthError::message(format!("token response was missing {key}")))
}

fn read_http(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|window| window == b"\r\n\r\n") || buf.len() > 16_384 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn request_path(request: &str) -> String {
    let line = request.lines().next().unwrap_or("");
    let mut parts = line.split_whitespace();
    let _method = parts.next();
    parts.next().unwrap_or("/").to_string()
}

fn query_param(path: &str, key: &str) -> Option<String> {
    let query = path.split_once('?')?.1;
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        if name == key {
            return Some(percent_decode(value));
        }
    }
    None
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""),
                16,
            ) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        if bytes[index] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[index]);
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn encode_query(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn write_html(stream: &mut TcpStream, status: u16, message: &str) {
    let safe = html_escape(message);
    let body = format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Hypermesh</title>\
<style>body{{font-family:sans-serif;margin:0;min-height:100vh;display:grid;place-items:center;background:#f4f7f4;color:#142018}}main{{max-width:28rem;padding:2rem}}</style>\
</head><body><main><p>{safe}</p></main></body></html>"
    );
    let reason = if status == 200 { "OK" } else { "Error" };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

fn html_escape(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn env_set(name: &str) -> bool {
    std::env::var_os(name)
        .map(|value| !value.to_string_lossy().trim().is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    #[test]
    fn redirect_uri_uses_the_bound_ephemeral_port() {
        let (listener, uri) = bind_loopback().unwrap();
        let port = listener.local_addr().unwrap().port();
        assert_ne!(port, 0);
        assert_eq!(uri, format!("http://127.0.0.1:{port}/callback"));
        assert_eq!(redirect_uri(49152), "http://127.0.0.1:49152/callback");
        assert!(!uri.ends_with(":3000/callback") || port == 3000);
    }

    #[test]
    fn state_mismatch_is_rejected_and_does_not_return_the_code() {
        let err = callback_code("/callback?code=secret-code&state=other", "expected").unwrap_err();
        assert!(matches!(err, AuthError::StateMismatch));
        assert!(!err.to_string().contains("secret-code"));
        let code = callback_code("/callback?code=secret-code&state=expected", "expected").unwrap();
        assert_eq!(code, "secret-code");
    }

    #[test]
    fn loopback_serves_the_signed_in_page_once_and_rejects_a_bad_state() {
        let (listener, uri) = bind_loopback().unwrap();
        let port = listener.local_addr().unwrap().port();
        let server =
            thread::spawn(move || accept_callback(listener, "good-state", Duration::from_secs(3)));
        let mismatch = ureq::get(&format!(
            "http://127.0.0.1:{port}/callback?code=abc&state=bad"
        ))
        .call();
        let body = match mismatch {
            Ok(response) => response.into_string().unwrap_or_default(),
            Err(ureq::Error::Status(_, response)) => response.into_string().unwrap_or_default(),
            Err(err) => panic!("callback request failed: {err}"),
        };
        assert!(body.contains("state did not match"));
        assert!(!body.contains("abc"));
        let err = server.join().unwrap().unwrap_err();
        assert!(matches!(err, AuthError::StateMismatch));
        assert!(uri.contains("/callback"));
    }

    #[test]
    fn loopback_success_page_uses_the_signed_in_sentence() {
        let (listener, _) = bind_loopback().unwrap();
        let port = listener.local_addr().unwrap().port();
        let server =
            thread::spawn(move || accept_callback(listener, "good-state", Duration::from_secs(3)));
        let response = ureq::get(&format!(
            "http://127.0.0.1:{port}/callback?code=from-browser&state=good-state"
        ))
        .call()
        .unwrap();
        let body = response.into_string().unwrap();
        assert!(body.contains(SIGNED_IN_SENTENCE));
        assert_eq!(server.join().unwrap().unwrap(), "from-browser");
    }

    struct SimClock {
        now: Instant,
        slept: Vec<Duration>,
    }

    impl Clock for SimClock {
        fn now(&self) -> Instant {
            self.now
        }

        fn wait(&mut self, duration: Duration) {
            self.slept.push(duration);
            self.now += duration;
        }
    }

    #[test]
    fn device_poll_handles_pending_slow_down_and_expiry() {
        let pending = scripted_device(&[
            r#"{"error":"authorization_pending"}"#,
            r#"{"access_token":"access-1","refresh_token":"refresh-1","expires_in":30,"token_type":"Bearer"}"#,
        ]);
        let mut clock = SimClock {
            now: Instant::now(),
            slept: Vec::new(),
        };
        let tokens = poll_device(
            &pending.endpoints,
            "device-1",
            Duration::from_secs(5),
            Duration::from_secs(60),
            &mut clock,
        )
        .unwrap();
        assert_eq!(tokens.refresh_token.as_deref(), Some("refresh-1"));
        assert_eq!(
            clock.slept,
            vec![Duration::from_secs(5), Duration::from_secs(5)]
        );
        assert!(pending
            .bodies
            .lock()
            .unwrap()
            .iter()
            .all(|body| !body.contains("client_secret")));
        assert!(pending
            .bodies
            .lock()
            .unwrap()
            .iter()
            .any(|body| body.contains("authorization_pending") || body.contains("device-1")));

        let slowed = scripted_device(&[
            r#"{"error":"slow_down"}"#,
            r#"{"access_token":"access-2","refresh_token":"refresh-2","expires_in":30,"token_type":"Bearer"}"#,
        ]);
        let mut clock = SimClock {
            now: Instant::now(),
            slept: Vec::new(),
        };
        poll_device(
            &slowed.endpoints,
            "device-2",
            Duration::from_secs(5),
            Duration::from_secs(60),
            &mut clock,
        )
        .unwrap();
        assert_eq!(
            clock.slept,
            vec![Duration::from_secs(5), Duration::from_secs(10)]
        );

        let expired = scripted_device(&[r#"{"error":"expired_token"}"#]);
        let mut clock = SimClock {
            now: Instant::now(),
            slept: Vec::new(),
        };
        let err = poll_device(
            &expired.endpoints,
            "device-3",
            Duration::from_secs(5),
            Duration::from_secs(60),
            &mut clock,
        )
        .unwrap_err();
        assert!(matches!(err, AuthError::Expired));
        assert_eq!(clock.slept, vec![Duration::from_secs(5)]);
        assert!(!err.to_string().contains("device-3"));
    }

    #[test]
    fn scope_is_openid_for_the_sso_session() {
        assert_eq!(SCOPE, "openid");
        let url = authorization_url(
            &Endpoints::panopticon(),
            "http://127.0.0.1:9/callback",
            "st",
            "ch",
        );
        assert!(url.contains("scope=openid"));
        assert!(!url.contains("offline_access"));
    }

    #[test]
    fn ended_session_clears_the_keychain_entry() {
        let mock = scripted_device(&[r#"{"error":"invalid_grant"}"#]);
        let store = MemoryStore::new();
        store.put_refresh_token("refresh-live").unwrap();
        let err = refresh_session(&store, &mock.endpoints).unwrap_err();
        assert!(matches!(err, AuthError::SessionEnded));
        assert_eq!(err.to_string(), "Your sign-in ended. Sign in again.");
        assert!(!err.to_string().contains("refresh-live"));
        assert_eq!(store.refresh_token().unwrap(), None);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let down = Endpoints {
            authorize_url: format!("http://{addr}/auth"),
            token_url: format!("http://{addr}/token"),
            device_url: format!("http://{addr}/device"),
            revoke_url: format!("http://{addr}/revoke"),
            client_id: PUBLIC_CLIENT_ID.into(),
        };
        let store = MemoryStore::new();
        store.put_refresh_token("refresh-live").unwrap();
        let err = refresh_session(&store, &down).unwrap_err();
        assert!(!matches!(err, AuthError::SessionEnded));
        assert_eq!(
            store.refresh_token().unwrap().as_deref(),
            Some("refresh-live")
        );
    }

    #[test]
    fn exchange_stores_the_refresh_token_only() {
        let mock = scripted_device(&[
            r#"{"access_token":"access-value","refresh_token":"refresh-value","expires_in":45,"token_type":"Bearer"}"#,
        ]);
        let tokens = exchange_code(
            &mock.endpoints,
            "http://127.0.0.1:9/callback",
            "auth-code",
            "verifier-verifier-verifier",
        )
        .unwrap();
        let store = MemoryStore::new();
        store_refresh(&store, &tokens).unwrap();
        assert_eq!(
            store.refresh_token().unwrap().as_deref(),
            Some("refresh-value")
        );
        let stored = store.refresh_token().unwrap().unwrap();
        assert!(!stored.contains("access-value"));
        let body = mock.bodies.lock().unwrap().join("\n");
        assert!(body.contains("grant_type=authorization_code"));
        assert!(body.contains("code_verifier=verifier-verifier-verifier"));
        assert!(!body.contains("client_secret"));
        let rendered = format!("{tokens:?}");
        assert!(!rendered.contains("access-value"));
        assert!(!rendered.contains("refresh-value"));
    }

    struct Mock {
        endpoints: Endpoints,
        bodies: Arc<Mutex<Vec<String>>>,
    }

    fn scripted_device(responses: &[&'static str]) -> Mock {
        let responses: Vec<String> = responses.iter().map(|body| (*body).to_string()).collect();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&bodies);
        thread::spawn(move || {
            for body in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                stream
                    .set_read_timeout(Some(Duration::from_millis(500)))
                    .unwrap();
                let mut buf = Vec::new();
                let mut tmp = [0u8; 2048];
                loop {
                    match stream.read(&mut tmp) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&tmp[..n]);
                            if buf.windows(4).any(|window| window == b"\r\n\r\n")
                                && request_complete(&buf)
                            {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                seen.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf).into_owned());
                let status = if body.contains("\"error\"") { 400 } else { 200 };
                let resp = format!(
                    "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        let base = format!("http://{addr}");
        Mock {
            endpoints: Endpoints {
                authorize_url: format!("{base}/auth"),
                token_url: format!("{base}/token"),
                device_url: format!("{base}/device"),
                revoke_url: format!("{base}/revoke"),
                client_id: PUBLIC_CLIENT_ID.into(),
            },
            bodies,
        }
    }

    fn request_complete(buf: &[u8]) -> bool {
        let text = String::from_utf8_lossy(buf);
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            return false;
        };
        let length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if name.eq_ignore_ascii_case("content-length") {
                    value.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0);
        body.len() >= length
    }
}
