//! Kernel-authenticated peer identity for local Unix channels.
use std::io;
use std::os::fd::AsRawFd;

#[cfg(target_os = "linux")]
pub fn uid(fd: &impl AsRawFd) -> io::Result<u32> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if len as usize != std::mem::size_of::<libc::ucred>() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "incomplete peer credentials",
        ));
    }
    Ok(cred.uid)
}

#[cfg(target_os = "macos")]
pub fn uid(fd: &impl AsRawFd) -> io::Result<u32> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // getpeereid reads the kernel-cached credentials from connect/listen time.
    let result = unsafe { libc::getpeereid(fd.as_raw_fd(), &mut uid, &mut gid) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

#[cfg(target_os = "linux")]
pub const SUN_PATH_MAX: usize = 108;
#[cfg(target_os = "macos")]
pub const SUN_PATH_MAX: usize = 104;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authenticates_kernel_peer_and_rejects_regular_file() {
        let (left, right) = std::os::unix::net::UnixStream::pair().unwrap();
        assert_eq!(uid(&left).unwrap(), unsafe { libc::geteuid() });
        assert_eq!(uid(&right).unwrap(), unsafe { libc::geteuid() });
        assert!(uid(&std::fs::File::open("Cargo.toml").unwrap()).is_err());
    }
}
