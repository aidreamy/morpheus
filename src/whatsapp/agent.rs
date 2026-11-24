//! Inbound-message routing: one conversation → one worker.
//!
//! WhatsApp delivers messages from every chat on one event stream. A chat's
//! harness is stateful (its context, its mode), so messages from the same
//! chat must run one at a time, while different chats may run in parallel.
//! The routing layer is therefore a map of chat → worker:
//!
//! - slash commands are answered immediately from the chat's control state
//!   and never enter the queue;
//! - every other message is routed (`@name`, `@all`, no prefix → main) and
//!   sent to the chat's FIFO queue;
//! - the worker owns the harness and the live ACP instances, and processes
//!   requests in order.
//!
//! This mirrors the `serve` loop of `morpheus-whatsapp` (which also isolates
//! conversations) while keeping the session state that `serve` deliberately
//! does not own. Which messages are prompts at all is not decided here:
//! `WhatsApp::is_prompt` owns that rule, so this interface and the crate's
//! `serve` cannot drift apart on self chats, echoes or noise.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};

use morpheus_whatsapp::IncomingMessage;

use super::commands;
use super::config::RuntimeConfig;
use super::control::ChatControl;
use super::selection::{self, Route};
use super::session::ChatSession;
use super::state::ChatStore;

/// One routed message waiting for the chat's worker.
struct TurnRequest {
    route: Route,
    reply: oneshot::Sender<Vec<String>>,
}

/// Routes inbound messages to their per-chat session.
pub struct WhatsAppAgent {
    config: Arc<RuntimeConfig>,
    store: ChatStore,
    /// Mode, roster and state per chat, shared with that chat's worker.
    controls: Mutex<HashMap<String, Arc<AsyncMutex<ChatControl>>>>,
    sessions: Mutex<HashMap<String, mpsc::UnboundedSender<TurnRequest>>>,
}

impl WhatsAppAgent {
    /// Build the router over an already-resolved configuration.
    #[must_use]
    pub fn new(config: RuntimeConfig) -> Self {
        Self::with_store(config, ChatStore::new())
    }

    /// [`Self::new`] with an explicit state store (tests, custom data dirs).
    #[must_use]
    pub fn with_store(config: RuntimeConfig, store: ChatStore) -> Self {
        Self {
            config: Arc::new(config),
            store,
            controls: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Handle one inbound message and return every reply to send, in order.
    pub async fn handle(&self, message: &IncomingMessage) -> Vec<String> {
        let Some(text) = reply_text(message) else {
            return vec![
                "I can only handle text messages on WhatsApp right now. \
                 Send the request as text."
                    .to_string(),
            ];
        };
        let text = text.trim();
        if text.is_empty() {
            return vec!["Send a message or `/help` to see the commands.".to_string()];
        }

        let key = message.chat.storage_key();

        // Commands are control-plane traffic: they are answered from the
        // chat's control state and never enter the turn queue. That is what
        // keeps `/help` and `/agent ls` responsive during a long turn.
        if let Some(command) = commands::parse(text) {
            let control = self.control_for(&key).await;
            return vec![control.lock().await.handle_command(command).await];
        }

        let control = self.control_for(&key).await;
        let route = {
            let control = control.lock().await;
            if control.mode() == crate::harness::Mode::Command {
                // Every message is a shell command in Command mode; routing
                // would strip an address the shell should see verbatim.
                Route::Main {
                    body: text.to_string(),
                }
            } else {
                selection::route(text, control.roster(), control.installed())
            }
        };

        match self.enqueue(key, route).await {
            Some(replies) => replies,
            None => vec!["The agent session for this chat ended; send the message again.".into()],
        }
    }

    /// Send one route to the chat's worker; retry once against a fresh worker
    /// if the previous one died (most likely a panic).
    async fn enqueue(&self, key: String, route: Route) -> Option<Vec<String>> {
        let tx = self.channel_for(key.clone()).await;
        let (reply_tx, reply_rx) = oneshot::channel();
        if tx
            .send(TurnRequest {
                route: route.clone(),
                reply: reply_tx,
            })
            .is_err()
        {
            if let Ok(mut sessions) = self.sessions.lock() {
                sessions.remove(&key);
            }
            let tx = self.channel_for(key).await;
            let (reply_tx, reply_rx) = oneshot::channel();
            if tx
                .send(TurnRequest {
                    route,
                    reply: reply_tx,
                })
                .is_err()
            {
                return None;
            }
            return reply_rx.await.ok();
        }
        reply_rx.await.ok()
    }

    /// The control state for `chat_key`, loading it on first use.
    async fn control_for(&self, chat_key: &str) -> Arc<AsyncMutex<ChatControl>> {
        if let Some(control) = self
            .controls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(chat_key)
        {
            return Arc::clone(control);
        }

        // Load outside the lock: the store read is async, and holding a
        // `std::sync::Mutex` across an await blocks the whole runtime.
        let loaded = Arc::new(AsyncMutex::new(
            ChatControl::load(
                self.store.clone(),
                chat_key.to_string(),
                self.config.initial_mode,
            )
            .await,
        ));
        let mut controls = self
            .controls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Another message for the same chat may have loaded it first; the
        // first control wins so both paths share one roster and mode.
        Arc::clone(controls.entry(chat_key.to_string()).or_insert(loaded))
    }

    /// The worker queue for `chat_key`, spawning the worker on first use.
    async fn channel_for(&self, chat_key: String) -> mpsc::UnboundedSender<TurnRequest> {
        if let Some(tx) = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&chat_key)
        {
            return tx.clone();
        }

        let control = self.control_for(&chat_key).await;
        let config = Arc::clone(&self.config);

        // Spawn and insert under the same lock, so a concurrent first message
        // cannot start a second worker for the same chat.
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(tx) = sessions.get(&chat_key) {
            return tx.clone();
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let key = chat_key.clone();
        tokio::spawn(async move {
            let mut session = ChatSession::new(config, key, control);
            run_worker(&mut session, rx).await;
        });
        sessions.insert(chat_key, tx.clone());
        tx
    }
}

/// Serialize one chat's prompts through its session.
async fn run_worker(session: &mut ChatSession, mut rx: mpsc::UnboundedReceiver<TurnRequest>) {
    while let Some(request) = rx.recv().await {
        let replies = session.handle_route(request.route).await;
        // The requester may have gone away (link closed); the turn's result
        // still belongs to the session context, which was already updated.
        let _ = request.reply.send(replies);
    }
}

/// The text an inbound message contributes, if any.
///
/// Media captions are treated as text: they are the user's words about the
/// attachment. The attachment bytes themselves are out of scope (the crate
/// models metadata only).
fn reply_text(message: &IncomingMessage) -> Option<&str> {
    message.text().or_else(|| message.caption())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::setup::Setup;
    use morpheus_whatsapp::{ChatId, MessageContent, Sender};

    fn message(chat: &str, content: MessageContent) -> IncomingMessage {
        let chat_id = ChatId::parse(chat).expect("chat id");
        IncomingMessage {
            id: "MSG1".to_string(),
            chat_kind: chat_id.kind(),
            chat: chat_id,
            sender: Sender {
                id: ChatId::parse(chat).expect("sender id"),
                name: None,
            },
            sender_alt: None,
            from_me: false,
            timestamp: Utc::now(),
            content,
        }
    }

    const DIRECT: &str = "5511912345678@s.whatsapp.net";

    #[test]
    fn direct_text_is_the_prompt() {
        let text = message(DIRECT, MessageContent::Text("hello".into()));
        assert_eq!(reply_text(&text), Some("hello"));
    }

    #[tokio::test]
    async fn commands_are_answered_before_a_worker_exists() {
        let dir = tempfile::tempdir().expect("temp dir");
        let config = RuntimeConfig::from_setup(&Setup::default(), "/tmp/project");
        let agent = WhatsAppAgent::with_store(config, ChatStore::at(dir.path()));

        let help = agent
            .handle(&message(DIRECT, MessageContent::Text("/help".into())))
            .await;
        assert_eq!(help.len(), 1, "a command is one message");
        assert!(help[0].contains("/agent"), "{}", help[0]);
        assert!(
            agent.sessions.lock().expect("sessions").is_empty(),
            "a command must not spawn a turn worker"
        );
        assert_eq!(agent.controls.lock().expect("controls").len(), 1);

        let list = agent
            .handle(&message(DIRECT, MessageContent::Text("/agent ls".into())))
            .await;
        assert!(list[0].contains("Selected: none"), "{}", list[0]);

        let stop = agent
            .handle(&message(DIRECT, MessageContent::Text("/stop".into())))
            .await;
        assert_eq!(stop, ["Nothing is running."]);

        let off = agent
            .handle(&message(DIRECT, MessageContent::Text("/agent off".into())))
            .await;
        assert!(off[0].contains("External agents are off"), "{}", off[0]);
        assert!(
            agent.sessions.lock().expect("sessions").is_empty(),
            "commands must never spawn a turn worker"
        );
    }

    #[test]
    fn media_captions_are_treated_as_text() {
        let image = message(
            DIRECT,
            MessageContent::Image {
                caption: Some("/help".into()),
                mime_type: Some("image/jpeg".into()),
            },
        );
        assert_eq!(reply_text(&image), Some("/help"));

        let uncaptioned = message(
            DIRECT,
            MessageContent::Image {
                caption: None,
                mime_type: None,
            },
        );
        assert_eq!(reply_text(&uncaptioned), None);
    }
}
