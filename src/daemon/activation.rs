//! Daemon lifecycle: socket activation, the bind-as-lock, idle shutdown.
//!
//! **Socket activation** — the client-side flow that makes the daemon exist
//! only when it is needed:
//!
//! 1. try to connect to the daemon's socket;
//! 2. on failure, spawn the daemon binary (`current_exe()` of the running
//!    app, so the daemon always ships with the app — no PATH lookup, no
//!    install step) and poll until the socket appears or the activation
//!    window closes.
//!
//! Two morpheus processes racing into step 2 resolve by **the bind being the
//! lock**: the loser's `bind` gets `EADDRINUSE` and falls through to
//! connecting, which is the only correct outcome for both.
//!
//! **Idle shutdown** — the daemon-side counterpart: when no client activity
//! happened for the idle budget, the serve loop returns and the process
//! exits. Nothing is orphaned, nothing runs when the feature is unused, and
//! the next request pays one activation instead of a resident process
//! nobody is talking to.
//!
//! **Client liveness** — the idle budget alone leaves the daemon resident for
//! up to [`IDLE`] after the last client exits, and on Windows a resident
//! daemon holds `morpheus-ttsd.exe` open (a `cargo clean`/rebuild gets
//! "access denied"). Each `hello` carries the client pid, so the serve loop
//! can exit as soon as none of the registered clients is alive; the idle
//! budget remains the backstop for a daemon whose client never greeted.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::ipc_error::Lifecycle;
use super::transport::{self, Stream};

/// The runtime socket directory: `$XDG_RUNTIME_DIR/morpheus/daemon`, falling
/// back to what `directories` computes when the runtime dir is not set
/// (some containers drop it, and Windows has no runtime dir at all).
///
/// Under `XDG_RUNTIME_DIR` the path is tmpfs, user-owned and wiped at
/// logout — the right home for a per-user socket with `0600` perms. The
/// fallback is the LOCAL data dir, which is the closest Windows equivalent:
/// per-user, not roaming, and its inherited ACL is user-only.
pub(crate) fn socket_dir() -> PathBuf {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        return Path::new(&runtime).join("morpheus").join("daemon");
    }
    directories::ProjectDirs::from("", "", "morpheus")
        .map(|dirs| {
            dirs.runtime_dir().map_or_else(
                || dirs.data_local_dir().join("daemon"),
                |runtime| runtime.join("morpheus").join("daemon"),
            )
        })
        .unwrap_or_else(|| std::env::temp_dir().join("morpheus-daemons"))
}

/// The socket path for a daemon named `name` (e.g. `decision`).
pub(crate) fn socket_path(name: &str) -> PathBuf {
    socket_dir().join(format!("{name}.sock"))
}

/// Whether process `pid` is still alive.
///
/// The daemons register the pid of every process that greets them and exit
/// when none is left, so a daemon spawned by a `morpheus` process does not
/// outlive it (on Windows it holds the daemon `.exe` open the whole time).
/// A pid the OS refuses to query is reported dead: the worst case is one
/// extra activation, never a stranded process.
pub(crate) fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        if pid == 0 || pid > i32::MAX as u32 {
            return false;
        }
        // Signal 0 performs the existence/permission checks and delivers
        // nothing.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(windows)]
    {
        if pid == 0 {
            return false;
        }
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};

        // `PROCESS_SYNCHRONIZE` from the Windows SDK; `windows-sys` does not
        // re-export the `SYNCHRONIZE` alias where the other constants live.
        const PROCESS_SYNCHRONIZE: u32 = 0x0010_0000;

        // A synchronizable handle: `OpenProcess` fails for a dead pid, and
        // the wait is satisfied only once the process exits.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if handle.is_null() {
            return false;
        }
        let state = unsafe { WaitForSingleObject(handle, 0) };
        let _ = unsafe { CloseHandle(handle) };
        state == WAIT_TIMEOUT
    }
    #[cfg(not(any(unix, windows)))]
    {
        // No portable probe here; reporting dead only costs a reactivation.
        let _ = pid;
        false
    }
}

/// Connect to `name`'s socket, activating (spawning) the daemon on demand.
///
/// `activate` spawns the process; keeping it a closure keeps this module
/// protocol-agnostic (the decision daemon passes its own spawn details).
pub(crate) fn connect_or_activate(
    name: &'static str,
    activate: impl FnOnce() -> Result<(), Lifecycle>,
) -> Result<Stream, Lifecycle> {
    let path = socket_path(name);
    if let Ok(stream) = transport::connect(&path) {
        return Ok(stream);
    }

    // Nothing is listening: spawn, then poll for the socket. The spawn
    // loses races the same way it wins them — a second binder gets
    // EADDRINUSE and its socket file is simply the winner's.
    activate()?;
    let deadline = std::time::Instant::now() + ACTIVATION_WINDOW;
    let mut delay = Duration::from_millis(25);
    loop {
        std::thread::sleep(delay);
        delay = (delay * 2).min(ACTIVATION_BACKOFF_CAP);
        // Every connect failure inside the window is retried (the socket
        // may not exist yet, or exist before its listener does); the
        // deadline is the single authority on giving up.
        match transport::connect(&path) {
            Ok(stream) => return Ok(stream),
            Err(_) if std::time::Instant::now() >= deadline => {
                return Err(Lifecycle::Timeout {
                    name,
                    path: path.display().to_string(),
                });
            }
            Err(_) => continue,
        }
    }
}

/// Total wait for an activating daemon to become reachable. Model loads run
/// on first `decide`, not at startup, so activation only waits for a socket
/// bind — fast even on a cold machine.
const ACTIVATION_WINDOW: Duration = Duration::from_secs(2);
const ACTIVATION_BACKOFF_CAP: Duration = Duration::from_millis(250);

/// The activation lock: an exclusive `flock` on `<name>.lock`, held for the
/// daemon's whole lifetime.
///
/// The review found the bind's probe→remove→bind sequence racy (M1): two
/// processes could both probe a stale socket, both conclude "dead", and the
/// loser would then unlink the winner's LIVE socket and bind its own — two
/// live daemons, the path pointing at only one. `flock` closes the window:
/// the probe/remove/bind critical section runs under the lock, and the
/// winner HOLDS the lock for its lifetime, so any later binder finds the
/// lock held and knows a live daemon owns the path — it exits 0 as the
/// loser instead of ever touching the socket file.
///
/// The lock file persists when a daemon crashes (locks do not — the OS
/// releases them on process death), which is exactly the property a pidfile
/// lacks.
///
/// `None` when the lock cannot even be created (read-only dir): the caller
/// then falls back to the racy path rather than refusing to run.
pub(crate) enum ActivationLock {
    /// We hold the lock: safe to probe/remove/bind the socket. The handle is
    /// `None` only in the fail-open fallback above.
    Held(Option<std::fs::File>),
    /// Someone else holds it: a live daemon owns the socket path.
    Contended,
}

pub(crate) fn acquire_activation_lock(name: &str) -> ActivationLock {
    let path = socket_dir().join(format!("{name}.lock"));
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) => {
            log::warn!("activation lock unavailable at {}: {e}", path.display());
            // Racy fallback rather than refusal — fail-open, like the rest
            // of the layer.
            return ActivationLock::Held(None);
        }
    };
    if try_lock(&file) {
        ActivationLock::Held(Some(file))
    } else {
        ActivationLock::Contended
    }
}

/// Take the lock without blocking. The lock is process-associated and
/// released by the OS on process death, so there is no unlock path to
/// maintain.
#[cfg(unix)]
fn try_lock(file: &std::fs::File) -> bool {
    use std::os::fd::AsRawFd;
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

/// The Windows `flock` equivalent: an exclusive lock over the whole file,
/// failing immediately instead of waiting. `File` handles are not opened
/// with `FILE_FLAG_OVERLAPPED`, so the `OVERLAPPED` offset only selects the
/// byte range and needs nothing else.
#[cfg(windows)]
fn try_lock(file: &std::fs::File) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    let mut overlapped = windows_sys::Win32::System::IO::OVERLAPPED::default();
    let held = unsafe {
        LockFileEx(
            file.as_raw_handle() as _,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    held != 0
}

/// Spawn the daemon binary next to the running app.
///
/// The daemon shares the app binary's install location by construction
/// (`current_exe()`), so there is exactly one artifact to install and the
/// spawned daemon is always the version the app was built with. It
/// detaches into its own process group with stdio null: it must survive
/// the spawning app (that is the point — other instances share it) and
/// never write to the app's terminal.
pub(crate) fn spawn_sibling(name: &'static str) -> Result<(), Lifecycle> {
    // The real daemon name in both error paths (m4) — "cannot spawn the
    // daemon daemon" was the old hardcoded-report bug.
    let exe = std::env::current_exe().map_err(|e| Lifecycle::Spawn { name, source: e })?;
    let daemon_exe = exe.with_file_name(format!("morpheus-{name}d"));
    let mut command = std::process::Command::new(&daemon_exe);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    // Detach from the spawning app's process group: the daemon outlives
    // that app and must not receive its terminal signals.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // No console of its own (a double-clicked GUI launch must not flash
        // a console window) and a new process group so a Ctrl+C aimed at the
        // spawning console never reaches the resident daemon.
        command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }

    command
        .spawn()
        .map_err(|e| Lifecycle::Spawn { name, source: e })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_is_named_under_the_daemon_dir() {
        // Shape only: the dir depends on the environment, the file name
        // must not drift from the `<name>.sock` convention the activation
        // flow and the server both derive from the daemon name.
        let path = socket_path("decision");
        assert!(path.ends_with("decision.sock"));
        assert!(path.starts_with(socket_dir()));
    }

    /// The daemon's client liveness check must be true for a running
    /// process and false once it exited; a false positive means a stranded
    /// daemon holding the `.exe`, a false negative kills a working one.
    #[test]
    fn a_process_is_alive_only_while_it_runs() {
        assert!(is_process_alive(std::process::id()));

        #[cfg(windows)]
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit"])
            .spawn()
            .expect("spawn cmd");
        #[cfg(unix)]
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");

        let pid = child.id();
        assert!(is_process_alive(pid));
        child.wait().expect("wait child");
        assert!(!is_process_alive(pid));
    }
}
