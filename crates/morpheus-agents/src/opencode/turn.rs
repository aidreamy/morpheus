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
//!    advertises goes to `POST /session/:id/command` - the endpoint that
//!    actually executes it,
//! 4. stream parts as [`SubagentEvent`]s, answering permission requests as
//!    they arrive (headless auto-approve, or the pane's own answer), and
//!    forwarding usage, retries and the session title,
//! 5. end on `session.idle` - or on the caller's stop signal, which is
//!    turned into `POST /session/:id/abort` and the agent's own idle.
//!
//! The report contract is the same as the ACP path's: only the text written
//! after the last tool call is returned (see [`crate::closure`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use super::api::{self, PromptBody, PromptPart};
use super::event::{Mapper, Signal, permission_answer, permission_options};
use super::http::{EventStream, Http, REQUEST_TIMEOUT};
use super::server::Server;
use crate::acp::TurnOutcome;
use crate::closure::TurnClosure;
use crate::events::{PanePermissionAnswer, SubagentEvent};

/// How often the caller's stop flag is sampled.
const STOP_POLL: Duration = Duration::from_millis(150);

/// Deadline for the abort request and the subsequent confirmation of idle.
const ABORT_GRACE: Duration = Duration::from_secs(15);

/// Prepare the server and resolve the session without dispatching input.
pub(super) async fn prepare_session(
    server: &Server,
    resume: Option<&str>,
    stop: &AtomicBool,
) -> Result<Option<String>, String> {
    tokio::select! {
        biased;
        () = wait_for_stop(stop) => Ok(None),
        result = async {
            server.ensure_ready().await?;
            ensure_session(server.http(), resume).await
        } => result.map(Some),
    }
}

async fn ensure_session(http: &Http, resume: Option<&str>) -> Result<String, String> {
    if let Some(id) = resume.filter(|id| !id.is_empty()) {
        let path = format!("/session/{id}");
        if http.get_optional::<api::Session>(&path).await?.is_some() {
            return Ok(id.to_string());
        }
        log::debug!("opencode: session '{id}' is gone, opening a new one");
    }
    let session: api::Session = http
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
/// are `provider/model` - exactly what the server's prompt body takes.
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
    permission_rx: Option<UnboundedReceiver<PanePermissionAnswer>>,
    stop: &AtomicBool,
    chunk_tx: &UnboundedSender<SubagentEvent>,
) -> Result<TurnOutcome, String> {
    let http = server.http();
    let stopped = wait_for_stop(stop);
    tokio::pin!(stopped);

    let stream = tokio::select! {
        biased;
        () = &mut stopped => return Ok(cancelled(Some(session), String::new())),
        result = async {
            seed_plan(http, session, chunk_tx).await;
            http.open_stream("/event").await
        } => result?,
    };

    let mut events = TurnEvents::new(session, stream, chunk_tx);
    let mut acknowledged = false;
    let result = tokio::select! {
        biased;
        () = &mut stopped => None,
        result = finish_turn(
            dispatch(http, session, input, model, agent),
            consume_events(http, &mut events, permission_rx),
            &mut acknowledged,
        ) => Some(result),
    };

    match result {
        None => {
            let confirmed = abort_and_wait(http, &mut events, ABORT_GRACE).await;
            if let Err(error) = &confirmed {
                log::warn!("opencode: could not confirm cancellation of {session}: {error}");
            }
            // An unacknowledged request may still reach the server after the abort.
            if !acknowledged || confirmed.is_err() {
                server.shutdown();
            }
            Ok(cancelled(Some(session), events.closure.output()))
        }
        Some(Ok(())) => Ok(TurnOutcome {
            output: events.closure.output(),
            stop_reason: "end_turn".to_string(),
            session_id: Some(session.to_string()),
            failed: false,
        }),
        Some(Err(error)) => {
            server.shutdown();
            let error = auth_hint(error);
            let output = events.closure.output();
            if output.is_empty() {
                Err(error)
            } else {
                log::warn!("opencode: session {session} failed mid-turn: {error}");
                Ok(TurnOutcome {
                    output,
                    stop_reason: "error".to_string(),
                    session_id: None,
                    failed: true,
                })
            }
        }
    }
}

pub(super) fn cancelled(session: Option<&str>, output: String) -> TurnOutcome {
    TurnOutcome {
        output,
        stop_reason: "cancelled".to_string(),
        session_id: session.map(str::to_string),
        failed: false,
    }
}

pub(super) async fn wait_for_stop(stop: &AtomicBool) {
    let mut poll = tokio::time::interval(STOP_POLL);
    while !stop.load(Ordering::Relaxed) {
        poll.tick().await;
    }
}

async fn finish_turn(
    dispatch: impl Future<Output = Result<Dispatch, String>>,
    events: impl Future<Output = Result<(), String>>,
    acknowledged: &mut bool,
) -> Result<(), String> {
    tokio::pin!(dispatch, events);
    tokio::select! {
        result = &mut dispatch => {
            let dispatch = result?;
            *acknowledged = true;
            match dispatch {
                Dispatch::Prompt => events.await,
                Dispatch::Command => tokio::time::timeout(REQUEST_TIMEOUT, events)
                    .await
                    .map_err(|_| "opencode: no idle event after the command finished".to_string())?,
            }
        }
        result = &mut events => {
            result?;
            // The turn is idle. A synchronous command should now acknowledge it.
            tokio::time::timeout(REQUEST_TIMEOUT, dispatch)
                .await
                .map_err(|_| "opencode: timed out waiting for the turn response".to_string())?
                .map(drop)
        }
    }
}

async fn abort_and_wait(
    http: &Http,
    events: &mut TurnEvents<'_>,
    grace: Duration,
) -> Result<(), String> {
    let path = format!("/session/{}/abort", events.session);
    let wait_for_idle = async {
        while let Some(signals) = events.next().await? {
            if signals.iter().any(|signal| matches!(signal, Signal::Idle)) {
                return Ok(());
            }
        }
        Err("the event stream closed before the session was idle".to_string())
    };
    tokio::time::timeout(grace, async {
        tokio::try_join!(http.post_empty(&path), wait_for_idle).map(|((), ())| ())
    })
    .await
    .map_err(|_| "timed out waiting for the session to stop".to_string())?
}

struct TurnEvents<'a> {
    session: &'a str,
    stream: EventStream,
    mapper: Mapper,
    closure: TurnClosure,
    chunks: &'a UnboundedSender<SubagentEvent>,
}

impl<'a> TurnEvents<'a> {
    fn new(
        session: &'a str,
        stream: EventStream,
        chunks: &'a UnboundedSender<SubagentEvent>,
    ) -> Self {
        Self {
            session,
            stream,
            mapper: Mapper::new(false),
            closure: TurnClosure::default(),
            chunks,
        }
    }

    async fn next(&mut self) -> Result<Option<Vec<Signal>>, String> {
        let Some(event) = self.stream.next().await? else {
            return Ok(None);
        };
        let (events, signals) = self.mapper.map(&event, self.session);
        for event in events {
            self.closure.observe(&event);
            let _ = self.chunks.send(event);
        }
        Ok(Some(signals))
    }
}

async fn consume_events(
    http: &Http,
    events: &mut TurnEvents<'_>,
    mut permission_rx: Option<UnboundedReceiver<PanePermissionAnswer>>,
) -> Result<(), String> {
    let mut windows_cache = None;
    while let Some(signals) = events.next().await? {
        for signal in signals {
            match signal {
                Signal::Idle => return Ok(()),
                Signal::Error(error) => return Err(error),
                Signal::Permission(notice) => {
                    let answer = if let Some(rx) = &mut permission_rx {
                        let _ = events.chunks.send(SubagentEvent::PermissionRequest {
                            call_id: notice.id.clone(),
                            title: notice.title.clone(),
                            kind: notice.kind,
                            raw_input: notice.raw_input.clone(),
                            options: permission_options(),
                        });
                        permission_answer(rx.recv().await.as_ref())
                    } else {
                        api::answer::ONCE
                    };
                    let (path, payload) = notice.reply(events.session, answer);
                    http.post_ack(&path, &payload).await.map_err(|error| {
                        format!("opencode: could not answer permission: {error}")
                    })?;
                }
                Signal::Usage(usage) => {
                    if windows_cache.is_none() {
                        windows_cache = Some(fetch_context_windows(http).await);
                    }
                    let key = format!("{}/{}", usage.provider, usage.model);
                    if let Some(&window) = windows_cache
                        .as_ref()
                        .and_then(|windows| windows.get(&key))
                        .filter(|&&window| window > 0)
                    {
                        let _ = events.chunks.send(SubagentEvent::Usage {
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
                    let _ = events.chunks.send(SubagentEvent::Thought {
                        text: format!("[retry {attempt}] {message}"),
                    });
                }
                Signal::Title(title) => {
                    let _ = events
                        .chunks
                        .send(SubagentEvent::SessionInfo { title: Some(title) });
                }
            }
        }
    }
    Err("opencode: the event stream closed mid-turn".to_string())
}

enum Dispatch {
    Prompt,
    Command,
}

/// Known slash commands use the command endpoint. Other input is sent as text.
async fn dispatch(
    http: &Http,
    session: &str,
    input: &str,
    model: Option<api::ModelRef>,
    agent: Option<&str>,
) -> Result<Dispatch, String> {
    let agent = agent.filter(|agent| !agent.is_empty());
    if let Some((command, arguments)) = split_command(input)
        && advertised_command(http, command).await
    {
        let body = api::CommandBody {
            command,
            arguments,
            agent,
            model: model.map(|model| format!("{}/{}", model.provider, model.model)),
        };
        return http
            .post_command(&format!("/session/{session}/command"), &body)
            .await
            .map(|()| Dispatch::Command)
            .map_err(|error| format!("opencode: command /{command} failed: {error}"));
    }
    let body = PromptBody {
        parts: vec![PromptPart::Text { text: input }],
        model,
        agent,
    };
    http.post_ack(&format!("/session/{session}/prompt_async"), &body)
        .await
        .map(|()| Dispatch::Prompt)
        .map_err(|error| format!("opencode: the prompt was refused: {error}"))
}

async fn advertised_command(http: &Http, name: &str) -> bool {
    match http.get::<Vec<api::Command>>("/command").await {
        Ok(commands) => commands.iter().any(|command| command.name == name),
        Err(error) => {
            log::debug!("opencode: could not list commands: {error}");
            false
        }
    }
}

async fn seed_plan(http: &Http, session: &str, chunk_tx: &UnboundedSender<SubagentEvent>) {
    let path = format!("/session/{session}/todo");
    let Ok(todos) = http.get::<Vec<api::Todo>>(&path).await else {
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

async fn fetch_context_windows(http: &Http) -> HashMap<String, u64> {
    let Ok(providers) = http.get::<api::Providers>("/config/providers").await else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{TcpListener, TcpStream};

    async fn test_server() -> (Http, TcpListener) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let http = Http::new(
            format!("http://{}", listener.local_addr().unwrap()),
            None,
            None,
        );
        (http, listener)
    }

    async fn request(listener: &TcpListener, expected: &str) -> (TcpStream, Vec<u8>) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert_eq!(line.trim_end(), format!("{expected} HTTP/1.1"));

        let mut content_length = 0;
        loop {
            line.clear();
            assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                content_length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).await.unwrap();
        (reader.into_inner(), body)
    }

    async fn reply(mut stream: TcpStream, status: &str, body: &str) {
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    }

    async fn open_events(http: &Http, listener: &TcpListener) -> (EventStream, TcpStream) {
        let serve = async {
            let (mut stream, _) = request(listener, "GET /event").await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            stream
        };
        let (events, stream) = tokio::join!(http.open_stream("/event"), serve);
        (events.unwrap(), stream)
    }

    async fn emit(stream: &mut TcpStream, event: serde_json::Value) {
        stream
            .write_all(format!("data: {event}\n\n").as_bytes())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_command_rejection_ends_the_turn_while_sse_is_open() {
        let (http, listener) = test_server().await;
        let (stream, _live_stream) = open_events(&http, &listener).await;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut events = TurnEvents::new("ses_1", stream, &tx);
        let mut acknowledged = false;
        let turn = finish_turn(
            dispatch(&http, "ses_1", "/review changes", None, None),
            consume_events(&http, &mut events, None),
            &mut acknowledged,
        );
        let serve = async {
            let (catalog, _) = request(&listener, "GET /command").await;
            reply(catalog, "200 OK", r#"[{"name":"review"}]"#).await;
            let (command, body) = request(&listener, "POST /session/ses_1/command").await;
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                json!({
                    "command": "review", "arguments": "changes"
                })
            );
            reply(command, "400 Bad Request", "This command was rejected").await;
        };
        let (result, ()) =
            tokio::time::timeout(Duration::from_secs(3), async { tokio::join!(turn, serve) })
                .await
                .expect("the command failure should not wait for SSE");
        let error = result.unwrap_err();
        assert!(error.contains("400 Bad Request"), "{error}");
        assert!(error.contains("This command was rejected"), "{error}");
        assert!(!acknowledged);
    }

    #[tokio::test]
    async fn a_live_permission_uses_the_current_reply_endpoint() {
        let (http, listener) = test_server().await;
        let (sse, mut stream) = open_events(&http, &listener).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (answers, permission_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut events = TurnEvents::new("ses_1", sse, &tx);
        let turn = consume_events(&http, &mut events, Some(permission_rx));
        let serve = async {
            for (id, session) in [("per_other", "ses_other"), ("per_1", "ses_1")] {
                emit(
                    &mut stream,
                    json!({
                        "type": "permission.asked",
                        "properties": {
                            "id": id, "sessionID": session, "permission": "bash",
                            "patterns": ["cargo test"], "metadata": {"command": "cargo test"},
                            "always": ["cargo test *"]
                        }
                    }),
                )
                .await;
            }
            let Some(SubagentEvent::PermissionRequest {
                call_id,
                title,
                kind,
                raw_input,
                ..
            }) = rx.recv().await
            else {
                panic!("expected permission request");
            };
            assert_eq!(call_id, "per_1");
            assert_eq!(title, "bash: cargo test");
            assert_eq!(kind, crate::events::ToolKind::Execute);
            assert_eq!(raw_input, Some(json!({ "command": "cargo test" })));
            answers
                .send(PanePermissionAnswer::Selected(
                    api::answer::REJECT.to_string(),
                ))
                .unwrap();
            let (permission, body) = request(&listener, "POST /permission/per_1/reply").await;
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                json!({ "reply": "reject" })
            );
            reply(permission, "200 OK", "true").await;
            emit(
                &mut stream,
                json!({
                    "type": "session.idle", "properties": { "sessionID": "ses_1" }
                }),
            )
            .await;
        };
        let (result, ()) =
            tokio::time::timeout(Duration::from_secs(3), async { tokio::join!(turn, serve) })
                .await
                .expect("the session should resume after the permission reply");
        result.unwrap();
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn cancellation_deadline_includes_the_abort_request() {
        let (http, listener) = test_server().await;
        let (stream, _live_stream) = open_events(&http, &listener).await;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut events = TurnEvents::new("ses_1", stream, &tx);
        let abort = abort_and_wait(&http, &mut events, Duration::from_millis(200));
        let serve = request(&listener, "POST /session/ses_1/abort");
        let (result, (_unanswered_request, _)) =
            tokio::time::timeout(Duration::from_secs(3), async { tokio::join!(abort, serve) })
                .await
                .expect("a stalled abort must time out");
        assert_eq!(
            result.unwrap_err(),
            "timed out waiting for the session to stop"
        );
    }

    #[tokio::test]
    async fn cancellation_preserves_events_up_to_idle() {
        let (http, listener) = test_server().await;
        let (sse, mut stream) = open_events(&http, &listener).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut events = TurnEvents::new("ses_1", sse, &tx);
        let abort = abort_and_wait(&http, &mut events, Duration::from_secs(3));
        let serve = async {
            let (request, _) = request(&listener, "POST /session/ses_1/abort").await;
            emit(
                &mut stream,
                json!({
                    "type": "session.idle", "properties": { "sessionID": "ses_other" }
                }),
            )
            .await;
            emit(&mut stream, json!({
                "type": "message.updated",
                "properties": { "info": { "id": "msg_1", "sessionID": "ses_1", "role": "assistant" } }
            })).await;
            emit(
                &mut stream,
                json!({
                    "type": "message.part.updated",
                    "properties": { "part": {
                        "type": "text", "id": "part_1", "sessionID": "ses_1", "messageID": "msg_1",
                        "text": "Partial answer"
                    } }
                }),
            )
            .await;
            emit(
                &mut stream,
                json!({
                    "type": "session.status",
                    "properties": { "sessionID": "ses_1", "status": { "type": "idle" } }
                }),
            )
            .await;
            reply(request, "200 OK", "true").await;
        };
        let (result, ()) =
            tokio::time::timeout(Duration::from_secs(3), async { tokio::join!(abort, serve) })
                .await
                .unwrap();
        result.unwrap();
        assert_eq!(events.closure.output(), "Partial answer");
        assert_eq!(
            rx.try_recv().unwrap(),
            SubagentEvent::Message {
                text: "Partial answer".to_string()
            }
        );
    }

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
