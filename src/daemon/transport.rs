//! The daemon transport's one platform-specific seam: the local stream
//! socket type.
//!
//! Everything above it (framing, JSON-RPC, timeouts, the activation dance)
//! is written against these aliases and is OS-independent. On Unix they are
//! `std`'s Unix domain sockets; on Windows they are the same AF_UNIX
//! sockets through `uds_windows`, whose API mirrors `std`'s (including
//! `set_read_timeout`/`set_write_timeout`/`set_nonblocking`, which the
//! client budget and the accept loop depend on).
//!
//! AF_UNIX on Windows is a filesystem path like on Unix, so the socket
//! naming, stale-file recovery and bind-as-lock logic are shared verbatim.
//! It requires Windows 10 1803+ / Windows Server 2019+.

#[cfg(unix)]
pub(crate) use std::os::unix::net::{UnixListener as Listener, UnixStream as Stream};

#[cfg(windows)]
pub(crate) use uds_windows::{UnixListener as Listener, UnixStream as Stream};

/// Connect to the socket at `path` without activating anything.
pub(crate) fn connect(path: &std::path::Path) -> std::io::Result<Stream> {
    Stream::connect(path)
}

/// Bind a listener to `path`.
pub(crate) fn bind(path: &std::path::Path) -> std::io::Result<Listener> {
    Listener::bind(path)
}

/// Restrict the socket to its owner where the OS has POSIX file modes.
///
/// Unix gets `0600` (the socket is in a per-user runtime dir, but the mode
/// is what makes a foreign-reader guarantee explicit). Windows has no mode
/// bits on an AF_UNIX file: access is governed by the directory's ACL, and
/// the socket lives under the user's `%LOCALAPPDATA%`, which is already
/// user-only. Nothing to do there.
pub(crate) fn restrict_to_owner(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(windows)]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use super::*;

    /// Bind → connect → one length-prefixed frame each way. This is the
    /// exact transport the daemons use, so a green run is the Windows
    /// portability check (path-based AF_UNIX, blocking reads with
    /// timeouts, a clean peer close).
    #[test]
    fn local_transport_round_trips_one_frame() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("transport.sock");
        let listener = bind(&path).expect("bind");
        restrict_to_owner(&path).expect("restrict socket");

        let client_path = path.clone();
        let client = std::thread::spawn(move || {
            let mut stream = connect(&client_path).expect("connect");
            stream.write_all(b"ping").expect("write to the listener");
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .ok();
            let mut response = [0u8; 4];
            stream.read_exact(&mut response).expect("read the reply");
            response
        });

        let (mut accepted, _addr) = listener.accept().expect("accept");
        accepted
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .ok();
        let mut request = [0u8; 4];
        accepted.read_exact(&mut request).expect("read the request");
        assert_eq!(&request, b"ping");
        accepted.write_all(b"pong").expect("write the reply");

        assert_eq!(&client.join().expect("client thread"), b"pong");
    }
}
