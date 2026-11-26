//! Native OpenCode integration through its own headless server.
//!
//! OpenCode is ACP-capable, and until now Morpheus drove it through the
//! generic ACP client like every other harness. It also ships something the
//! ACP surface does not expose: `opencode serve`, a documented HTTP server
//! with an OpenAPI spec, server-sent events for the live turn and REST
//! endpoints for the whole session model (messages, agents, commands,
//! providers, permissions). This module uses that server directly, so
//! OpenCode turns carry richer, correctly typed data than the generic
//! protocol translation can.
//!
//! # Layout
//!
//! | Module        | Responsibility                                                        |
//! | ------------- | --------------------------------------------------------------------- |
//! | [`api`]       | Wire types of the documented endpoints (tolerant, all fields optional) |
//! | [`http`]      | HTTP client plus the buffered SSE decoder                              |
//! | [`server`]    | Lifecycle of the `opencode serve` child (port, health, auth, kill)     |
//! | [`event`]     | Server events → [`SubagentEvent`](crate::events::SubagentEvent)        |
//! | `turn`        | One prompt turn: subscribe, prompt, permissions, stop, closure         |
//! | `session`     | Persistent pane session (one server + one session id)                  |
//! | `snapshot`    | Advertised surface and history replay                                  |
//!
//! # Routing
//!
//! The public entry points stay [`crate::acp::call`],
//! [`crate::acp::open_session`], [`crate::acp::session_snapshot`] and
//! [`crate::acp::replay`]; those functions route an `opencode` call here
//! when [`is_native`] holds and use ACP for every other agent (and for
//! OpenCode when the native path is disabled). Callers — the TUI, the
//! WhatsApp bridge, the `subagent_call` tool — therefore need no change and
//! receive the same types they always did.
//!
//! Set `MORPHEUS_OPENCODE_TRANSPORT=acp` to force the generic ACP transport
//! (a diagnostic escape hatch); the native transport is the default whenever
//! the OpenCode CLI is installed.
//!
//! # What the native path adds
//!
//! - Real tool names and per-call status from the server's `tool` parts, not
//!   just a protocol kind plus free-form title.
//! - The session's todo list as the pane's plan.
//! - The provider/model catalog as the pane's model picker, with the model
//!   sent in the prompt body itself.
//! - Abort, permissions and session resume over documented endpoints,
//!   instead of a translated protocol turn.
//!
//! Session semantics match ACP's: consecutive calls resume the most recent
//! session by default, a vanished session silently falls back to a fresh
//! one, and only a clean turn stores its id.

pub mod api;
pub mod event;
pub mod http;
pub mod server;
pub mod session;
mod snapshot;
mod turn;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::acp::{CwdStrictness, SessionHandle, SessionSnapshot, prepare_session_cwd};
use crate::events::{PanePermissionAnswer, SubagentEvent};

/// The registered agent name this backend owns (OpenCode).
pub const AGENT: &str = "opencode";

/// Which native server backend a pane is driving.
///
/// Kilo is a fork of OpenCode and keeps the same HTTP/SSE server, so the
/// two backends share every byte of protocol code; only the operational
/// details (binary name, auth env vars, default username, instance routing
/// header and the transport override env var) differ. Keeping them as one
/// enum (rather than two modules) avoids duplicating the protocol layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    OpenCode,
    Kilo,
}

impl Backend {
    /// Map an agent name to its backend, if it is one of the native ones.
    #[must_use]
    pub(crate) fn from_agent(agent: &str) -> Option<Self> {
        match agent.to_ascii_lowercase().as_str() {
            "opencode" => Some(Self::OpenCode),
            "kilo" => Some(Self::Kilo),
            _ => None,
        }
    }

    /// The agent/binary name (`opencode` or `kilo`).
    #[must_use]
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::OpenCode => "opencode",
            Self::Kilo => "kilo",
        }
    }

    /// Environment variable holding the server password.
    #[must_use]
    pub(crate) fn password_env(self) -> &'static str {
        match self {
            Self::OpenCode => "OPENCODE_SERVER_PASSWORD",
            Self::Kilo => "KILO_SERVER_PASSWORD",
        }
    }

    /// Environment variable holding the server username.
    #[must_use]
    pub(crate) fn username_env(self) -> &'static str {
        match self {
            Self::OpenCode => "OPENCODE_SERVER_USERNAME",
            Self::Kilo => "KILO_SERVER_USERNAME",
        }
    }

    /// Default username when no env override is set.
    #[must_use]
    pub(crate) fn default_username(self) -> &'static str {
        self.name()
    }

    /// Environment variable that forces the generic ACP transport.
    #[must_use]
    pub(crate) fn transport_env(self) -> &'static str {
        match self {
            Self::OpenCode => "MORPHEUS_OPENCODE_TRANSPORT",
            Self::Kilo => "MORPHEUS_KILO_TRANSPORT",
        }
    }

    /// HTTP header Kilo uses for instance routing (`None` for OpenCode,
    /// whose server binds to a single directory).
    #[must_use]
    pub(crate) fn instance_header(self) -> Option<&'static str> {
        match self {
            Self::OpenCode => None,
            Self::Kilo => Some("x-kilo-directory"),
        }
    }
}

/// Whether `agent` must be driven through the native server.
///
/// True for the built-in `opencode` harness when the CLI is installed, which
/// is exactly the case the ACP transport used to handle. The environment
/// override keeps a broken native path from blocking a user:
/// `MORPHEUS_OPENCODE_TRANSPORT=acp` restores the generic transport.
#[must_use]
pub fn is_native(agent: &str) -> bool {
    let Some(backend) = Backend::from_agent(agent) else {
        return false;
    };
    if std::env::var(backend.transport_env())
        .map(|value| value.eq_ignore_ascii_case("acp"))
        .unwrap_or(false)
    {
        return false;
    }
    crate::acp::detect_installed().contains(&backend.name())
}

/// One native turn with the same contract as [`crate::acp::call`].
///
/// # Errors
///
/// Returns the protocol-level reason when the server cannot start, the
/// session cannot be opened, or the turn fails without producing output.
#[allow(clippy::too_many_arguments)]
pub async fn call(
    agent: &str,
    input: &str,
    resume: Option<String>,
    picks: &[(String, String)],
    mode: Option<&str>,
    permission_rx: Option<UnboundedReceiver<PanePermissionAnswer>>,
    cwd: PathBuf,
    stop_signal: Arc<AtomicBool>,
    chunk_tx: UnboundedSender<SubagentEvent>,
) -> Result<(String, String, Option<String>), String> {
    // STRICT, like the ACP path: a live turn never silently relocates a
    // vanished workspace.
    let backend = Backend::from_agent(agent).expect("is_native validated the agent");
    let cwd = prepare_session_cwd(&cwd, CwdStrictness::Strict);
    let server = server::Server::start(&cwd, backend)?;
    let outcome = async {
        server.ensure_ready().await?;
        let session = turn::ensure_session(&server, resume.as_deref()).await?;
        turn::run(
            &server,
            &session,
            input,
            turn::model_from_picks(picks),
            mode,
            permission_rx,
            stop_signal,
            &chunk_tx,
        )
        .await
    }
    .await;
    server.shutdown();
    let outcome = outcome?;
    Ok((outcome.output, outcome.stop_reason, outcome.session_id))
}

/// Open the persistent native session a pane keeps between turns.
///
/// # Errors
///
/// Returns an error when the `opencode` server process cannot be spawned.
pub fn open_session(
    agent: &str,
    resume: Option<String>,
    cwd: PathBuf,
) -> Result<SessionHandle, String> {
    let backend = Backend::from_agent(agent).expect("is_native validated the agent");
    let cwd = prepare_session_cwd(&cwd, CwdStrictness::Strict);
    let session = session::Session::start(&cwd, resume, backend)?;
    Ok(SessionHandle::from_native(Arc::new(session), cwd))
}

/// Probe the advertised commands, modes and model catalog, plus the mode and
/// model the given session currently runs with.
///
/// # Errors
///
/// Returns an error when the throwaway server cannot start or be reached.
pub async fn snapshot(
    agent: &str,
    session_id: Option<String>,
    cwd: PathBuf,
) -> Result<SessionSnapshot, String> {
    let backend = Backend::from_agent(agent).expect("is_native validated the agent");
    snapshot::snapshot(backend, session_id, cwd).await
}

/// Replay a session's retained conversation into `chunk_tx`.
///
/// # Errors
///
/// Returns an error when the throwaway server cannot start, the session is
/// gone, or its messages cannot be read.
pub async fn replay(
    agent: &str,
    session_id: String,
    cwd: PathBuf,
    chunk_tx: UnboundedSender<SubagentEvent>,
) -> Result<(), String> {
    let backend = Backend::from_agent(agent).expect("is_native validated the agent");
    snapshot::replay(backend, &session_id, cwd, chunk_tx).await
}
