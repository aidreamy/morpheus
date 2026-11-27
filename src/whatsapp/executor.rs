//! One ACP agent instance, driven headlessly.
//!
//! A selected instance owns one live [`SessionHandle`] between turns. The
//! handle is reused while it is alive and rooted at the chat's working
//! directory; otherwise it is reopened and resumed with the persisted session
//! id. Each turn drains the typed event stream so permission requests are
//! answered (deny-by-default outside `yolo`) and the agent's advertised modes
//! and config options are learned for the next turn.
//!
//! There is no approval dialog on WhatsApp: outside `yolo` every permission
//! request is answered with the agent's own reject option, or `Cancel` when it
//! offers none. `yolo` picks the first allow option. The permission channel is
//! ALWAYS `Some`; passing `None` would let the ACP layer auto-approve, which is
//! only acceptable in `yolo` and is never needed there.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use morpheus_agents::acp::{self, SessionHandle};
use morpheus_agents::events::{
    AgentConfigOption, AgentPermissionOption, AgentSessionMode, PanePermissionAnswer, SubagentEvent,
};
use tokio::sync::mpsc;

use crate::harness::Mode;

/// What one ACP turn produced, plus the session state learned while it ran.
pub struct AcpTurnResult {
    pub output: String,
    pub stop_reason: String,
    pub session_id: Option<String>,
    pub failed: bool,
    pub denied_tools: Vec<String>,
    /// The modes the agent advertised during this turn, when it (re)published
    /// them. `None` means the previous list still applies.
    pub modes: Option<Vec<AgentSessionMode>>,
    /// The config options advertised during this turn (used for model picks).
    pub config_options: Option<Vec<AgentConfigOption>>,
}

impl AcpTurnResult {
    fn before_prompt(output: String, stop_reason: &str, failed: bool) -> Self {
        Self {
            output,
            stop_reason: stop_reason.to_string(),
            session_id: None,
            failed,
            denied_tools: Vec::new(),
            modes: None,
            config_options: None,
        }
    }
}

/// One live ACP instance.
pub struct AcpSession {
    handle: SessionHandle,
    pub modes: Vec<AgentSessionMode>,
    pub config_options: Vec<AgentConfigOption>,
}

impl AcpSession {
    /// Open (or resume) the instance's session.
    ///
    /// # Errors
    ///
    /// Returns the ACP layer's reason when the agent is unknown or its
    /// launcher cannot start; handshake failures surface on the first turn.
    pub fn open(agent: &str, resume: Option<String>, cwd: &Path) -> Result<Self, String> {
        let handle = acp::open_session(agent, resume, cwd.to_path_buf())?;
        Ok(Self {
            handle,
            modes: Vec::new(),
            config_options: Vec::new(),
        })
    }

    /// Whether the live process is still usable for this workspace.
    #[must_use]
    pub fn is_usable(&self, cwd: &Path) -> bool {
        self.handle.is_alive() && self.handle.matches_cwd(cwd)
    }

    #[must_use]
    pub fn handle(&self) -> SessionHandle {
        self.handle.clone()
    }

    /// Close the harness process (safe to call repeatedly).
    pub fn shutdown(&self) {
        self.handle.shutdown();
    }

    /// Fold a finished turn's discoveries into the reusable state.
    pub fn absorb(&mut self, result: &AcpTurnResult) {
        if let Some(modes) = &result.modes {
            self.modes.clone_from(modes);
        }
        if let Some(options) = &result.config_options {
            self.config_options.clone_from(options);
        }
    }
}

/// Run one turn on a live handle.
///
/// `yolo` selects the auto-allow answer; every other mode rejects. The caller
/// owns labeling and persistence.
pub async fn run(
    handle: SessionHandle,
    input: String,
    modes: Vec<AgentSessionMode>,
    config_options: Vec<AgentConfigOption>,
    mode: Mode,
    model: Option<String>,
    yolo: bool,
    stop: Arc<AtomicBool>,
) -> AcpTurnResult {
    let native_modes = if matches!(mode, Mode::Ask | Mode::Build) {
        match handle.native_modes(&stop).await {
            Ok(modes) => modes,
            Err(error) if mode == Mode::Build => {
                log::debug!("morpheus whatsapp: could not read native modes: {error}");
                None
            }
            Err(error) => {
                return AcpTurnResult::before_prompt(
                    format!("Could not read the agent's available modes: {error}"),
                    "error",
                    true,
                );
            }
        }
    } else {
        None
    };
    if stop.load(Ordering::Relaxed) {
        return AcpTurnResult::before_prompt("Turn cancelled.".to_string(), "cancelled", false);
    }
    let mapped_mode = match map_mode(mode, native_modes.as_deref(), &modes) {
        Ok(mode) => mode,
        Err(error) => return AcpTurnResult::before_prompt(error.to_string(), "error", true),
    };
    let picks = crate::coordinator::turn::turn_picks(Some(&config_options), &[], model.as_deref());

    let (chunk_tx, mut chunk_rx) = mpsc::unbounded_channel::<SubagentEvent>();
    let (permission_tx, permission_rx) = mpsc::unbounded_channel();
    let turn = handle.run_turn(
        input,
        &picks,
        mapped_mode.as_deref(),
        Some(permission_rx),
        stop,
        chunk_tx,
    );
    tokio::pin!(turn);

    let mut denied_tools = Vec::new();
    let mut observed_modes = None;
    let mut observed_options = None;
    let outcome = loop {
        tokio::select! {
            outcome = &mut turn => break outcome,
            event = chunk_rx.recv() => match event {
                Some(event) => match event {
                    SubagentEvent::PermissionRequest { title, options, .. } => {
                        let (answer, denied) = permission_answer(&options, yolo);
                        if denied {
                            denied_tools.push(title);
                        }
                        // `Err` means the turn already ended; nothing to answer.
                        let _ = permission_tx.send(answer);
                    }
                    SubagentEvent::Modes { modes, .. } => observed_modes = Some(modes),
                    SubagentEvent::ConfigOptions { options } => observed_options = Some(options),
                    _ => {}
                },
                // The stream closes only when the turn future ends; keep
                // awaiting it for the terminal outcome.
                None => break turn.as_mut().await,
            }
        }
    };

    log::debug!(
        "morpheus whatsapp: ACP turn ended ({}, failed={})",
        outcome.stop_reason,
        outcome.failed
    );
    let output = if outcome.output.trim().is_empty() && !outcome.failed {
        "The agent finished without a textual reply.".to_string()
    } else {
        outcome.output
    };
    AcpTurnResult {
        output,
        stop_reason: outcome.stop_reason,
        session_id: outcome.session_id,
        failed: outcome.failed,
        denied_tools,
        modes: observed_modes,
        config_options: observed_options,
    }
}

fn map_mode(
    mode: Mode,
    native_modes: Option<&[AgentSessionMode]>,
    acp_modes: &[AgentSessionMode],
) -> Result<Option<String>, &'static str> {
    let Some(native_modes) = native_modes else {
        return Ok(crate::harness::agent_mode::map_mode_to_agent(
            mode, acp_modes,
        ));
    };
    let find = |id| {
        native_modes
            .iter()
            .find(|mode| mode.id == id)
            .map(|mode| mode.id.clone())
    };
    match mode {
        Mode::Ask => find("ask")
            .or_else(|| find("plan"))
            .map(Some)
            .ok_or("The native server offers neither Ask nor Plan mode."),
        Mode::Build => Ok(find("build").or_else(|| find("code"))),
        _ => Ok(None),
    }
}

/// The answer to one permission request: auto-allow in `yolo`, reject
/// everywhere else (falling back to cancellation when no reject is offered).
/// The boolean says whether this counted as a denied tool for the reply.
fn permission_answer(
    options: &[AgentPermissionOption],
    yolo: bool,
) -> (PanePermissionAnswer, bool) {
    if yolo {
        return match options
            .iter()
            .find(|option| option.kind.starts_with("allow"))
        {
            Some(option) => (PanePermissionAnswer::Selected(option.id.clone()), false),
            None => (PanePermissionAnswer::Cancel, false),
        };
    }
    match options
        .iter()
        .find(|option| option.kind.starts_with("reject"))
    {
        Some(option) => (PanePermissionAnswer::Selected(option.id.clone()), true),
        None => (PanePermissionAnswer::Cancel, true),
    }
}

/// A working directory for ACP sessions: absolute when possible, so the
/// harness's strict cwd validation does not see a relative path.
#[must_use]
pub fn session_cwd(cwd: &str) -> PathBuf {
    Path::new(cwd)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(cwd))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_ask_requires_an_advertised_ask_or_plan_mode() {
        let advertised = |id: &str, description: &str| AgentSessionMode {
            id: id.into(),
            name: id.into(),
            description: Some(description.into()),
        };
        let modes = [
            advertised("build", "Build from a plan"),
            advertised("plan", "Plan changes"),
            advertised("ask", "Answer questions"),
        ];
        assert_eq!(
            map_mode(Mode::Ask, Some(&modes), &[]).unwrap().as_deref(),
            Some("ask")
        );
        assert_eq!(
            map_mode(Mode::Ask, Some(&modes[..2]), &[])
                .unwrap()
                .as_deref(),
            Some("plan")
        );
        assert_eq!(
            map_mode(Mode::Build, Some(&modes), &[]).unwrap().as_deref(),
            Some("build")
        );
        let kilo_modes = [advertised("code", "Implement changes"), modes[2].clone()];
        assert_eq!(
            map_mode(Mode::Build, Some(&kilo_modes), &[])
                .unwrap()
                .as_deref(),
            Some("code")
        );
        assert!(map_mode(Mode::Ask, Some(&modes[..1]), &modes).is_err());
        assert!(map_mode(Mode::Ask, Some(&[]), &modes).is_err());
    }

    fn option(id: &str, kind: &str) -> AgentPermissionOption {
        AgentPermissionOption {
            id: id.to_string(),
            name: id.to_string(),
            kind: kind.to_string(),
        }
    }

    #[test]
    fn non_yolo_prefers_an_offered_reject() {
        let options = vec![
            option("allow-1", "allow_once"),
            option("reject-1", "reject_once"),
        ];
        assert_eq!(
            permission_answer(&options, false),
            (PanePermissionAnswer::Selected("reject-1".to_string()), true)
        );
    }

    #[test]
    fn non_yolo_cancels_when_no_reject_is_offered() {
        let options = vec![option("allow-1", "allow_once")];
        assert_eq!(
            permission_answer(&options, false),
            (PanePermissionAnswer::Cancel, true)
        );
        assert_eq!(
            permission_answer(&[], false),
            (PanePermissionAnswer::Cancel, true)
        );
    }

    #[test]
    fn yolo_prefers_an_offered_allow_and_never_counts_a_denial() {
        let options = vec![
            option("reject-1", "reject_once"),
            option("allow-1", "allow_always"),
            option("allow-2", "allow_once"),
        ];
        assert_eq!(
            permission_answer(&options, true),
            (PanePermissionAnswer::Selected("allow-1".to_string()), false)
        );
        assert_eq!(
            permission_answer(&[], true),
            (PanePermissionAnswer::Cancel, false)
        );
    }

    #[test]
    fn relative_working_directories_are_made_absolute_when_possible() {
        let cwd = session_cwd(".");
        assert!(cwd.is_absolute(), "{}", cwd.display());
    }
}
