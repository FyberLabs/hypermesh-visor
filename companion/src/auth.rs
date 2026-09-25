//! Browser OAuth back to loopback, then a real renter API key for the CLI.
//! The access token is not written down. The visor session vault is not touched.

use std::fs::File;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};

use crate::credentials::{self, StoreError};
use crate::pages::{self, API_BASE, LOOPBACK_REDIRECT, OAUTH_CLIENT_ID, TOKEN_URL};

#[derive(Clone)]
pub struct Endpoints {
    pub token_url: String,
    pub api_base: String,
    pub client_id: String,
    pub redirect_uri: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            token_url: TOKEN_URL.into(),
            api_base: API_BASE.into(),
            client_id: OAUTH_CLIENT_ID.into(),
            redirect_uri: LOOPBACK_REDIRECT.into(),
        }
    }
}

pub struct Login {
    pub state: String,
    pub verifier: String,
    #[allow(dead_code)]
    pub challenge: String,
    pub start_url: String,
}

pub fn begin_login() -> Login {
    let verifier = URL_SAFE_NO_PAD.encode(random_bytes(32));
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = URL_SAFE_NO_PAD.encode(random_bytes(24));
    let start_url = pages::oauth_start_url(&state, &challenge);
    Login {
        state,
        verifier,
        challenge,
        start_url,
    }
}

/// Wait for the browser to hit the loopback redirect, exchange the code, mint
/// a renter API key, and store it where `hypermesh auth` reads it.
pub fn finish_login(
    endpoints: &Endpoints,
    login: &Login,
    config_dir: &std::path::Path,
    ready: impl FnOnce() -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let code = accept_code(&login.state, ready)?;
    let token = exchange_code(endpoints, &code, &login.verifier)?;
    let user_id = account_id(endpoints, &token)?;
    let tenant_id = first_tenant(endpoints, &token)?;
    let api_key = create_renter_key(endpoints, &token, &tenant_id)?;
    credentials::write_login(config_dir, &api_key, &tenant_id, &user_id)
}

fn exchange_code(endpoints: &Endpoints, code: &str, verifier: &str) -> Result<String, StoreError> {
    let response = ureq::post(&endpoints.token_url)
        .timeout(Duration::from_secs(30))
        .send_form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", endpoints.redirect_uri.as_str()),
            ("client_id", endpoints.client_id.as_str()),
            ("code_verifier", verifier),
        ])
        .map_err(|err| StoreError(format!("token endpoint failed: {err}")))?;
    let body = response
        .into_string()
        .map_err(|err| StoreError(err.to_string()))?;
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|err| StoreError(err.to_string()))?;
    json.get("access_token")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| StoreError("token endpoint returned no access_token".into()))
}

fn account_id(endpoints: &Endpoints, token: &str) -> Result<String, StoreError> {
    let url = format!(
        "{}/api/v1/account/me",
        endpoints.api_base.trim_end_matches('/')
    );
    let json = get_json(&url, token, None)?;
    Ok(json
        .get("id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string())
}

fn first_tenant(endpoints: &Endpoints, token: &str) -> Result<String, StoreError> {
    let url = format!(
        "{}/api/v1/tenants",
        endpoints.api_base.trim_end_matches('/')
    );
    let json = get_json(&url, token, None)?;
    let id = json
        .get("tenants")
        .and_then(|value| value.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("id"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    id.ok_or_else(|| StoreError("no tenant on the signed-in account".into()))
}

fn create_renter_key(
    endpoints: &Endpoints,
    token: &str,
    tenant_id: &str,
) -> Result<String, StoreError> {
    let url = format!(
        "{}/api/v1/api-keys",
        endpoints.api_base.trim_end_matches('/')
    );
    let response = ureq::post(&url)
        .timeout(Duration::from_secs(30))
        .set("Authorization", &format!("Bearer {token}"))
        .set("X-Tenant-ID", tenant_id)
        .set("content-type", "application/json")
        .send_string(r#"{"name":"hypermesh-desktop","purpose":"renter"}"#)
        .map_err(|err| StoreError(format!("api key create failed: {err}")))?;
    let body = response
        .into_string()
        .map_err(|err| StoreError(err.to_string()))?;
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|err| StoreError(err.to_string()))?;
    let key = json
        .get("api_key")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            json.get("secret_plaintext")
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
        })
        .map(str::to_string);
    key.ok_or_else(|| StoreError("api key create returned no key".into()))
}

fn get_json(
    url: &str,
    token: &str,
    tenant: Option<&str>,
) -> Result<serde_json::Value, StoreError> {
    let mut request = ureq::get(url)
        .timeout(Duration::from_secs(30))
        .set("Authorization", &format!("Bearer {token}"));
    if let Some(tenant) = tenant {
        request = request.set("X-Tenant-ID", tenant);
    }
    let response = request
        .call()
        .map_err(|err| StoreError(format!("GET {url} failed: {err}")))?;
    let body = response
        .into_string()
        .map_err(|err| StoreError(err.to_string()))?;
    serde_json::from_str(&body).map_err(|err| StoreError(err.to_string()))
}

fn accept_code(
    expected_state: &str,
    ready: impl FnOnce() -> Result<(), StoreError>,
) -> Result<String, StoreError> {
    let listener = TcpListener::bind("127.0.0.1:3000")
        .map_err(|err| StoreError(format!("login needs 127.0.0.1:3000 free ({err})")))?;
    listener
        .set_nonblocking(true)
        .map_err(|err| StoreError(err.to_string()))?;
    ready()?;
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        if Instant::now() > deadline {
            return Err(StoreError("login timed out waiting for the browser".into()));
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let request = read_http(&mut stream).unwrap_or_default();
                let path = request_path(&request);
                if path.starts_with("/callback") {
                    let state = query_param(&path, "state").unwrap_or_default();
                    let code = query_param(&path, "code").unwrap_or_default();
                    let err = query_param(&path, "error").unwrap_or_default();
                    write_html(
                        &mut stream,
                        if code.is_empty() { 400 } else { 200 },
                        if code.is_empty() {
                            "Hypermesh login did not return a code. You can close this tab."
                        } else {
                            "Hypermesh login finished. You can close this tab."
                        },
                    );
                    if !err.is_empty() {
                        return Err(StoreError(format!("login was refused ({err})")));
                    }
                    if state != expected_state {
                        return Err(StoreError("login state did not match".into()));
                    }
                    if code.is_empty() {
                        return Err(StoreError("login redirect had no code".into()));
                    }
                    return Ok(code);
                }
                write_html(&mut stream, 404, "Not found.");
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(err) => return Err(StoreError(err.to_string())),
        }
    }
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
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 16_384 {
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
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn write_html(stream: &mut TcpStream, status: u16, message: &str) {
    let body = format!("<!doctype html><html><body><p>{message}</p></body></html>");
    let reason = if status == 200 { "OK" } else { "Error" };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    let mut file = File::open("/dev/urandom").expect("urandom");
    file.read_exact(&mut buf).expect("urandom");
    buf
}

/// The visor poll is a GET of session purpose and verb. It does not send a key.
pub fn visor_request(host: &str) -> String {
    format!("GET /companion HTTP/1.1\r\nhost: {host}\r\nconnection: close\r\n\r\n")
}

pub fn pose_from_companion(body: &str) -> Pose {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return Pose::Idle;
    };
    if value.get("open").and_then(|open| open.as_bool()) != Some(true) {
        return Pose::Idle;
    }
    let Some(session) = value.get("session").filter(|session| !session.is_null()) else {
        return Pose::Idle;
    };
    let purpose = session
        .get("purpose")
        .and_then(|purpose| purpose.as_str())
        .unwrap_or("")
        .to_string();
    let verb = session
        .get("verb")
        .and_then(|verb| verb.as_str())
        .map(str::to_string);
    Pose::Active { purpose, verb }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pose {
    Idle,
    Active {
        purpose: String,
        verb: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn visor_poll_does_not_carry_a_key() {
        let request = visor_request("127.0.0.1:9847");
        assert!(request.starts_with("GET /companion "));
        assert!(!request.to_ascii_lowercase().contains("api_key"));
        assert!(!request.contains("authorization"));
    }

    #[test]
    fn idle_without_a_session() {
        assert_eq!(pose_from_companion(r#"{"open":false}"#), Pose::Idle);
        match pose_from_companion(
            r#"{"open":true,"session":{"id":"x","purpose":"review the desktop","verb":"view"}}"#,
        ) {
            Pose::Active { purpose, verb } => {
                assert_eq!(purpose, "review the desktop");
                assert_eq!(verb.as_deref(), Some("view"));
            }
            Pose::Idle => panic!("expected active"),
        }
    }

    #[test]
    fn exchanges_a_real_key_and_does_not_store_the_access_token() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let server = std::thread::spawn(move || scripted(listener));
        let dir = std::env::temp_dir().join(format!("hypermesh-oauth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let endpoints = Endpoints {
            token_url: format!("{base}/token"),
            api_base: base,
            client_id: OAUTH_CLIENT_ID.into(),
            redirect_uri: LOOPBACK_REDIRECT.into(),
        };
        let login = Login {
            state: "state".into(),
            verifier: "verifier-verifier-verifier-verifier".into(),
            challenge: "unused".into(),
            start_url: String::new(),
        };
        // Drive finish_login's HTTP half without the loopback listener.
        let token = exchange_code(&endpoints, "authcode", &login.verifier).unwrap();
        assert_eq!(token, "header.payload.sig");
        let user = account_id(&endpoints, &token).unwrap();
        let tenant = first_tenant(&endpoints, &token).unwrap();
        let key = create_renter_key(&endpoints, &token, &tenant).unwrap();
        credentials::write_login(&dir, &key, &tenant, &user).unwrap();
        let cred = std::fs::read_to_string(credentials::credentials_path(&dir)).unwrap();
        assert_eq!(cred, "api_key = \"abcd1234.secretvalue\"\n");
        assert!(!cred.contains("header.payload.sig"));
        let config = std::fs::read_to_string(credentials::config_path(&dir)).unwrap();
        assert!(!config.contains("header.payload.sig"));
        assert!(config.contains(&tenant));
        let seen = server.join().unwrap();
        assert!(seen.iter().any(|req| req.contains("grant_type=authorization_code")));
        assert!(seen.iter().any(|req| req.contains("code=authcode")));
        assert!(seen
            .iter()
            .any(|req| req.contains("GET /api/v1/account/me")));
        assert!(seen.iter().any(|req| req.contains("GET /api/v1/tenants")));
        assert!(seen.iter().any(|req| req.contains("POST /api/v1/api-keys")));
        assert!(seen.iter().any(|req| req.contains("\"purpose\":\"renter\"")));
        assert!(seen
            .iter()
            .any(|req| req.contains("X-Tenant-ID: 22222222-2222-2222-2222-222222222222")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn scripted(listener: TcpListener) -> Vec<String> {
        let mut seen = Vec::new();
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_millis(300)))
                .unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 2048];
            loop {
                match stream.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.len() > 16_384 {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let req = String::from_utf8_lossy(&buf).into_owned();
            let body = if req.starts_with("POST /token") {
                r#"{"access_token":"header.payload.sig","token_type":"Bearer"}"#
            } else if req.starts_with("GET /api/v1/account/me") {
                r#"{"id":"11111111-1111-1111-1111-111111111111","email":"a@b.c"}"#
            } else if req.starts_with("GET /api/v1/tenants") {
                r#"{"tenants":[{"id":"22222222-2222-2222-2222-222222222222","name":"t","is_active":true}],"skip":0,"limit":10,"total":1,"has_previous":false,"has_next":false}"#
            } else if req.starts_with("POST /api/v1/api-keys") {
                r#"{"id":"33333333-3333-3333-3333-333333333333","tenant_id":"22222222-2222-2222-2222-222222222222","api_key":"abcd1234.secretvalue","secret_plaintext":"abcd1234.secretvalue","purpose":"renter"}"#
            } else {
                r#"{"error":"unexpected"}"#
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(resp.as_bytes()).unwrap();
            seen.push(req);
        }
        seen
    }
}
