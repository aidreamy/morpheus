//! One prompt turn against a live OpenCode server.
//!
//! The sequence mirrors what the ACP path does, expressed with the server's
//! HTTP + SSE API:
//!
//! 1. seed the pane's plan from the session's todo list (a resumed session
//!    already has one),
//! 2. open the server-wide event stream BEFORE dispatching (no missed parts),
//! 3. dispatch the turn: a plain message goes to
//!    `POST /session/:id/prompt_async`, while a `/command` the server
//!    advertises goes to `POST /session/:id/command` — the endpoint that
//!    actually executes it,
//! 4. stream parts as [`SubagentEvent`]s, answering permission requests as
//!    they arrive (headless auto-approve, or the pane's own answer), and
//!    forwarding usage, retries and the session title,
//! 5. end on `session.idle` — or on the caller's stop signal, which is
//!    turned into `POST /session/:id/abort` and the agent's own idle.
//!
//! The report contract is the same as the ACP path's: only the text written
//! after the last tool call is returned (see [`crate::closure`]).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use super::api::{self, PromptBody, PromptPart};
use super::event::{Mapper, Signal, permission_answer, permission_options, spawn_pump};
use super::server::Server;
use crate::acp::TurnOutcome;
use crate::closure::TurnClosure;
use crate::events::{PanePermissionAnswer, SubagentEvent};

/// How often the stop flag is inspected while waiting for the next event.
const STOP_POLL: Duration = Duration::from_millis(150);

/// How long an aborted session may take to report idle before the client
/// stops waiting for it.
const ABORT_GRACE: Duration = Duration::from_secs(15);

/// Resolve the session to use: resume `resume` when the server still knows
/// it, otherwise open a fresh one.
///
/// # Errors
///
/// Returns an error when the server cannot be reached or refuses to create a
/// session.
pub(super) async fn ensure_session(
    server: &Server,
    resume: Option<&str>,
) -> Result<String, String> {
    if let Some(id) = resume.filter(|id| !id.is_empty()) {
        let path = format!("/session/{id}");
        match server.http().get_optional::<api::Session>(&path).await {
            Ok(Some(_)) => return Ok(id.to_string()),
            Ok(None) => log::debug!("opencode: session '{id}' is gone, opening a new one"),
            Err(error) => log::debug!("opencode: could not probe session '{id}': {error}"),
        }
    }
    let session: api::Session = server
        .http()
        .post("/session", &serde_json::json!({ "title": "morpheus" }))
        .await?;
    if session.id.is_empty() {
        return Err("opencode: the server created a session without an id".to_string());
    }
    Ok(session.id)
}

/// The model a `subagent_call` pick set selects, in the server's spelling.
///
/// Picks use the ACP config-option convention (`(option id, value)`); the
/// model option this crate publishes for OpenCode is `model`, whose values
/// are `provider/model` — exactly what the server's prompt body takes.
#[must_use]
pub(super) fn model_from_picks(picks: &[(String, String)]) -> Option<api::ModelRef> {
    picks
        .iter()
        .find(|(id, _)| id == "model")
        .and_then(|(_, value)| api::ModelRef::parse(value))
}

/// Split a `/command args` message into its command name and its arguments.
///
/// Only the FIRST word is the command; everything after the first space is
/// passed through verbatim (newlines included), which is what the server's
/// command endpoint expects.
#[must_use]
pub(super) fn split_command(input: &str) -> Option<(&str, &str)> {
    let trimmed = input.trim_start();
    let rest = trimmed.strip_prefix('/')?;
    let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let command = &rest[..end];
    if command.is_empty() {
        return None;
    }
    let arguments = rest[end..].trim_start();
    Some((command, arguments))
}

/// Run one turn and report what it produced.
///
/// # Errors
///
/// Returns the protocol-level reason when the turn could not run at all
/// (server unreachable, prompt refused, event stream lost) and produced no
/// text. A mid-turn agent error with partial output is reported as a failed
/// [`TurnOutcome`] instead, so the caller keeps what the agent said.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    server: &Server,
    session: &str,
    input: &str,
    model: Option<api::ModelRef>,
    agent: Option<&str>,
    mut permission_rx: Option<UnboundedReceiver<PanePermissionAnswer>>,
    stop: Arc<AtomicBool>,
    chunk_tx: &UnboundedSender<SubagentEvent>,
) -> Result<TurnOutcome, String> {
    server.ensure_ready().await?;
    seed_plan(server, session, chunk_tx).await;
    // Subscribe first: the prompt may start streaming immediately, and the
    // stream is server-wide, so anything not yet read is simply a part the
    // mapper has not seen.
    let stream = server.http().open_stream("/event").await?;
    let mut events = spawn_pump(stream);
    let mut mapper = Mapper::new(false);
    let mut closure = TurnClosure::default();
    let mut windows_cache: Option<HashMap<String, u64>> = None;

    // The command endpoint takes the model as a `provider/model` string;
    // keep that spelling before the typed value moves into the prompt body.
    let model_value = model
        .as_ref()
        .map(|model| format!("{}/{}", model.provider, model.model));
    let body = PromptBody {
        parts: vec![PromptPart::Text { text: input }],
        model,
        agent: agent.filter(|agent| !agent.is_empty()),
    };
    dispatch(server, session, input, &body, agent, model_value.as_deref()).await?;

    let mut cancelled = false;
    let mut abort_deadline: Option<std::time::Instant> = None;
    let result: Result<(), String> = 'turn: loop {
        tokio::select! {
            event = events.recv() => {
                let Some(event) = event else {
                    if cancelled {
                        break 'turn Ok(());
                    }
                    break 'turn Err("opencode: the event stream closed mid-turn".to_string());
                };
                let (mapped, signals) = mapper.map(&event, session);
                for event in mapped {
                    closure.observe(&event);
                    let _ = chunk_tx.send(event);
                }
                for signal in signals {
                    match signal {
                        Signal::Idle => break 'turn Ok(()),
                        Signal::Error(error) => break 'turn Err(error),
                        Signal::Permission(notice) => {
                            let answer = match permission_rx.as_mut() {
                                Some(rx) => {
                                    let _ = chunk_tx.send(SubagentEvent::PermissionRequest {
                                        call_id: notice.id.clone(),
                                        title: notice.title.clone(),
                                        kind: notice.kind,
                                        raw_input: notice.raw_input.clone(),
                                        options: permission_options(),
                                    });
                                    await_answer(rx, &stop).await
                                }
                                // Headless call: the YOLO contract, same as the
                                // ACP path's auto-approve.
                                None => Some(PanePermissionAnswer::Selected(
                                    api::answer::ONCE.to_string(),
                                )),
                            };
                            let response = permission_answer(answer.as_ref());
                            let path = format!("/session/{session}/permissions/{}", notice.id);
                            if let Err(error) = server
                                .http()
                                .post_ack(&path, &serde_json::json!({ "response": response }))
                                .await
                            {
                                log::warn!("opencode: could not answer a permission request: {error}");
                            }
                        }
                        Signal::Usage(usage) => {
                            if windows_cache.is_none() {
                                windows_cache = Some(fetch_context_windows(server).await);
                            }
                            let window = windows_cache
                                .as_ref()
                                .and_then(|windows| {
                                    windows.get(&format!("{}/{}", usage.provider, usage.model))
                                })
                                .copied()
                                .unwrap_or_default();
                            // A context percentage without a declared window
                            // would render as a permanent 0%; skip it instead.
                            if window > 0 {
                                let _ = chunk_tx.send(SubagentEvent::Usage {
                                    context_window: window,
                                    tokens_in_context: usage.in_context(),
                                });
                            }
                            log::trace!(
                                "opencode: usage provider={} model={} in={} out={} reasoning={} total={}",
                                usage.provider,
                                usage.model,
                                usage.input,
                                usage.output,
                                usage.reasoning,
                                usage.total
                            );
                        }
                        Signal::Retry { attempt, message } => {
                            log::debug!("opencode: provider retry #{attempt}: {message}");
                            let _ = chunk_tx.send(SubagentEvent::Thought {
                                text: format!("[retry {attempt}] {message}"),
                            });
                        }
                        Signal::Title(title) => {
                            let _ = chunk_tx.send(SubagentEvent::SessionInfo {
                                title: Some(title),
                            });
                        }
                    }
                }
            }
            () = tokio::time::sleep(STOP_POLL) => {
                if stop.load(Ordering::Relaxed) && !cancelled {
                    cancelled = true;
                    abort_deadline = Some(std::time::Instant::now() + ABORT_GRACE);
                    let path = format!("/session/{session}/abort");
                    if let Err(error) = server.http().post_empty(&path).await {
                        log::warn!("opencode: abort failed: {error}");
                    }
                }
                if let Some(deadline) = abort_deadline
                    && std::time::Instant::now() >= deadline
                {
                    break 'turn Ok(());
                }
            }
        }
    };

    match result {
        Ok(()) => Ok(TurnOutcome {
            output: closure.output(),
            stop_reason: if cancelled { "cancelled" } else { "end_turn" }.to_string(),
            session_id: Some(session.to_string()),
            failed: false,
        }),
        Err(error) => {
            let error = auth_hint(error);
            if closure.output().is_empty() {
                Err(error)
            } else {
                log::warn!("opencode: session {session} failed mid-turn: {error}");
                Ok(TurnOutcome {
                    output: closure.output(),
                    stop_reason: "error".to_string(),
                    session_id: None,
                    failed: true,
                })
            }
        }
    }
}

/// Send the turn: a slash command the server advertises goes through the
/// command endpoint, everything else is a regular prompt.
///
/// The command endpoint answers only when the turn is finished, so its POST
/// runs detached — the SSE stream is what feeds the turn either way.
async fn dispatch(
    server: &Server,
    session: &str,
    input: &str,
    body: &PromptBody<'_>,
    agent: Option<&str>,
    model: Option<&str>,
) -> Result<(), String> {
    if let Some((command, arguments)) = split_command(input)
        && advertised_command(server, command).await
    {
        let http = server.http().clone();
        let path = format!("/session/{session}/command");
        // The detached POST outlives this call, so the values it sends are
        // owned rather than borrowed from `input`. Optional keys are only
        // sent when set: the server rejects a `null` agent/model.
        let name = command.to_string();
        let mut payload = serde_json::json!({
            "command": name.clone(),
            "arguments": arguments.to_string(),
        });
        if let Some(agent) = agent.filter(|agent| !agent.is_empty()) {
            payload["agent"] = Value::String(agent.to_string());
        }
        if let Some(model) = model {
            payload["model"] = Value::String(model.to_string());
        }
        log::debug!("opencode: running session command /{name}");
        tokio::spawn(async move {
            if let Err(error) = http.post_ack::<serde_json::Value>(&path, &payload).await {
                log::warn!("opencode: command /{name} failed: {error}");
            }
        });
        return Ok(());
    }
    let path = format!("/session/{session}/prompt_async");
    server
        .http()
        .post_ack(&path, body)
        .await
        .map_err(|error| format!("opencode: the prompt was refused: {error}"))
}

/// Whether the server advertises `name` as a command. An unknown `/word` is
/// ordinary text and must reach the model as written.
async fn advertised_command(server: &Server, name: &str) -> bool {
    match server.http().get::<Vec<api::Command>>("/command").await {
        Ok(commands) => commands.iter().any(|command| command.name == name),
        Err(error) => {
            log::debug!("opencode: could not list commands: {error}");
            false
        }
    }
}

/// Seed the plan from the session's current todo list, so a resumed pane
/// shows the work already planned before this turn.
async fn seed_plan(server: &Server, session: &str, chunk_tx: &UnboundedSender<SubagentEvent>) {
    let path = format!("/session/{session}/todo");
    let Ok(todos) = server.http().get::<Vec<api::Todo>>(&path).await else {
        return;
    };
    let entries: Vec<_> = todos
        .iter()
        .filter(|todo| !todo.content.is_empty())
        .map(super::event::plan_entry)
        .collect();
    if !entries.is_empty() {
        let _ = chunk_tx.send(SubagentEvent::Plan { entries });
    }
}

/// The context window each `provider/model` declares, for the usage badge.
async fn fetch_context_windows(server: &Server) -> HashMap<String, u64> {
    let Ok(providers) = server
        .http()
        .get::<api::Providers>("/config/providers")
        .await
    else {
        return HashMap::new();
    };
    let mut windows = HashMap::new();
    for provider in &providers.providers {
        for (key, model) in &provider.models {
            let model_id = if model.id.is_empty() {
                key.as_str()
            } else {
                model.id.as_str()
            };
            if let Some(context) = model.limit.as_ref().and_then(|limit| limit.context) {
                windows.insert(format!("{}/{model_id}", provider.id), context);
            }
        }
    }
    windows
}

/// Make a provider-auth failure actionable: the server reports the missing
/// credential, but only the CLI knows how to store one.
fn auth_hint(error: String) -> String {
    if error.contains("ProviderAuth") || error.contains("AuthError") {
        return format!(
            "{error}\n\nOpenCode has no usable credentials for that provider. Run \
             `opencode auth login` (or set the provider's API key) and try again."
        );
    }
    error
}

/// Await the pane's permission answer, giving up (rejecting) when the turn
/// is stopped meanwhile.
async fn await_answer(
    rx: &mut UnboundedReceiver<PanePermissionAnswer>,
    stop: &AtomicBool,
) -> Option<PanePermissionAnswer> {
    loop {
        tokio::select! {
            answer = rx.recv() => return answer,
            () = tokio::time::sleep(STOP_POLL) => {
                if stop.load(Ordering::Relaxed) {
                    return None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_pick_is_read_from_the_option_set() {
        let picks = vec![
            ("model".to_string(), "anthropic/claude-sonnet-5".to_string()),
            ("other".to_string(), "ignored".to_string()),
        ];
        let model = model_from_picks(&picks).expect("a model");
        assert_eq!(model.provider, "anthropic");
        assert_eq!(model.model, "claude-sonnet-5");
    }

    #[test]
    fn an_unaddressable_model_pick_is_ignored() {
        let picks = vec![("model".to_string(), "claude-sonnet-5".to_string())];
        assert!(model_from_picks(&picks).is_none());
        assert!(model_from_picks(&[]).is_none());
    }

    #[test]
    fn commands_are_split_from_their_arguments() {
        assert_eq!(split_command("/init"), Some(("init", "")));
        assert_eq!(
            split_command("/review the diff please"),
            Some(("review", "the diff please"))
        );
        assert_eq!(
            split_command("  /cmd  two\nlines"),
            Some(("cmd", "two\nlines"))
        );
        // Not a command: ordinary text (sent to the model verbatim).
        assert_eq!(split_command("hello /world"), None);
        assert_eq!(split_command("/"), None);
        assert_eq!(split_command("/ spaced"), None);
    }

    #[test]
    fn provider_auth_errors_carry_the_login_hint() {
        let error = auth_hint("ProviderAuthError: missing key".to_string());
        assert!(error.contains("opencode auth login"), "{error}");
        assert_eq!(auth_hint("boom".to_string()), "boom");
    }
}
