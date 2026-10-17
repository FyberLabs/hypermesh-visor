//! Loopback poll of `GET /companion` and the automation control posts.
//! The companion does not send a key. A few missed polls stay on the last
//! snapshot so one slow visor call does not flash the disconnected state.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::auth::{self, Snapshot};
use crate::automation::{Link, DISCONNECT_MISSES};

pub struct Poller {
    link: Link<Snapshot>,
}

impl Poller {
    pub fn new() -> Self {
        Self {
            link: Link::new(Snapshot::disconnected()),
        }
    }

    pub fn snapshot(&self) -> &Snapshot {
        self.link.current()
    }

    pub fn poll(&mut self) {
        match fetch() {
            Some(snapshot) => self.link.ok(snapshot),
            None => self.link.miss(Snapshot::disconnected()),
        }
    }

    pub fn post(&self, action: &str) -> bool {
        let (host, addr) = endpoint();
        let body = format!(r#"{{"action":"{action}"}}"#);
        let request = format!(
            "POST /companion/automation HTTP/1.1\r\nhost: {host}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        post_raw(&addr, &request)
    }
}

pub fn disconnect_misses() -> u32 {
    DISCONNECT_MISSES
}

fn fetch() -> Option<Snapshot> {
    let (host, addr) = endpoint();
    let request = auth::visor_request(&host);
    let text = exchange(&addr, &request)?;
    let body = body_of(&text)?;
    if !status_ok(&text) {
        return None;
    }
    auth::snapshot_from_body(body)
}

fn post_raw(addr: &str, request: &str) -> bool {
    let Some(text) = exchange(addr, request) else {
        return false;
    };
    status_ok(&text)
}

fn exchange(addr: &str, request: &str) -> Option<String> {
    let socket: std::net::SocketAddr = addr.parse().ok()?;
    let mut stream = TcpStream::connect_timeout(&socket, Duration::from_millis(200)).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(300))).ok()?;
    stream.write_all(request.as_bytes()).ok()?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.len() > 256 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    String::from_utf8(buf).ok()
}

fn body_of(text: &str) -> Option<&str> {
    text.split_once("\r\n\r\n").map(|(_, body)| body)
}

fn status_ok(text: &str) -> bool {
    text.lines()
        .next()
        .is_some_and(|line| line.contains(" 200 ") || line.contains(" 204 "))
}

fn endpoint() -> (String, String) {
    let raw = std::env::var("HYPERMESH_VISOR_URL").unwrap_or_default();
    let raw = raw.trim().trim_end_matches('/');
    let host = raw
        .strip_prefix("https://")
        .or_else(|| raw.strip_prefix("http://"))
        .unwrap_or(raw);
    let host = host.split('/').next().unwrap_or("");
    if host.is_empty() {
        (
            "127.0.0.1:9847".to_string(),
            "127.0.0.1:9847".to_string(),
        )
    } else {
        (host.to_string(), host.to_string())
    }
}
