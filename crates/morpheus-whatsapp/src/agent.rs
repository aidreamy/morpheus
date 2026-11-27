//! The agent integration: one message in, one reply out.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::{JoinError, JoinSet};

use crate::client::WhatsApp;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::event::Event;
use crate::message::IncomingMessage;
use crate::qr;

/// How many agent calls may run at once. Replies are independent, so a slow
/// answer must not hold up the messages behind it, but unbounded parallelism
/// would let a burst of chats hammer the model at the same time.
const MAX_IN_FLIGHT: usize = 4;

/// Turns inbound WhatsApp messages into replies.
///
/// Implementations get the live [`WhatsApp`] handle too, so a reply is not
/// limited to one string: an agent can send several messages, target another
/// chat, or react, while the returned text is simply the common case.
///
/// The trait is dyn-compatible, so an agent can be chosen at runtime:
/// `let agent: Box<dyn Agent> = ...; serve(config, agent)`.
#[async_trait]
pub trait Agent: Send + Sync {
    /// Handle one inbound message.
    ///
    /// Return `Ok(Some(text))` to reply in the same chat, `Ok(None)` to stay
    /// silent, or an error to log and skip the message. [`serve`] asks
    /// [`WhatsApp::is_prompt`] first, so the agent is not handed
    /// status/broadcast posts, channel messages, protocol chatter, reactions
    /// or the account's own traffic addressed elsewhere - but *is* handed
    /// what the user types in this account's own chat. Use
    /// [`WhatsApp::connect`] directly if reactions should reach the agent.
    ///
    /// Calls for different messages run concurrently (up to four at a time),
    /// so implementations must not assume they are serialized.
    async fn on_message(
        &self,
        message: &IncomingMessage,
        whatsapp: &WhatsApp,
    ) -> Result<Option<String>>;
}

#[async_trait]
impl Agent for Box<dyn Agent> {
    async fn on_message(
        &self,
        message: &IncomingMessage,
        whatsapp: &WhatsApp,
    ) -> Result<Option<String>> {
        (**self).on_message(message, whatsapp).await
    }
}

/// Run `agent` against a WhatsApp link until the link stops or the account is
/// logged out.
///
/// This is the batteries-included entry point: it connects, renders pairing
/// QR codes to the log, forwards inbound messages to the agent and sends the
/// replies. Use [`WhatsApp::connect`] directly when the caller owns the UI
/// (a TUI, an HTTP server) and needs the raw [`Event`] stream.
///
/// Messages are handled in spawned tasks, up to four at a time, so a slow
/// reply (an LLM call, for instance) does not stall lifecycle events or other
/// conversations. A slot is reserved **before** the task is spawned, so a
/// burst cannot pile up waiting tasks: a message that arrives while all four
/// slots are busy is shed with a logged warning instead. Lifecycle events
/// (pairing, connection, failures, logout) wait for room in the event queue
/// instead of being dropped immediately.
///
/// Finished tasks are reaped on every iteration and a panicking agent is
/// logged instead of disappearing silently.
///
/// Pairing instructions are logged at `info`; a QR code appears in the log
/// for each code the server issues. The status of every event is logged, so
/// a normal `env_logger` setup is enough to link an account.
///
/// Returns `Ok(())` only when the account is logged out (the intended way to
/// stop). A link that ends for any other reason - a fatal failure such as
/// another session taking over, or the event stream closing - returns an
/// error, so a supervisor can restart or report it.
pub async fn serve<A: Agent + 'static>(config: Config, agent: A) -> Result<()> {
    let (whatsapp, mut events) = WhatsApp::connect(config).await?;
    let agent = Arc::new(agent);
    let in_flight = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    let mut tasks = JoinSet::new();

    let outcome = loop {
        // Collect finished replies (and surface panics) before waiting for
        // the next event, so the set does not grow for the process lifetime.
        reap(&mut tasks);

        let Some(event) = events.recv().await else {
            break Err(Error::Stopped);
        };

        match event {
            Event::QrCode { payload, .. } => match qr::render(&payload) {
                Ok(code) => log::info!(
                    "Scan this QR code from WhatsApp → Linked devices → Link a device:\n{code}"
                ),
                Err(err) => log::error!("morpheus-whatsapp: {err}"),
            },
            Event::PairingCode { code, .. } => log::info!(
                "On the phone: WhatsApp → Linked devices → Link with phone number instead, \
                 then type {code}"
            ),
            Event::PairingFailed { reason } => {
                log::warn!("morpheus-whatsapp: pairing failed: {reason}")
            }
            Event::Connected => log::info!("morpheus-whatsapp: connected"),
            Event::Disconnected => {
                log::warn!("morpheus-whatsapp: connection dropped, reconnecting")
            }
            Event::LoggedOut => {
                log::warn!("morpheus-whatsapp: device unlinked from the phone; stopping");
                break Ok(());
            }
            Event::Failure { kind, reason } if kind.is_fatal() => {
                log::error!("morpheus-whatsapp: unrecoverable failure: {reason}");
                break Err(Error::LinkFailed { kind, reason });
            }
            Event::Failure { reason, .. } => {
                log::warn!("morpheus-whatsapp: {reason}; the engine retries with backoff");
            }
            Event::Message(message) => {
                if !whatsapp.is_prompt(&message) {
                    continue;
                }

                let Some(permit) = take_slot(&in_flight) else {
                    log::warn!(
                        "morpheus-whatsapp: all {MAX_IN_FLIGHT} agent slots are busy, dropped message {} from {}",
                        message.id,
                        message.sender.id
                    );
                    continue;
                };

                let agent = Arc::clone(&agent);
                let whatsapp = whatsapp.clone();
                tasks.spawn(async move {
                    // Holding the permit for the whole reply keeps the cap
                    // honest; it is released when the task ends.
                    let _permit = permit;
                    let reply = agent.on_message(&message, &whatsapp).await;

                    match reply {
                        Ok(Some(reply)) if !reply.trim().is_empty() => {
                            if let Err(err) = whatsapp.send_text(&message.chat, reply).await {
                                log::warn!(
                                    "morpheus-whatsapp: reply to {} failed: {err}",
                                    message.chat
                                );
                            }
                        }
                        Ok(_) => {}
                        Err(err) => log::warn!(
                            "morpheus-whatsapp: agent failed on a message from {}: {err}",
                            message.sender.id
                        ),
                    }
                });
            }
        }
    };

    // Do not let in-flight replies outlive the link they would write to.
    tasks.abort_all();
    while let Some(finished) = tasks.join_next().await {
        observe(finished);
    }
    whatsapp.close().await;

    outcome
}

/// Reserve one of the agent slots, or report that the loop is saturated.
fn take_slot(in_flight: &Arc<Semaphore>) -> Option<OwnedSemaphorePermit> {
    Arc::clone(in_flight).try_acquire_owned().ok()
}

/// Reap every finished task, logging agent panics.
fn reap(tasks: &mut JoinSet<()>) {
    while let Some(finished) = tasks.try_join_next() {
        observe(finished);
    }
}

fn observe(finished: std::result::Result<(), JoinError>) {
    if let Err(err) = finished
        && err.is_panic()
    {
        log::error!("morpheus-whatsapp: an agent task panicked: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Recorder;

    #[async_trait]
    impl Agent for Recorder {
        async fn on_message(
            &self,
            _message: &IncomingMessage,
            _whatsapp: &WhatsApp,
        ) -> Result<Option<String>> {
            Ok(Some("ack".to_string()))
        }
    }

    #[tokio::test]
    async fn slots_are_reserved_before_spawning() {
        let in_flight = Arc::new(Semaphore::new(1));
        let slot = take_slot(&in_flight).expect("first message gets a slot");
        // A second message is shed instead of spawning a task that waits.
        assert!(take_slot(&in_flight).is_none());
        drop(slot);
        assert!(take_slot(&in_flight).is_some());
    }

    #[tokio::test]
    async fn finished_tasks_are_reaped_and_panics_surface() {
        let mut tasks = JoinSet::new();
        tasks.spawn(async {});
        tasks.spawn(async { panic!("agent blew up") });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        assert_eq!(tasks.len(), 2, "both tasks are still owned by the set");
        reap(&mut tasks);
        assert!(tasks.is_empty(), "finished tasks must be reaped");
    }

    #[test]
    fn agent_is_dyn_compatible() {
        fn takes_agent<A: Agent + 'static>(_: A) {}

        let agent: Box<dyn Agent> = Box::new(Recorder);
        // Both `&dyn Agent` and `Box<dyn Agent>` satisfy the trait.
        let _ = agent.as_ref() as &dyn Agent;
        takes_agent(agent);
    }
}
