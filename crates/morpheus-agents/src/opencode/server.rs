//! Lifecycle of the `opencode serve` child process.
//!
//! One [`Server`] owns one headless OpenCode server:
//!
//! 1. picks a free loopback port,
//! 2. spawns `opencode serve --hostname 127.0.0.1 --port <port>` in the
//!    workspace directory (the server roots its project there),
//! 3. waits for `GET /global/health` to answer before any turn uses it,
//! 4. kills the child when the last handle is dropped.
//!
//! The server publishes an OpenAPI-described HTTP API; this module only owns
//! the process, never the protocol - see [`super::http`] and [`super::api`].

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::http::Http;

/// How long the server may take to answer `/global/health` before the start
/// is considered failed. The CLI boots a Bun server; a couple of seconds is
/// normal even on a cold cache, so the budget is generous.
const START_TIMEOUT: Duration = Duration::from_secs(30);

/// Poll interval while waiting for health.
const HEALTH_POLL: Duration = Duration::from_millis(150);

/// How many stderr lines are retained for a startup failure message.
const STDERR_TAIL: usize = 40;

/// A running OpenCode server.
pub struct Server {
    backend: super::Backend,
    http: Http,
    child: Mutex<Option<Child>>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    cwd: PathBuf,
    ready: AtomicBool,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("opencode::Server")
            .field("backend", &self.backend)
            .field("base", &self.http.base())
            .field("cwd", &self.cwd)
            .field("ready", &self.ready.load(Ordering::Relaxed))
            .finish()
    }
}

impl Server {
    /// Spawn a server rooted at `cwd` and return it immediately.
    ///
    /// Readiness is NOT awaited here (this is callable from the synchronous
    /// session-open path); [`Self::ensure_ready`] performs the health wait
    /// before the first request.
    ///
    /// # Errors
    ///
    /// Returns an error when the `opencode` binary cannot be spawned or no
    /// loopback port can be reserved.
    pub fn start(cwd: &Path, backend: super::Backend) -> Result<Arc<Self>, String> {
        let port = free_loopback_port()?;
        let auth = server_auth(backend);
        let instance_header = backend
            .instance_header()
            .map(|name| (name.to_string(), cwd.display().to_string()));
        let mut words: Vec<String> = vec![
            backend.name().to_string(),
            "serve".to_string(),
            "--hostname".to_string(),
            "127.0.0.1".to_string(),
            "--port".to_string(),
            port.to_string(),
        ];
        // npm/pnpm installations expose `opencode` as a `.cmd`/`.bat` shim,
        // which `Command` cannot execute directly on Windows; the same
        // translation the ACP launcher uses applies here.
        if let Some(translated) = crate::acp::windows_script_launcher(&words) {
            words = translated;
        }
        let (program, args) = words
            .split_first()
            .ok_or_else(|| "openCode serve: empty launch command".to_string())?;

        let mut child = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                format!(
                    "failed to start `{} serve` in '{}': {error}. \
                     Is the {} CLI installed and on PATH?",
                    backend.name(),
                    cwd.display(),
                    backend.name(),
                )
            })?;
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(stdout) = child.stdout.take() {
            spawn_log_reader(stdout, Arc::new(Mutex::new(VecDeque::new())));
        }
        if let Some(pipe) = child.stderr.take() {
            spawn_log_reader(pipe, Arc::clone(&stderr));
        }

        log::debug!(
            "{}: server spawned on http://127.0.0.1:{port} (cwd '{}')",
            backend.name(),
            cwd.display()
        );
        Ok(Arc::new(Self {
            backend,
            http: Http::new(format!("http://127.0.0.1:{port}"), auth, instance_header),
            child: Mutex::new(Some(child)),
            stderr,
            cwd: cwd.to_path_buf(),
            ready: AtomicBool::new(false),
        }))
    }

    /// The workspace the server was started in.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The HTTP client bound to this server.
    #[must_use]
    pub fn http(&self) -> &Http {
        &self.http
    }

    /// Wait until the server answers `/global/health`.
    ///
    /// Idempotent: after the first success every call returns immediately.
    ///
    /// # Errors
    ///
    /// Returns an error with the child's stderr tail when the server exits
    /// early or never becomes healthy within [`START_TIMEOUT`].
    pub async fn ensure_ready(&self) -> Result<(), String> {
        if self.ready.load(Ordering::Acquire) {
            return Ok(());
        }
        let started = std::time::Instant::now();
        loop {
            if let Some(error) = self.exit_status() {
                return Err(format!(
                    "`{} serve` exited before becoming ready: {error}{}",
                    self.backend.name(),
                    self.stderr_tail()
                ));
            }
            let health = tokio::time::timeout(
                Duration::from_secs(2),
                self.http.get::<super::api::Health>("/global/health"),
            )
            .await;
            if let Ok(Ok(health)) = health
                && health.healthy
            {
                log::info!(
                    "{}: server ready on {} (version {})",
                    self.backend.name(),
                    self.http.base(),
                    health.version.as_deref().unwrap_or("unknown")
                );
                self.ready.store(true, Ordering::Release);
                return Ok(());
            }
            if started.elapsed() >= START_TIMEOUT {
                return Err(format!(
                    "`{} serve` did not become healthy within {}s{}",
                    self.backend.name(),
                    START_TIMEOUT.as_secs(),
                    self.stderr_tail()
                ));
            }
            tokio::time::sleep(HEALTH_POLL).await;
        }
    }

    /// Whether the child process is still running.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.exit_status().is_none()
    }

    /// Kill the server (idempotent).
    pub fn shutdown(&self) {
        self.ready.store(false, Ordering::Release);
        let mut slot = self.child.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(child) = slot.take() {
            kill_tree(child);
            log::debug!("{}: server stopped", self.backend.name());
        }
    }

    /// `None` while the child runs, `Some(status)` once it exited.
    fn exit_status(&self) -> Option<String> {
        let mut slot = self.child.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(child) = slot.as_mut() else {
            return Some("server is stopped".to_string());
        };
        match child.try_wait() {
            Ok(None) => None,
            Ok(Some(status)) => Some(format!("exit status {status}")),
            Err(error) => Some(format!("could not read the child status: {error}")),
        }
    }

    /// The retained stderr lines, formatted as a suffix for error messages.
    fn stderr_tail(&self) -> String {
        let lines = self.stderr.lock().unwrap_or_else(PoisonError::into_inner);
        if lines.is_empty() {
            return String::new();
        }
        let mut text = String::from("\nstderr:");
        for line in lines.iter() {
            text.push_str("\n  ");
            text.push_str(line);
        }
        text
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Kill the server and everything it spawned, then reap it.
///
/// On Windows the launch goes through the npm `.cmd` shim, so the child this
/// process owns is the `cmd` wrapper - killing it alone would leave the real
/// Bun server listening on its port. `taskkill /T` terminates the whole
/// tree; on Unix the child IS the server and a plain kill suffices.
fn kill_tree(mut child: Child) {
    #[cfg(windows)]
    {
        let pid = child.id().to_string();
        let _ = Command::new("taskkill")
            .args(["/PID", pid.as_str(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Read a child pipe line by line into the log, mirroring the ACP launcher's
/// debug logging so a failing server is diagnosable with `RUST_LOG=debug`.
fn spawn_log_reader(pipe: impl std::io::Read + Send + 'static, tail: Arc<Mutex<VecDeque<String>>>) {
    std::thread::Builder::new()
        .name("opencode-server-log".to_string())
        .spawn(move || {
            for line in BufReader::new(pipe).lines() {
                let Ok(line) = line else { break };
                log::debug!("opencode server: {line}");
                let mut tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
                if tail.len() == STDERR_TAIL {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        })
        .ok();
}

/// Reserve a loopback port by binding and immediately releasing it.
fn free_loopback_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| format!("could not reserve a loopback port: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("could not read the reserved port: {error}"))?
        .port();
    drop(listener);
    Ok(port)
}

/// Basic-auth credentials for the server, read from the variables the
/// backend's CLI itself documents. `None` when no password is configured.
fn server_auth(backend: super::Backend) -> Option<(String, String)> {
    let password = std::env::var(backend.password_env())
        .ok()
        .filter(|value| !value.is_empty())?;
    let user = std::env::var(backend.username_env())
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| backend.default_username().to_string());
    Some((user, password))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reserved_port_is_usable_loopback_space() {
        let port = free_loopback_port().expect("a free port");
        assert!(port > 0);
        // Binding it again must work: the reservation listener was closed.
        let listener = std::net::TcpListener::bind(("127.0.0.1", port));
        assert!(listener.is_ok());
    }

    #[test]
    fn server_auth_requires_a_password() {
        // The environment is process-global; assert the documented shape
        // only when it is actually configured.
        if let Some((user, password)) = server_auth(crate::opencode::Backend::OpenCode) {
            assert!(!user.is_empty());
            assert!(!password.is_empty());
        }
    }
}
