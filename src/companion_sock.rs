//! Companion channel on a Unix socket.
//!
//! The directory is `$XDG_RUNTIME_DIR/hypermesh` (mode 0700) and the socket is
//! `companion.sock` (mode 0600). `HYPERMESH_RUNTIME_DIR` replaces the directory.
//! When `XDG_RUNTIME_DIR` is unset the directory is
//! `$XDG_STATE_HOME/hypermesh/run`, or `$HOME/.local/state/hypermesh/run`.
//! Both ends check `SO_PEERCRED` and continue only when the peer uid is this
//! user. The loopback TCP API cannot lift a user pause. The companion copy of
//! the path choice in `hypermesh-companion` must stay the same.

use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use tokio::net::{UnixListener, UnixStream};
use tower::ServiceExt;

use crate::api::{companion_router, AppState};

pub const SOCKET_NAME: &str = "companion.sock";

/// `sun_path` holds 108 bytes including the trailing NUL.
const SUN_PATH_MAX: usize = 108;

pub fn runtime_dir() -> io::Result<PathBuf> {
    runtime_dir_from(
        std::env::var("HYPERMESH_RUNTIME_DIR").ok().as_deref(),
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        std::env::var("XDG_STATE_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
    .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))
}

/// `HYPERMESH_RUNTIME_DIR`, then `$XDG_RUNTIME_DIR/hypermesh`, then
/// `$XDG_STATE_HOME/hypermesh/run`, then `$HOME/.local/state/hypermesh/run`.
/// The socket path has to fit in [`SUN_PATH_MAX`].
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
    let socket = dir.join(SOCKET_NAME);
    if socket.as_os_str().len() >= SUN_PATH_MAX {
        return Err("companion socket path does not fit in sun_path");
    }
    Ok(dir)
}

pub fn socket_file(dir: &Path) -> PathBuf {
    dir.join(SOCKET_NAME)
}

/// Owner is this user and group/other have no bits.
pub fn access_is_private(owner: u32, ours: u32, mode: u32) -> bool {
    owner == ours && mode & 0o077 == 0
}

pub fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

pub fn peer_uid(fd: &impl AsRawFd) -> io::Result<u32> {
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
        return Err(io::Error::last_os_error());
    }
    Ok(cred.uid)
}

pub fn peer_is_current_user(fd: &impl AsRawFd) -> bool {
    match peer_uid(fd) {
        Ok(uid) => uid == current_uid(),
        Err(_) => false,
    }
}

/// A peer that is not this user is dropped before any request bytes are read.
pub fn admit(peer: Option<u32>, ours: u32) -> bool {
    peer == Some(ours)
}

pub fn prepare_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    enforce_private(dir, 0o700)
}

/// True when a process is accepting connections on this socket. A leftover
/// file from a crashed visor does not count.
pub fn unix_socket_listening(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

/// Held for the visor's lifetime. Dropping the file releases the lock.
#[derive(Debug)]
pub struct RuntimeLock {
    _file: fs::File,
}

/// Exclusive non-blocking lock on `companion.lock` in the runtime directory.
/// A second visor fails with `visor already running` before it can replace
/// the socket.
pub fn lock_runtime(dir: &Path) -> io::Result<RuntimeLock> {
    let path = dir.join("companion.lock");
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&path)?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let err = io::Error::last_os_error();
        let code = err.raw_os_error();
        let blocked = code == Some(libc::EAGAIN) || code == Some(libc::EWOULDBLOCK);
        if blocked {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "visor already running",
            ));
        }
        return Err(err);
    }
    let _ = enforce_private(&path, 0o600);
    Ok(RuntimeLock { _file: file })
}

pub async fn bind(dir: &Path) -> io::Result<UnixListener> {
    let path = socket_file(dir);
    if unix_socket_listening(&path) {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "visor already running",
        ));
    }
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let listener = UnixListener::bind(&path)?;
    enforce_private(&path, 0o600)?;
    Ok(listener)
}

/// `EMFILE` and its cousins are temporary. A dead listener is not.
pub fn accept_is_transient(err: &io::Error) -> bool {
    const CODES: &[i32] = &[
        libc::EMFILE,
        libc::ENFILE,
        libc::ENOBUFS,
        libc::ENOMEM,
        libc::EINTR,
        libc::ECONNABORTED,
        libc::EAGAIN,
    ];
    if let Some(code) = err.raw_os_error() {
        if CODES.contains(&code) || code == libc::EWOULDBLOCK {
            return true;
        }
    }
    matches!(
        err.kind(),
        io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::TimedOut
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::OutOfMemory
    )
}

pub async fn accept_loop(listener: UnixListener, state: Arc<AppState>) -> io::Result<()> {
    let app = companion_router(state);
    let mut pause = Duration::from_millis(10);
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                pause = Duration::from_millis(10);
                if !admit(peer_uid(&stream).ok(), current_uid()) {
                    eprintln!(
                        "hypermesh-visor: refused a companion socket peer that is not this user"
                    );
                    continue;
                }
                let app = app.clone();
                tokio::spawn(async move {
                    serve_one(stream, app).await;
                });
            }
            Err(err) if accept_is_transient(&err) => {
                eprintln!("hypermesh-visor: companion accept: {err}");
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(Duration::from_secs(1));
            }
            Err(err) => return Err(err),
        }
    }
}

async fn serve_one(stream: UnixStream, app: axum::Router) {
    let io = TokioIo::new(stream);
    let service = hyper::service::service_fn(move |request: hyper::Request<Incoming>| {
        let app = app.clone();
        async move {
            let request = request.map(Body::new);
            let response = app
                .oneshot(request)
                .await
                .unwrap_or_else(|err| match err {});
            Ok::<_, std::convert::Infallible>(response)
        }
    });
    let _ = Builder::new(TokioExecutor::new())
        .http1_only()
        .serve_connection(io, service)
        .await;
}

fn enforce_private(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path)?;
    let ours = current_uid();
    if meta.uid() != ours {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "companion socket path is not owned by this user",
        ));
    }
    let mut perms = meta.permissions();
    perms.set_mode(mode);
    fs::set_permissions(path, perms)?;
    let meta = fs::metadata(path)?;
    let now = meta.permissions().mode() & 0o777;
    if !access_is_private(meta.uid(), ours, now) || now != mode {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "companion socket path is not private",
        ));
    }
    Ok(())
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_dir_prefers_the_override_then_xdg_then_state() {
        assert_eq!(
            runtime_dir_from(Some("/tmp/hm"), Some("/run/user/7"), None, None).unwrap(),
            PathBuf::from("/tmp/hm")
        );
        assert_eq!(
            runtime_dir_from(
                Some("  "),
                Some("/run/user/7"),
                Some("/state"),
                Some("/home/u")
            )
            .unwrap(),
            PathBuf::from("/run/user/7/hypermesh")
        );
        assert_eq!(
            runtime_dir_from(None, None, Some("/state"), Some("/home/u")).unwrap(),
            PathBuf::from("/state/hypermesh/run")
        );
        assert_eq!(
            runtime_dir_from(None, None, Some(" "), Some("/home/u")).unwrap(),
            PathBuf::from("/home/u/.local/state/hypermesh/run")
        );
        assert!(runtime_dir_from(None, None, None, None).is_err());
        let long = "a".repeat(120);
        assert!(runtime_dir_from(Some(&long), None, None, None).is_err());
    }

    #[test]
    fn a_foreign_or_unreadable_peer_is_refused() {
        assert!(admit(Some(10), 10));
        assert!(!admit(Some(0), 10));
        assert!(!admit(None, 10));
        assert!(access_is_private(10, 10, 0o700));
        assert!(access_is_private(10, 10, 0o600));
        assert!(!access_is_private(0, 10, 0o600));
        assert!(!access_is_private(10, 10, 0o644));
        assert!(!access_is_private(10, 10, 0o640));
    }

    #[test]
    fn an_emfile_accept_is_retried_and_a_dead_listener_is_not() {
        assert!(accept_is_transient(&io::Error::from_raw_os_error(
            libc::EMFILE
        )));
        assert!(accept_is_transient(&io::Error::from_raw_os_error(
            libc::ENFILE
        )));
        assert!(accept_is_transient(&io::Error::from_raw_os_error(
            libc::EINTR
        )));
        assert!(accept_is_transient(&io::Error::from_raw_os_error(
            libc::EAGAIN
        )));
        assert!(accept_is_transient(&io::Error::from_raw_os_error(
            libc::ENOMEM
        )));
        assert!(accept_is_transient(&io::Error::from_raw_os_error(
            libc::ECONNABORTED
        )));
        assert!(accept_is_transient(&io::Error::new(
            io::ErrorKind::WouldBlock,
            "again"
        )));
        assert!(!accept_is_transient(&io::Error::from_raw_os_error(
            libc::EBADF
        )));
        assert!(!accept_is_transient(&io::Error::new(
            io::ErrorKind::NotFound,
            "gone"
        )));
    }

    #[tokio::test]
    async fn the_socket_dir_and_file_stay_private() {
        let dir = std::env::temp_dir().join(format!(
            "hypermesh-companion-sock-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = fs::remove_dir_all(&dir);
        prepare_dir(&dir).unwrap();
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let listener = bind(&dir).await.unwrap();
        let file_mode = fs::metadata(socket_file(&dir))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
        drop(listener);
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn bind_refuses_a_live_socket_without_replacing_it() {
        let dir = std::env::temp_dir().join(format!(
            "hypermesh-companion-live-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = fs::remove_dir_all(&dir);
        prepare_dir(&dir).unwrap();
        let path = socket_file(&dir);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let err = bind(&dir).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        assert!(err.to_string().contains("visor already running"));
        assert!(unix_socket_listening(&path));
        drop(listener);
        assert!(!unix_socket_listening(&path));
        let replaced = bind(&dir).await.unwrap();
        drop(replaced);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_visor_cannot_take_the_runtime_lock() {
        let dir = std::env::temp_dir().join(format!(
            "hypermesh-visor-lock-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = fs::remove_dir_all(&dir);
        prepare_dir(&dir).unwrap();
        let first = lock_runtime(&dir).unwrap();
        let err = lock_runtime(&dir).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        assert!(err.to_string().contains("visor already running"));
        drop(first);
        let second = lock_runtime(&dir).unwrap();
        drop(second);
        let _ = fs::remove_dir_all(&dir);
    }
}
