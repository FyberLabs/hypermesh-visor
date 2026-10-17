//! Loopback poll of `GET /companion` and the automation control posts.
//! The companion does not send a key. A few missed polls stay on the last
//! snapshot so one slow visor call does not flash the disconnected state.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::auth::{self, Snapshot};
use crate::automation::{Link, DISCONNECT_MISSES};

/// Visor poll and control posts. Both run off the X11 loop: a slow reply
/// must not freeze the cursor overlay or hold up a pause.
pub struct Poller {
    snapshot: Arc<Mutex<Snapshot>>,
    queue: Arc<Mutex<VecDeque<&'static str>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Poller {
    pub fn new() -> Self {
        let snapshot = Arc::new(Mutex::new(Snapshot::disconnected()));
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let snapshot_thread = Arc::clone(&snapshot);
        let queue_thread = Arc::clone(&queue);
        let stop_thread = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("hypermesh-companion-visor".into())
            .spawn(move || visor_loop(snapshot_thread, queue_thread, stop_thread))
            .ok();
        Self {
            snapshot,
            queue,
            stop,
            thread,
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().unwrap().clone()
    }

    pub fn enqueue(&self, action: &'static str) {
        self.queue.lock().unwrap().push_back(action);
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn visor_loop(
    snapshot: Arc<Mutex<Snapshot>>,
    queue: Arc<Mutex<VecDeque<&'static str>>>,
    stop: Arc<AtomicBool>,
) {
    let mut link = Link::new(Snapshot::disconnected());
    while !stop.load(Ordering::SeqCst) {
        let batch = {
            let mut queued = queue.lock().unwrap();
            queued.drain(..).collect::<Vec<_>>()
        };
        if let Some(failed) = post_batch(&batch) {
            let mut queued = queue.lock().unwrap();
            restore_failed_posts(&mut queued, &batch[failed..]);
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        match fetch() {
            Some(next) => link.ok(next),
            None => link.miss(disconnect_from(link.current())),
        }
        *snapshot.lock().unwrap() = link.current().clone();
        for _ in 0..4 {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Posts in order. The index of the first failure is returned so that action
/// and everything after it stay queued ahead of anything enqueued meanwhile.
fn post_batch(batch: &[&str]) -> Option<usize> {
    post_each(batch, post)
}

fn post_each<'a>(batch: &[&'a str], mut post: impl FnMut(&'a str) -> bool) -> Option<usize> {
    for (index, action) in batch.iter().enumerate() {
        if !post(action) {
            return Some(index);
        }
    }
    None
}

/// A failed post goes back to the front. Newer actions stay behind it.
fn restore_failed_posts(queue: &mut VecDeque<&'static str>, failed: &[&'static str]) {
    let mut restored: VecDeque<&'static str> = failed.iter().copied().collect();
    restored.append(queue);
    *queue = restored;
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

fn post(action: &str) -> bool {
    let (host, addr) = endpoint();
    let body = format!(r#"{{"action":"{action}"}}"#);
    let request = format!(
        "POST /companion/automation HTTP/1.1\r\nhost: {host}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    post_raw(&addr, &request)
}

fn post_raw(addr: &str, request: &str) -> bool {
    let Some(text) = exchange(addr, request) else {
        return false;
    };
    status_ok(&text)
}

fn exchange(addr: &str, request: &str) -> Option<String> {
    let socket = lookup(addr)?;
    let mut stream = TcpStream::connect_timeout(&socket, Duration::from_millis(200)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(300)))
        .ok()?;
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

/// A failed poll keeps the last upgrading notice. A later successful poll is
/// what clears it, including a body with no notice.
fn disconnect_from(previous: &Snapshot) -> Snapshot {
    let mut down = Snapshot::disconnected();
    down.upgrading = previous.upgrading;
    down
}

fn lookup(host: &str) -> Option<std::net::SocketAddr> {
    host.to_socket_addrs().ok()?.next()
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
        ("127.0.0.1:9847".to_string(), "127.0.0.1:9847".to_string())
    } else {
        (host.to_string(), host.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::automation::Link;

    #[test]
    fn hostname_resolves_to_a_socket() {
        let addr = lookup("localhost:9847").expect("localhost");
        assert_eq!(addr.port(), 9847);
        assert!(addr.ip().is_loopback());
        assert_eq!(lookup("127.0.0.1:9847").map(|addr| addr.port()), Some(9847));
    }

    #[test]
    fn missed_polls_keep_the_upgrading_notice() {
        let mut up = Snapshot::disconnected();
        up.connected = true;
        up.upgrading = true;
        let mut link = Link::new(Snapshot::disconnected());
        link.ok(up);
        link.miss(disconnect_from(link.current()));
        link.miss(disconnect_from(link.current()));
        assert!(link.current().upgrading);
        link.miss(disconnect_from(link.current()));
        assert!(link.disconnected());
        assert!(link.current().upgrading);
        let mut cleared = Snapshot::disconnected();
        cleared.connected = true;
        link.ok(cleared);
        assert!(!link.current().upgrading);
    }

    #[test]
    fn a_failed_pause_is_posted_before_the_next_poll() {
        let mut sent = Vec::new();
        assert_eq!(
            post_each(&["pause", "resume"], |action| {
                sent.push(action);
                true
            }),
            None
        );
        assert_eq!(sent, ["pause", "resume"]);
        assert_eq!(
            post_each(&["pause", "resume"], |action| action == "pause"),
            Some(1)
        );
        let mut queue = VecDeque::from(["resume"]);
        restore_failed_posts(&mut queue, &["pause"]);
        assert_eq!(queue.into_iter().collect::<Vec<_>>(), ["pause", "resume"]);
    }
}
