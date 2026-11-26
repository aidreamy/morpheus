//! The persistent OpenCode session a pane keeps between turns.
//!
//! The ACP path keeps one harness process and one session alive for the
//! lifetime of a pane. The native path keeps the equivalent pair: one
//! [`Server`] child process and one server session id. Turns are serialized
//! by the caller (the pane dispatch gate already guarantees one at a time),
//! exactly like the ACP supervisor.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use super::server::Server;
use super::turn;
use crate::acp::TurnOutcome;
use crate::events::{PanePermissionAnswer, SubagentEvent};

/// One live native session.
pub struct Session {
    server: std::sync::Arc<Server>,
    cwd: PathBuf,
    /// The session id to resume (from the caller) and, afterwards, the id
    /// this handle runs on.
    session_id: Mutex<Option<String>>,
    shutdown: AtomicBool,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("opencode::Session")
            .field("cwd", &self.cwd)
            .field("server", &self.server)
            .field("alive", &self.is_alive())
            .finish()
    }
}

impl Session {
    /// Spawn the server for `cwd` and remember the session id to resume.
    ///
    /// # Errors
    ///
    /// Returns an error when the server process cannot be spawned. Protocol
    /// failures surface on the first turn, like the ACP path.
    pub fn start(cwd: &Path, resume: Option<String>) -> Result<Self, String> {
        let server = Server::start(cwd)?;
        Ok(Self {
            server,
            cwd: cwd.to_path_buf(),
            session_id: Mutex::new(resume.filter(|id| !id.is_empty())),
            shutdown: AtomicBool::new(false),
        })
    }

    /// The canonical workspace the session was opened in.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Whether the server child is still running and the handle is open.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        !self.shutdown.load(Ordering::Relaxed) && self.server.is_alive()
    }

    /// Stop the server (idempotent).
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.server.shutdown();
    }

    /// Run one turn, resuming the stored session when the server still knows
    /// it and opening one on the first call otherwise.
    ///
    /// A failed turn leaves the stored id in place: the server keeps its
    /// session across a failed prompt, and the next call probes it again
    /// (falling back to a fresh session when it is gone).
    #[allow(clippy::too_many_arguments)]
    pub async fn run_turn(
        &self,
        input: String,
        picks: &[(String, String)],
        mode: Option<&str>,
        permission_rx: Option<UnboundedReceiver<PanePermissionAnswer>>,
        stop_signal: std::sync::Arc<AtomicBool>,
        chunk_tx: UnboundedSender<SubagentEvent>,
    ) -> TurnOutcome {
        if self.shutdown.load(Ordering::Relaxed) {
            return crate::acp::failed_outcome(
                "The OpenCode session was closed before answering the turn.".to_string(),
            );
        }
        if let Err(error) = self.server.ensure_ready().await {
            return crate::acp::failed_outcome(error);
        }
        let resume = self
            .session_id
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let model = turn::model_from_picks(picks);
        let session = match turn::ensure_session(&self.server, resume.as_deref()).await {
            Ok(session) => session,
            Err(error) => return crate::acp::failed_outcome(error),
        };
        *self
            .session_id
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(session.clone());
        match turn::run(
            &self.server,
            &session,
            &input,
            model,
            mode,
            permission_rx,
            stop_signal,
            &chunk_tx,
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(error) => crate::acp::failed_outcome(error),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}
