//! Poll of `GET /companion` and the automation control posts.
//! Both go over the visor's Unix socket, and only after `SO_PEERCRED` says
//! the peer is this user. Nothing is written to a peer that fails that check.
//! The companion does not send a key. A few missed polls stay on the last
//! snapshot so one slow visor call does not flash the disconnected state.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::auth::{self, Snapshot};
use crate::automation::{Link, VisorPhase, DISCONNECT_MISSES};

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
    /// Set when a queued resume must not be restored after a failed post.
    resume_dropped: Arc<AtomicBool>,
    /// Set until the keyboard-unavailable report is accepted.
    keyboard_report: Arc<AtomicBool>,
    /// `0` omit, `1` keyboard present, `2` keyboard missing. Sent on every poll.
    keyboard_level: Arc<AtomicU8>,
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
        let resume_dropped = Arc::new(AtomicBool::new(false));
        let keyboard_report = Arc::new(AtomicBool::new(false));
        let keyboard_level = Arc::new(AtomicU8::new(0));
        let snapshot_thread = Arc::clone(&snapshot);
        let queue_thread = Arc::clone(&queue);
        let stop_thread = Arc::clone(&stop);
        let dropped_thread = Arc::clone(&resume_dropped);
        let keyboard_thread = Arc::clone(&keyboard_report);
        let level_thread = Arc::clone(&keyboard_level);
        let thread = thread::Builder::new()
            .name("hypermesh-companion-visor".into())
            .spawn(move || {
                visor_loop(
                    snapshot_thread,
                    queue_thread,
                    stop_thread,
                    dropped_thread,
                    keyboard_thread,
                    level_thread,
                )
            })
            .ok();
        Self {
            snapshot,
            queue,
            stop,
            resume_dropped,
            keyboard_report,
            keyboard_level,
            thread,
        }
    }

    pub fn snapshot(&self) -> (Snapshot, u64) {
        let cache = self.snapshot.lock().unwrap();
        (cache.snapshot.clone(), cache.epoch)
    }

    pub fn enqueue(&self, action: &'static str) {
        if action == "resume" {
            self.resume_dropped.store(false, Ordering::SeqCst);
        }
        self.queue.lock().unwrap().push_back(action);
    }

    /// Drop a resume that has not been confirmed. A failed post must not put it back.
    pub fn cancel_resume(&self) {
        self.resume_dropped.store(true, Ordering::SeqCst);
        self.queue
            .lock()
            .unwrap()
            .retain(|action| *action != "resume");
    }

    /// Ask the visor to refuse automation until it is restarted.
    pub fn report_keyboard_unavailable(&self) {
        self.keyboard_report.store(true, Ordering::SeqCst);
    }

    /// Level sent on the next poll. `true` is present, `false` is missing.
    pub fn set_keyboard_available(&self, available: bool) {
        self.keyboard_level
            .store(if available { 1 } else { 2 }, Ordering::SeqCst);
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
    resume_dropped: Arc<AtomicBool>,
    keyboard_report: Arc<AtomicBool>,
    keyboard_level: Arc<AtomicU8>,
) {
    let mut link = Link::new(Snapshot::disconnected());
    let mut pause_asked = false;
    while !stop.load(Ordering::SeqCst) {
        if keyboard_report.load(Ordering::SeqCst) && post_interrupt() {
            keyboard_report.store(false, Ordering::SeqCst);
        }
        let batch = {
            let mut queued = queue.lock().unwrap();
            queued.drain(..).collect::<Vec<_>>()
        };
        if let Some(failed) = post_batch(&batch) {
            let mut queued = queue.lock().unwrap();
            restore_after_cancel(
                &mut queued,
                &batch[failed..],
                resume_dropped.load(Ordering::SeqCst),
            );
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let fetched = fetch(keyboard_header(keyboard_level.load(Ordering::SeqCst)));
        let fresh = {
            let mut queued = queue.lock().unwrap();
            apply_poll(&mut link, &mut queued, &mut pause_asked, fetched)
        };
        let mut cache = snapshot.lock().unwrap();
        note_poll(&mut cache.epoch, fresh);
        cache.snapshot = link.current().clone();
        let sleep_ms = poll_sleep_ms(cache.snapshot.phase, cache.snapshot.connected);
        drop(cache);
        let slices = (sleep_ms / 10).max(1);
        for _ in 0..slices {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

/// 40 ms while an automation is running or paused, so a physical input is
/// noticed quickly. 250 ms when nothing is driving.
pub fn poll_sleep_ms(phase: VisorPhase, connected: bool) -> u64 {
    if connected && matches!(phase, VisorPhase::Running | VisorPhase::Paused) {
        40
    } else {
        250
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
fn restore_after_cancel(
    queue: &mut VecDeque<&'static str>,
    failed: &[&'static str],
    resume_dropped: bool,
) {
    if !resume_dropped {
        restore_failed_posts(queue, failed);
        return;
    }
    let kept: Vec<&'static str> = failed
        .iter()
        .copied()
        .filter(|action| *action != "resume")
        .collect();
    restore_failed_posts(queue, &kept);
}

fn restore_failed_posts(queue: &mut VecDeque<&'static str>, failed: &[&'static str]) {
    let mut restored: VecDeque<&'static str> = failed.iter().copied().collect();
    restored.append(queue);
    *queue = restored;
}

pub fn disconnect_misses() -> u32 {
    DISCONNECT_MISSES
}

/// The companion stops reading a poll here. A larger body is not a snapshot.
const POLL_READ_MAX: usize = 256 * 1024;

enum Exchange {
    Body(String),
    /// The read hit [`POLL_READ_MAX`] or was not valid UTF-8.
    Truncated,
    /// No connection, or the peer was not this user.
    Failed,
}

enum PollFetch {
    Ready(Snapshot),
    /// The visor did not answer, or it answered with something other than 200.
    Down,
    /// A 200 that was cut off or did not parse. Automation may still be running.
    Unreadable,
}

fn keyboard_header(level: u8) -> Option<bool> {
    match level {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

fn fetch(keyboard: Option<bool>) -> PollFetch {
    let request = auth::visor_request_with_keyboard("hypermesh", keyboard);
    match exchange(&request) {
        Exchange::Failed => PollFetch::Down,
        Exchange::Truncated => PollFetch::Unreadable,
        Exchange::Body(text) => classify_poll(false, &text),
    }
}

/// `truncated` wins even when the bytes already on hand would parse.
fn classify_poll(truncated: bool, text: &str) -> PollFetch {
    if truncated {
        return PollFetch::Unreadable;
    }
    if text.is_empty() || !status_ok(text) {
        return PollFetch::Down;
    }
    let Some(body) = body_of(text) else {
        return PollFetch::Unreadable;
    };
    match auth::snapshot_from_body(body) {
        Some(snapshot) => PollFetch::Ready(snapshot),
        None => PollFetch::Unreadable,
    }
}

/// A poll that cannot be read while automation is running or paused queues
/// one `pause` and keeps the last snapshot. It does not walk toward Off.
/// `fresh` is true only for a parsed snapshot, so the poll epoch stays put.
fn apply_poll(
    link: &mut Link<Snapshot>,
    queue: &mut VecDeque<&'static str>,
    pause_asked: &mut bool,
    fetched: PollFetch,
) -> bool {
    match fetched {
        PollFetch::Ready(next) => {
            *pause_asked = false;
            link.ok(next);
            true
        }
        PollFetch::Down => {
            let down = disconnect_from(link.current());
            link.miss(down);
            false
        }
        PollFetch::Unreadable => {
            let current = link.current().clone();
            if current.connected
                && matches!(current.phase, VisorPhase::Running | VisorPhase::Paused)
            {
                if !*pause_asked && !queue.iter().any(|action| *action == "pause") {
                    queue.push_back("pause");
                }
                *pause_asked = true;
                false
            } else {
                let down = disconnect_from(&current);
                link.miss(down);
                false
            }
        }
    }
}

fn post(action: &str) -> bool {
    let request = automation_post(action);
    match exchange(&request) {
        Exchange::Body(text) => status_ok(&text),
        _ => false,
    }
}

fn post_interrupt() -> bool {
    let body = r#"{"available":false}"#;
    let request = format!(
        "POST /companion/interrupt HTTP/1.1\r\nhost: hypermesh\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    match exchange(&request) {
        Exchange::Body(text) => status_ok(&text),
        _ => false,
    }
}

fn automation_post(action: &str) -> String {
    let body = format!(r#"{{"action":"{action}"}}"#);
    format!(
        "POST /companion/automation HTTP/1.1\r\nhost: hypermesh\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn exchange(request: &str) -> Exchange {
    let Some(path) = socket_path() else {
        return Exchange::Failed;
    };
    exchange_read(path.as_path(), request)
}

fn exchange_at(path: &Path, request: &str) -> Option<String> {
    match exchange_read(path, request) {
        Exchange::Body(text) => Some(text),
        _ => None,
    }
}

pub fn trust_request(
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    if !matches!(path, "/mcp/trust" | "/mcp/discovery" | "/mcp/sources" | "/mcp/profiles" | "/mcp/servers") {
        return Err("Invalid trust endpoint".into());
    }
    let method = if body.is_some() { "POST" } else { "GET" };
    let body = body.map(|v| v.to_string()).unwrap_or_default();
    let request = format!("{method} {path} HTTP/1.1\r\nhost: hypermesh\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",body.len());
    let socket = socket_path().ok_or("Companion socket unavailable")?;
    let Exchange::Body(response) = exchange_read_timeout(&socket, &request, Duration::from_secs(5))
    else {
        return Err("Cannot reach visor; no trust change confirmed".into());
    };
    if !status_ok(&response) {
        let message = body_of(&response)
            .and_then(|body| serde_json::from_str::<serde_json::Value>(body).ok())
            .and_then(|value| value.get("error").and_then(|v| v.as_str()).map(str::to_string))
            .unwrap_or_else(|| "Review rejected; review again".into());
        let message: String = message.chars().take(200).flat_map(char::escape_default).collect();
        return Err(format!("MCP review failed: {message}"));
    }
    let body = body_of(&response).ok_or("Invalid visor response")?;
    if body.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(body).map_err(|_| "Invalid or oversized visor response".into())
}

fn exchange_read(path: &Path, request: &str) -> Exchange {
    exchange_read_timeout(path, request, Duration::from_millis(300))
}
fn exchange_read_timeout(path: &Path, request: &str, timeout: Duration) -> Exchange {
    let mut stream = match UnixStream::connect(path) {
        Ok(stream) => stream,
        Err(_) => return Exchange::Failed,
    };
    if stream
        .set_read_timeout(Some(timeout))
        .is_err()
        || stream
            .set_write_timeout(Some(Duration::from_millis(200)))
            .is_err()
    {
        return Exchange::Failed;
    }
    let peer = peer_uid(&stream);
    let mut outgoing = None;
    if !release_to_peer(peer, current_uid(), || {
        outgoing = Some(request.to_string());
    }) {
        note_foreign_peer();
        return Exchange::Failed;
    }
    let Some(outgoing) = outgoing else {
        return Exchange::Failed;
    };
    if stream.write_all(outgoing.as_bytes()).is_err() {
        return Exchange::Failed;
    }
    read_capped(&mut stream)
}

fn read_capped(stream: &mut UnixStream) -> Exchange {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.len() > POLL_READ_MAX {
                    return Exchange::Truncated;
                }
            }
            Err(_) => break,
        }
    }
    match String::from_utf8(buf) {
        Ok(text) => Exchange::Body(text),
        Err(_) => Exchange::Truncated,
    }
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

/// Same choice as the visor's `runtime_dir_from`. `sun_path` is 108 bytes
/// including the trailing NUL.
pub fn runtime_dir_from(
    override_dir: Option<&str>,
    xdg_runtime: Option<&str>,
    xdg_state: Option<&str>,
    home: Option<&str>,
) -> Result<PathBuf, &'static str> {
    let dir = if let Some(dir) = nonempty(override_dir) {
        PathBuf::from(dir)
    } else if let Some(dir) = nonempty(xdg_runtime) {
        PathBuf::from(dir).join("hypermesh")
    } else if let Some(dir) = nonempty(xdg_state) {
        PathBuf::from(dir).join("hypermesh").join("run")
    } else if let Some(dir) = nonempty(home) {
        PathBuf::from(dir)
            .join(".local")
            .join("state")
            .join("hypermesh")
            .join("run")
    } else {
        return Err(
            "companion socket needs HYPERMESH_RUNTIME_DIR, XDG_RUNTIME_DIR, XDG_STATE_HOME, or HOME",
        );
    };
    let socket = dir.join("companion.sock");
    if socket.as_os_str().len() >= 108 {
        return Err("companion socket path does not fit in sun_path");
    }
    Ok(dir)
}

pub fn control_socket_path() -> Result<PathBuf, &'static str> {
    let dir = runtime_dir_from(
        std::env::var("HYPERMESH_RUNTIME_DIR").ok().as_deref(),
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        std::env::var("XDG_STATE_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )?;
    Ok(dir.join("control.sock"))
}

fn socket_path() -> Option<PathBuf> {
    match runtime_dir_from(
        std::env::var("HYPERMESH_RUNTIME_DIR").ok().as_deref(),
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        std::env::var("XDG_STATE_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    ) {
        Ok(dir) => Some(dir.join("companion.sock")),
        Err(err) => {
            note_socket_path(err);
            None
        }
    }
}

fn note_socket_path(err: &str) {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if LOGGED.swap(true, Ordering::SeqCst) {
        return;
    }
    eprintln!("hypermesh-companion: {err}");
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
    use crate::automation::{Link, VisorPhase};

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
            runtime_dir_from(Some("/tmp/hm"), Some("/run/user/7"), None, None).unwrap(),
            PathBuf::from("/tmp/hm")
        );
        assert_eq!(
            runtime_dir_from(None, Some("/run/user/7"), Some("/state"), Some("/home/u")).unwrap(),
            PathBuf::from("/run/user/7/hypermesh")
        );
        assert_eq!(
            runtime_dir_from(None, None, Some("/state"), Some("/home/u")).unwrap(),
            PathBuf::from("/state/hypermesh/run")
        );
        assert_eq!(
            runtime_dir_from(None, None, None, Some("/home/u")).unwrap(),
            PathBuf::from("/home/u/.local/state/hypermesh/run")
        );
        assert!(runtime_dir_from(None, None, None, None).is_err());
        assert!(runtime_dir_from(Some(&"a".repeat(120)), None, None, None).is_err());
    }

    #[test]
    fn polls_slow_down_when_nothing_is_driving() {
        assert_eq!(poll_sleep_ms(VisorPhase::Running, true), 40);
        assert_eq!(poll_sleep_ms(VisorPhase::Paused, true), 40);
        assert_eq!(poll_sleep_ms(VisorPhase::Off, true), 250);
        assert_eq!(poll_sleep_ms(VisorPhase::Running, false), 250);
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

    fn running_body() -> &'static str {
        r#"HTTP/1.1 200 OK\r\ncontent-length: 1\r\n\r\n{"open":true,"session":{"purpose":"drive"},"automation":{"phase":"running","resume_seq":3}}"#
    }

    #[test]
    fn a_truncated_or_unparsed_poll_is_unreadable_and_a_miss_is_down() {
        assert!(matches!(
            classify_poll(true, running_body()),
            PollFetch::Unreadable
        ));
        assert!(matches!(
            classify_poll(false, "HTTP/1.1 200 OK\r\n\r\nnot-json"),
            PollFetch::Unreadable
        ));
        assert!(matches!(
            classify_poll(false, "HTTP/1.1 200 OK\r\n\r\n"),
            PollFetch::Unreadable
        ));
        assert!(matches!(
            classify_poll(false, "HTTP/1.1 500 ERR\r\n\r\nnope"),
            PollFetch::Down
        ));
        assert!(matches!(classify_poll(false, ""), PollFetch::Down));
        assert!(matches!(
            classify_poll(
                false,
                "HTTP/1.1 200 OK\r\n\r\n{\"open\":false,\"automation\":{\"phase\":\"off\"}}"
            ),
            PollFetch::Ready(_)
        ));
    }

    #[test]
    fn an_unreadable_poll_while_running_posts_one_pause() {
        let snap = crate::auth::snapshot_from_body(
            r#"{"open":true,"session":{"purpose":"drive"},"automation":{"phase":"running","resume_seq":3}}"#,
        )
        .unwrap();
        let mut link = Link::new(Snapshot::disconnected());
        link.ok(snap);
        let mut queue = VecDeque::new();
        let mut asked = false;
        assert!(!apply_poll(
            &mut link,
            &mut queue,
            &mut asked,
            PollFetch::Unreadable
        ));
        assert_eq!(queue.iter().copied().collect::<Vec<_>>(), ["pause"]);
        assert!(link.current().connected);
        assert_eq!(link.current().phase, VisorPhase::Running);
        queue.clear();
        assert!(!apply_poll(
            &mut link,
            &mut queue,
            &mut asked,
            PollFetch::Unreadable
        ));
        assert!(queue.is_empty());
        assert!(link.current().connected);

        let paused = crate::auth::snapshot_from_body(
            r#"{"open":true,"automation":{"phase":"paused","resume_seq":3}}"#,
        )
        .unwrap();
        assert!(apply_poll(
            &mut link,
            &mut queue,
            &mut asked,
            PollFetch::Ready(paused)
        ));
        assert!(!asked);
        assert!(!apply_poll(
            &mut link,
            &mut queue,
            &mut asked,
            PollFetch::Unreadable
        ));
        assert_eq!(queue.iter().copied().collect::<Vec<_>>(), ["pause"]);
        assert_eq!(link.current().phase, VisorPhase::Paused);
    }

    #[test]
    fn an_unreadable_poll_while_off_counts_as_a_miss() {
        let mut off = Snapshot::disconnected();
        off.connected = true;
        let mut link = Link::new(Snapshot::disconnected());
        link.ok(off);
        let mut queue = VecDeque::new();
        let mut asked = false;
        for _ in 0..disconnect_misses() {
            assert!(!apply_poll(
                &mut link,
                &mut queue,
                &mut asked,
                PollFetch::Unreadable
            ));
        }
        assert!(queue.is_empty());
        assert!(link.disconnected());
        assert!(!link.current().connected);
    }

    #[test]
    fn a_cancelled_resume_is_not_restored_after_a_failed_post() {
        let mut queue = VecDeque::from(["pause"]);
        restore_after_cancel(&mut queue, &["resume", "pause"], true);
        assert_eq!(
            queue.into_iter().collect::<Vec<_>>(),
            vec!["pause", "pause"]
        );
        let mut queue = VecDeque::new();
        restore_after_cancel(&mut queue, &["resume"], false);
        assert_eq!(queue.into_iter().collect::<Vec<_>>(), vec!["resume"]);
    }
}
