//! Loopback poll of `GET /companion` and the automation control posts.
//! Both go over the visor's Unix socket, and only after `SO_PEERCRED` says
//! the peer is this user. Nothing is written to a peer that fails that check.
//! The companion does not send a key. A few missed polls stay on the last
//! snapshot so one slow visor call does not flash the disconnected state.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::auth::{self, Snapshot};
use crate::automation::{Link, DISCONNECT_MISSES};

/// Visor poll and control posts. Both run off the X11 loop: a slow reply
/// must not freeze the cursor overlay or hold up a pause.
struct Cache {
    snapshot: Snapshot,
    /// Bumps on each successful poll. A repeated snapshot from a missed poll
    /// keeps the old epoch, so it cannot confirm a pointer mismatch.
    epoch: u64,
}

pub struct Poller {
    snapshot: Arc<Mutex<Cache>>,
    queue: Arc<Mutex<VecDeque<&'static str>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Poller {
    pub fn new() -> Self {
        let snapshot = Arc::new(Mutex::new(Cache {
            snapshot: Snapshot::disconnected(),
            epoch: 0,
        }));
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

    pub fn snapshot(&self) -> (Snapshot, u64) {
        let cache = self.snapshot.lock().unwrap();
        (cache.snapshot.clone(), cache.epoch)
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
    snapshot: Arc<Mutex<Cache>>,
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
        let fresh = match fetch() {
            Some(next) => {
                link.ok(next);
                true
            }
            None => {
                link.miss(disconnect_from(link.current()));
                false
            }
        };
        let mut cache = snapshot.lock().unwrap();
        note_poll(&mut cache.epoch, fresh);
        cache.snapshot = link.current().clone();
        drop(cache);
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
    let request = auth::visor_request("hypermesh");
    let text = exchange(&request)?;
    let body = body_of(&text)?;
    if !status_ok(&text) {
        return None;
    }
    auth::snapshot_from_body(body)
}

fn post(action: &str) -> bool {
    let request = automation_post(action);
    let Some(text) = exchange(&request) else {
        return false;
    };
    status_ok(&text)
}

fn automation_post(action: &str) -> String {
    let body = format!(r#"{{"action":"{action}"}}"#);
    format!(
        "POST /companion/automation HTTP/1.1\r\nhost: hypermesh\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn exchange(request: &str) -> Option<String> {
    exchange_at(&socket_path(), request)
}

fn exchange_at(path: &Path, request: &str) -> Option<String> {
    let mut stream = UnixStream::connect(path).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(300)))
        .ok()?;
    stream
        .set_write_timeout(Some(Duration::from_millis(200)))
        .ok()?;
    let peer = peer_uid(&stream);
    let mut outgoing = None;
    if !release_to_peer(peer, current_uid(), || {
        outgoing = Some(request.to_string());
    }) {
        note_foreign_peer();
        return None;
    }
    stream.write_all(outgoing?.as_bytes()).ok()?;
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

/// Copy the request only when the peer uid is ours. A missing credential
/// writes nothing.
fn release_to_peer(peer: Option<u32>, ours: u32, mut write: impl FnMut()) -> bool {
    match peer {
        Some(uid) if uid == ours => {
            write();
            true
        }
        _ => false,
    }
}

fn note_foreign_peer() {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if LOGGED.swap(true, Ordering::SeqCst) {
        return;
    }
    eprintln!("hypermesh-companion: companion socket peer is not this user, so nothing was sent");
}

fn peer_uid(fd: &impl AsRawFd) -> Option<u32> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 {
        None
    } else {
        Some(cred.uid)
    }
}

fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

pub fn runtime_dir_from(
    override_dir: Option<&str>,
    xdg: Option<&str>,
    uid: u32,
    temp: &Path,
) -> PathBuf {
    if let Some(dir) = nonempty(override_dir) {
        return PathBuf::from(dir);
    }
    if let Some(dir) = nonempty(xdg) {
        return PathBuf::from(dir).join("hypermesh");
    }
    temp.join(format!("hypermesh-{uid}"))
}

fn socket_path() -> PathBuf {
    runtime_dir_from(
        std::env::var("HYPERMESH_RUNTIME_DIR").ok().as_deref(),
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        current_uid(),
        &std::env::temp_dir(),
    )
    .join("companion.sock")
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
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

fn note_poll(epoch: &mut u64, fresh: bool) {
    if fresh {
        *epoch = epoch.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::automation::Link;

    #[test]
    fn a_missed_poll_does_not_confirm_a_pointer_mismatch() {
        let mut epoch = 4;
        note_poll(&mut epoch, false);
        assert_eq!(epoch, 4);
        note_poll(&mut epoch, true);
        assert_eq!(epoch, 5);
    }

    #[test]
    fn the_socket_path_follows_the_runtime_dir() {
        assert_eq!(
            runtime_dir_from(Some("/tmp/hm"), Some("/run/user/7"), 9, Path::new("/tmp")),
            PathBuf::from("/tmp/hm")
        );
        assert_eq!(
            runtime_dir_from(None, Some("/run/user/7"), 9, Path::new("/tmp")),
            PathBuf::from("/run/user/7/hypermesh")
        );
        assert_eq!(
            runtime_dir_from(None, None, 9, Path::new("/tmp")),
            PathBuf::from("/tmp/hypermesh-9")
        );
    }

    #[test]
    fn a_foreign_peer_is_not_sent_the_request() {
        let mut wrote = false;
        assert!(!release_to_peer(Some(1), 2, || wrote = true));
        assert!(!wrote);
        assert!(!release_to_peer(None, 2, || wrote = true));
        assert!(!wrote);
        assert!(release_to_peer(Some(2), 2, || wrote = true));
        assert!(wrote);
    }

    #[test]
    fn a_same_user_socket_is_written_only_after_the_peer_check() {
        let dir = std::env::temp_dir().join(format!(
            "hypermesh-companion-peer-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("companion.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 256];
            loop {
                match stream.read(&mut tmp) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let text = String::from_utf8_lossy(&buf);
            assert!(text.contains("POST /companion/automation"));
            assert!(!text.contains("x-hypermesh-companion"));
            stream
                .write_all(
                    b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let text = exchange_at(&path, &automation_post("resume")).unwrap();
        assert!(status_ok(&text));
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn control_posts_carry_no_bearer_token() {
        let resume = automation_post("resume");
        assert!(resume.contains("\"action\":\"resume\""));
        assert!(!resume.contains("x-hypermesh-companion"));
        assert!(resume.find("\r\n\r\n").unwrap() < resume.find("\"action\"").unwrap());
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
