//! The desktop bug speaks the visor prompt stream.
//!
//! One prompt, the `X-Api-Key` header, and `model` only when the caller set it.
//! The key is checked before a socket is opened. It is not written to the reply.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const FORBIDDEN_RENTER_PREFIXES: &[&str] = &["hm_dev_", "hm_rtr_", "hm_site_"];

pub struct PromptCall {
    pub visor: String,
    pub session: String,
    pub prompt: String,
    pub model: Option<String>,
}

pub fn validate_key(key: &str) -> Result<&str, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("api key is required".into());
    }
    for prefix in FORBIDDEN_RENTER_PREFIXES {
        if key.starts_with(prefix) {
            return Err(format!(
                "{prefix} is not a renter identity; use an org API key (purpose: renter)"
            ));
        }
    }
    Ok(key)
}

/// Builds the same NDJSON prompt the CLI posts to `POST /session/{id}/stream`.
pub fn prompt_line(prompt: &str, model: Option<&str>) -> Result<String, String> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err("prompt is required".into());
    }
    let mut value = serde_json::json!({
        "kind": "prompt",
        "prompt": prompt,
    });
    if let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) {
        value["model"] = serde_json::Value::String(model.to_string());
    }
    let mut line =
        serde_json::to_string(&value).map_err(|_| "prompt could not be encoded".to_string())?;
    line.push('\n');
    Ok(line)
}

pub fn stream_request(call: &PromptCall, api_key: &str) -> Result<(String, String), String> {
    let api_key = validate_key(api_key)?;
    let (host, path) = stream_target(&call.visor, &call.session)?;
    let body = prompt_line(&call.prompt, call.model.as_deref())?;
    let request = format!(
        "POST {path} HTTP/1.1\r\nhost: {host}\r\ncontent-type: application/x-ndjson\r\naccept: application/x-ndjson\r\nx-api-key: {api_key}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    Ok((host, request))
}

/// Sends one prompt. A rejected key returns before any connection.
pub fn send_prompt(call: &PromptCall, api_key: &str) -> Result<String, String> {
    let api_key = validate_key(api_key)?.to_string();
    let (host, request) = stream_request(call, &api_key)?;
    let mut addr = host
        .to_socket_addrs()
        .map_err(|_| "visor stream request failed".to_string())?;
    let Some(addr) = addr.next() else {
        return Err("visor stream request failed".into());
    };
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .map_err(|_| "visor stream request failed".to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|_| "visor stream request failed".to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|_| "visor stream request failed".to_string())?;
    stream
        .write_all(request.as_bytes())
        .map_err(|_| "visor stream request failed".to_string())?;
    let mut buf = Vec::new();
    stream
        .take((1 << 20) as u64)
        .read_to_end(&mut buf)
        .map_err(|_| "visor stream request failed".to_string())?;
    let text = String::from_utf8_lossy(&buf);
    let text = redact(&text, &api_key);
    let (status, body) = split_response(&text);
    if !(200..300).contains(&status) {
        let message = if body.is_empty() {
            format!("visor stream HTTP {status}")
        } else {
            format!("visor stream HTTP {status}: {body}")
        };
        return Err(redact(&message, &api_key));
    }
    Ok(body)
}

fn stream_target(visor: &str, session: &str) -> Result<(String, String), String> {
    let session = session.trim();
    if session.is_empty() || session.contains(['/', '\\', '?', '#', ' ']) {
        return Err("session id is required".into());
    }
    let visor = visor.trim();
    let rest = visor
        .strip_prefix("https://")
        .or_else(|| visor.strip_prefix("http://"))
        .ok_or_else(|| "visor url must be an http(s) URL".to_string())?;
    if rest.is_empty() || rest.contains(['?', '#', '@', ' ']) {
        return Err("visor url must be an http(s) URL".into());
    }
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    if authority.is_empty() || path != "/" {
        return Err("visor url must be an http(s) URL".into());
    }
    Ok((authority.to_string(), format!("/session/{session}/stream")))
}

fn redact(text: &str, key: &str) -> String {
    let key = key.trim();
    if key.is_empty() {
        return text.to_string();
    }
    text.replace(key, "***")
}

fn split_response(text: &str) -> (u16, String) {
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return (0, String::new());
    };
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    (status, body.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    const FIXTURE_KEY: &str = "org_fixture_ok";

    fn call(visor: &str) -> PromptCall {
        PromptCall {
            visor: visor.into(),
            session: "11111111-1111-1111-1111-111111111111".into(),
            prompt: "count the sheep".into(),
            model: None,
        }
    }

    #[test]
    fn rejected_key_does_not_open_a_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let err = send_prompt(&call(&format!("http://{addr}")), "hm_dev_fixture_tail").unwrap_err();
        assert!(err.contains("hm_dev_"));
        assert!(!err.contains("fixture_tail"));
        assert!(!err.contains(FIXTURE_KEY));
        let accepted = listener.accept();
        assert!(accepted.is_err());
    }

    #[test]
    fn prompt_omits_model_until_the_caller_sets_it() {
        let (_, request) = stream_request(&call("http://127.0.0.1:9"), FIXTURE_KEY).unwrap();
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let value: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(value["kind"], "prompt");
        assert_eq!(value["prompt"], "count the sheep");
        assert!(value.get("model").is_none());
        assert!(request.contains("x-api-key: org_fixture_ok\r\n"));
        assert!(request
            .starts_with("POST /session/11111111-1111-1111-1111-111111111111/stream HTTP/1.1\r\n"));
        assert!(!request.to_ascii_lowercase().contains("x-lease-id"));
        assert!(!request.to_ascii_lowercase().contains("authorization"));

        let mut with_model = call("http://127.0.0.1:9");
        with_model.model = Some("caller-picked".into());
        let (_, request) = stream_request(&with_model, FIXTURE_KEY).unwrap();
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let value: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(value["model"], "caller-picked");
    }

    #[test]
    fn fixture_key_sends_one_prompt_and_the_reply_hides_the_key() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 2048];
            loop {
                let n = stream.read(&mut tmp).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|window| window == b"\r\n\r\n") {
                    let text = String::from_utf8_lossy(&buf).into_owned();
                    let length = text
                        .lines()
                        .find_map(|line| {
                            let lower = line.to_ascii_lowercase();
                            let rest = lower.strip_prefix("content-length:")?;
                            rest.trim().parse::<usize>().ok()
                        })
                        .unwrap_or(0);
                    let have = text
                        .split("\r\n\r\n")
                        .nth(1)
                        .map(|body| body.len())
                        .unwrap_or(0);
                    if have >= length {
                        break;
                    }
                }
            }
            let reply = b"HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ncontent-length: 33\r\nconnection: close\r\n\r\n{\"kind\":\"prompt\",\"accepted\":true}\n";
            stream.write_all(reply).unwrap();
            String::from_utf8(buf).unwrap()
        });
        let reply = send_prompt(&call(&format!("http://{addr}")), FIXTURE_KEY).unwrap();
        assert_eq!(reply, "{\"kind\":\"prompt\",\"accepted\":true}");
        assert!(!reply.contains(FIXTURE_KEY));
        let request = seen.join().unwrap();
        assert!(request.contains("x-api-key: org_fixture_ok"));
        assert!(request.contains("\"prompt\":\"count the sheep\""));
        assert!(!request.contains("\"model\""));
        assert!(!request.to_ascii_lowercase().contains("x-lease-id"));
    }
}
