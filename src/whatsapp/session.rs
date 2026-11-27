//! One WhatsApp chat's turn worker: the main harness, ACP instances and
//! fan-out.
//!
//! Each chat gets its own worker (see [`super::agent`]) and therefore its own
//! [`ChatSession`]. The worker owns the live ACP handles for that chat; the
//! durable mode, roster, session ids and main-agent context live in the shared
//! [`ChatControl`], because slash commands change them immediately while a
//! turn may still be running.
//!
//! A main-agent turn assembles a fresh harness (the ONE assembly in
//! `crate::assembly`), runs it while draining its events, and stores the
//! returned context. An external turn drives one ACP instance through
//! [`super::executor`]. `@all` runs one turn per instance concurrently and
//! labels every reply with the agent (and instance) that produced it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinSet;

use morpheus_agents::model_picks::ModelPicks;
use morpheus_tools::question::types::AnswerItem;

use crate::harness::{ContextManagerState, Harness, HarnessEvent, Mode, PermissionAction};
use crate::setup::data_dir_override;

use super::config::RuntimeConfig;
use super::control::ChatControl;
use super::delivery::ReplyBatch;
use super::executor::{self, AcpSession};
use super::reply::{self, truncate};
use super::selection::{self, Route};

/// How the agent loop of one turn ended.
#[derive(Default)]
struct Turn {
    /// Streamed assistant text, kept as a fallback when the context manager
    /// has no sealed final answer (e.g. a stopped turn).
    tokens: String,
    /// The context snapshot returned by `Done`/`Stopped`/`Error`.
    context: Option<ContextManagerState>,
    /// The fatal error message, when the turn failed.
    error: Option<String>,
    /// Whether the turn was interrupted.
    stopped: bool,
    /// Tools the loop asked approval for. There is no approval dialog on
    /// WhatsApp, so they were denied; the reply tells the user why.
    denied_tools: Vec<String>,
}

impl Turn {
    /// Collect one harness event, answering the loop where it would
    /// otherwise block forever waiting for a UI.
    fn absorb(
        &mut self,
        event: HarnessEvent,
        answer_tx: &mpsc::UnboundedSender<Result<Vec<AnswerItem>, String>>,
        perm_tx: &mpsc::UnboundedSender<PermissionAction>,
    ) {
        match event {
            HarnessEvent::Token { text } => self.tokens.push_str(&text),
            HarnessEvent::Done { context, .. } => self.context = Some(context),
            HarnessEvent::Stopped { context } => {
                self.context = Some(context);
                self.stopped = true;
            }
            HarnessEvent::Error {
                message, context, ..
            } => {
                self.error = Some(message);
                if context.is_some() {
                    self.context = context;
                }
            }
            HarnessEvent::PermissionRequest { tool, .. } => {
                // Deny rather than hang: the loop waits for an answer, and
                // there is no interactive surface here. Denying is the safe
                // choice; `/mode yolo` is the explicit opt-in to run tools
                // without approval.
                if !self.denied_tools.contains(&tool) {
                    self.denied_tools.push(tool);
                }
                let _ = perm_tx.send(PermissionAction::Deny);
            }
            HarnessEvent::QuestionRequest { .. } => {
                // `ask_questions` is hidden from the model on this surface,
                // but a stale registration must not wedge the loop.
                let _ =
                    answer_tx.send(Err("Interactive questions are not available on WhatsApp. \
                     Ask the user directly in your reply instead."
                        .to_string()));
            }
            HarnessEvent::Toast { message, .. } => {
                // Toasts are TUI chrome; keep them observable without
                // spamming the chat.
                log::warn!("morpheus whatsapp: {message}");
            }
            _ => {}
        }
    }

    /// The user-facing reply for this turn.
    fn reply(self, harness: &Harness) -> String {
        let body = if let Some(error) = self.error {
            format!("The agent could not complete this turn: {error}")
        } else {
            harness
                .context_manager
                .final_answer()
                .filter(|answer| !answer.trim().is_empty())
                .or_else(|| {
                    let tokens = self.tokens.trim();
                    (!tokens.is_empty()).then(|| tokens.to_string())
                })
                .unwrap_or_else(|| {
                    if self.stopped {
                        "The turn was stopped before it produced a reply.".to_string()
                    } else {
                        "The agent finished without a textual reply.".to_string()
                    }
                })
        };

        let mut reply = truncate(body);
        if !self.denied_tools.is_empty() {
            reply.push_str(&format!(
                "\n\n_Tools requiring approval were denied (no approval dialog on WhatsApp): {}. \
                 Use `/mode yolo` if you want them to run._",
                self.denied_tools.join(", ")
            ));
        }
        reply
    }
}

/// The state of one WhatsApp chat's worker.
pub struct ChatSession {
    config: Arc<RuntimeConfig>,
    chat_key: String,
    /// Mode, roster, durable sessions and context, shared with the router.
    control: Arc<Mutex<ChatControl>>,
    /// Live ACP instances keyed by `agent#index`.
    acp: HashMap<String, AcpSession>,
}

impl ChatSession {
    /// Start a chat worker in the user's persisted mode, with no history.
    #[must_use]
    pub fn new(
        config: Arc<RuntimeConfig>,
        chat_key: String,
        control: Arc<Mutex<ChatControl>>,
    ) -> Self {
        Self {
            config,
            chat_key,
            control,
            acp: HashMap::new(),
        }
    }

    /// Execute one routed message and return the replies to send, in order.
    ///
    /// `humanize` was captured when the message was admitted, so a `/human`
    /// change never affects a message already in the queue.
    pub async fn handle_route(&mut self, route: Route, humanize: bool) -> ReplyBatch {
        // Command mode never consults routing: every message is a shell
        // command. The router queues Main for it (see `agent.rs`).
        if self.control.lock().await.mode() == Mode::Command
            && let Route::Main { body } | Route::Passthrough(body) = route
        {
            return ReplyBatch::new(vec![self.run_shell(&body).await], humanize);
        }
        match route {
            Route::Main { body } | Route::Passthrough(body) => {
                ReplyBatch::new(vec![self.run_turn(&body).await], humanize)
            }
            Route::Agent {
                agent,
                index,
                total,
                body,
            } => ReplyBatch::new(
                vec![self.agent_turn(&agent, index, total, &body).await],
                humanize,
            ),
            Route::Fanout { body } => ReplyBatch::new(self.fanout(&body).await, humanize),
            // Routing hints are control-plane answers, like commands.
            Route::Hint(text) => ReplyBatch::plain(vec![text]),
        }
    }

    /// Assemble the harness for this turn and run the agent loop, answering
    /// the loop's UI requests and waiting for it to finish.
    async fn run_turn(&mut self, input: &str) -> String {
        let (mode, context, stop_signal) = {
            let mut control = self.control.lock().await;
            control.begin_turn();
            (control.mode(), control.context(), control.stop_signal())
        };
        let assembly = self.config.assembly(
            mode,
            context,
            Some(crate::assembly::session_affinity_id(&format!(
                "whatsapp:{}",
                self.chat_key
            ))),
        );
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let mut harness = match crate::assembly::assemble(assembly, &event_tx).await {
            Ok(harness) => harness,
            Err(error) => {
                self.control.lock().await.end_turn();
                return format!("Could not start the agent for this chat: {error}");
            }
        };

        let (answer_tx, answer_rx) = mpsc::unbounded_channel();
        let (perm_tx, perm_rx) = mpsc::unbounded_channel();
        let mut turn = Turn::default();

        {
            let mut running = std::pin::pin!(harness.run_agent_loop(
                input,
                event_tx,
                answer_rx,
                perm_rx,
                stop_signal,
            ));
            loop {
                tokio::select! {
                    _ = &mut running => break,
                    event = event_rx.recv() => match event {
                        Some(event) => turn.absorb(event, &answer_tx, &perm_tx),
                        None => break,
                    },
                }
            }
        }

        // `run_agent_loop` returns right after sending its terminal event;
        // drain what is still queued so `Done` is never missed.
        while let Ok(event) = event_rx.try_recv() {
            turn.absorb(event, &answer_tx, &perm_tx);
        }

        // A turn that never reached the context manager keeps the previous
        // snapshot instead of erasing the conversation history.
        if let Some(context) = turn.context.take() {
            let mut control = self.control.lock().await;
            if let Err(error) = control.set_context(Some(context)).await {
                log::warn!("morpheus whatsapp: could not persist the chat context: {error}");
            }
            control.end_turn();
        } else {
            self.control.lock().await.end_turn();
        }
        turn.reply(&harness)
    }

    /// Run one ACP instance and format its labeled reply.
    async fn agent_turn(&mut self, agent: &str, index: usize, total: usize, input: &str) -> String {
        let (mode, stop_signal, yolo) = {
            let mut control = self.control.lock().await;
            control.begin_turn();
            let mode = control.mode();
            (mode, control.stop_signal(), mode == Mode::Yolo)
        };
        let reply = match self
            .acp_turn(agent, index, total, input, mode, stop_signal, yolo)
            .await
        {
            Ok(text) => text,
            Err(error) => reply::format_agent_error(agent, index, total, &error),
        };
        self.control.lock().await.end_turn();
        reply
    }

    /// Drive one instance, reusing or reopening its live session.
    async fn acp_turn(
        &mut self,
        agent: &str,
        index: usize,
        total: usize,
        input: &str,
        mode: Mode,
        stop_signal: Arc<AtomicBool>,
        yolo: bool,
    ) -> Result<String, String> {
        let key = selection::instance_key(agent, index);
        let cwd = executor::session_cwd(&self.config.cwd);
        self.ensure_session(agent, &key, &cwd).await?;

        let model = ModelPicks::new(&data_dir_override().join("agents"))
            .get(agent)
            .map(str::to_string);
        let (handle, modes, options) = {
            let session = self.acp.get(&key).ok_or("the ACP session vanished")?;
            (
                session.handle(),
                session.modes.clone(),
                session.config_options.clone(),
            )
        };

        let result = executor::run(
            handle,
            input.to_string(),
            modes,
            options,
            mode,
            model,
            yolo,
            stop_signal,
        )
        .await;

        log::debug!(
            "morpheus whatsapp: {} turn ended ({})",
            key,
            result.stop_reason
        );
        if let Some(session) = self.acp.get_mut(&key) {
            session.absorb(&result);
        }
        if result.failed {
            if let Some(session) = self.acp.remove(&key) {
                session.shutdown();
            }
            return Err(result.output);
        }
        if let Some(id) = result.session_id {
            if let Err(error) = self.control.lock().await.record_session(&key, id).await {
                log::warn!("morpheus whatsapp: could not persist the ACP session id: {error}");
            }
        }

        let mut body = result.output;
        if let Some(note) = reply::denial_note(&result.denied_tools) {
            body.push_str(&note);
        }
        Ok(reply::format_agent_reply(agent, index, total, body))
    }

    /// One concurrent turn per selected instance; replies in completion order.
    async fn fanout(&mut self, input: &str) -> Vec<String> {
        let roster = self.control.lock().await.roster().to_vec();
        let targets = selection::entries(&roster);
        if targets.is_empty() {
            return vec![
                "No external agents are selected. Send `/agent <name>` first.".to_string(),
            ];
        }

        let (mode, stop_signal, yolo) = {
            let mut control = self.control.lock().await;
            control.begin_turn();
            let mode = control.mode();
            (mode, control.stop_signal(), mode == Mode::Yolo)
        };
        let cwd = executor::session_cwd(&self.config.cwd);

        // Open the sessions first (sequential, cheap) so the turns can then
        // run concurrently without borrowing `self`.
        let mut prepared = Vec::new();
        for entry in &targets {
            let key = selection::instance_key(&entry.agent, entry.index);
            if let Err(error) = self.ensure_session(&entry.agent, &key, &cwd).await {
                prepared.push(Err((entry.clone(), error)));
                continue;
            }
            let session = self.acp.get(&key).expect("the session was just ensured");
            prepared.push(Ok((
                entry.clone(),
                key,
                session.handle(),
                session.modes.clone(),
                session.config_options.clone(),
            )));
        }

        let input = input.to_string();
        let mut turns = JoinSet::new();
        let mut immediate = Vec::new();
        for item in prepared {
            match item {
                Err((entry, error)) => immediate.push(reply::format_agent_error(
                    &entry.agent,
                    entry.index,
                    entry.total,
                    &error,
                )),
                Ok((entry, key, handle, modes, options)) => {
                    let model = ModelPicks::new(&data_dir_override().join("agents"))
                        .get(&entry.agent)
                        .map(str::to_string);
                    let stop = Arc::clone(&stop_signal);
                    let turn_input = input.clone();
                    turns.spawn(async move {
                        let result = executor::run(
                            handle, turn_input, modes, options, mode, model, yolo, stop,
                        )
                        .await;
                        (entry, key, result)
                    });
                }
            }
        }

        let mut replies = immediate;
        while let Some(joined) = turns.join_next().await {
            let Ok((entry, key, result)) = joined else {
                log::error!("morpheus whatsapp: an @all turn panicked");
                continue;
            };
            if let Some(session) = self.acp.get_mut(&key) {
                session.absorb(&result);
            }
            if result.failed {
                if let Some(session) = self.acp.remove(&key) {
                    session.shutdown();
                }
                replies.push(reply::format_agent_error(
                    &entry.agent,
                    entry.index,
                    entry.total,
                    &result.output,
                ));
                continue;
            }
            if let Some(id) = result.session_id {
                if let Err(error) = self.control.lock().await.record_session(&key, id).await {
                    log::warn!("morpheus whatsapp: could not persist the ACP session id: {error}");
                }
            }
            let mut body = result.output;
            if let Some(note) = reply::denial_note(&result.denied_tools) {
                body.push_str(&note);
            }
            replies.push(reply::format_agent_reply(
                &entry.agent,
                entry.index,
                entry.total,
                body,
            ));
        }
        self.control.lock().await.end_turn();
        replies
    }

    /// Reuse a live session when it is usable; otherwise open (and resume)
    /// a new one. The router's persisted id survives a restart.
    async fn ensure_session(
        &mut self,
        agent: &str,
        key: &str,
        cwd: &PathBuf,
    ) -> Result<(), String> {
        let reusable = self
            .acp
            .get(key)
            .is_some_and(|session| session.is_usable(cwd));
        if reusable {
            return Ok(());
        }
        if let Some(stale) = self.acp.remove(key) {
            stale.shutdown();
        }
        let resume = self.control.lock().await.session_id(key);
        let session = AcpSession::open(agent, resume, cwd)?;
        let handle = session.handle();
        self.control
            .lock()
            .await
            .register_handle(key.to_string(), handle);
        self.acp.insert(key.to_string(), session);
        Ok(())
    }

    /// Command mode: the message IS a shell command. No LLM call, no
    /// context mutation - the conversation history is preserved untouched.
    async fn run_shell(&self, command: &str) -> String {
        use crate::harness::Tools;

        if command.trim().is_empty() {
            return "Send a shell command, or `/mode build` to return to the agent.".to_string();
        }

        if self.config.disabled_tools.contains("bash_run") {
            return "Shell commands are disabled in your tool settings (`bash_run` is off). \
                    Re-enable it in `.morpheus-tools.json` or `setup.json` to use command mode."
                .to_string();
        }

        let tools = crate::harness::MorpheusTools::new(&self.config.cwd);
        match tools
            .dispatch("bash_run", serde_json::json!({ "command": command }))
            .await
        {
            Ok(output) if output.trim().is_empty() => {
                "Command finished with no output.".to_string()
            }
            Ok(output) => truncate(output),
            Err(error) => format!("Command failed: {error}"),
        }
    }
}
