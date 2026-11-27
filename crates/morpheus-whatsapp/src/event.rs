//! The event stream a caller drives the link with.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;

use crate::message::IncomingMessage;

/// How many events may queue up before newer ones are dropped.
pub(crate) const BUFFER: usize = 1024;

/// A stream of link events, handed out by
/// [`WhatsApp::connect`](crate::WhatsApp::connect).
///
/// The channel closes when the link stops, so a `while let Some(event) =
/// events.recv().await` loop is the natural consumer.
///
/// The queue is bounded and never blocks the protocol engine. Event delivery
/// has two paths:
///
/// - **Message events** are lossy: if the caller falls more than 1024 events
///   behind, further messages are dropped and a warning is logged. A stalled
///   consumer therefore costs messages, not a wedged connection.
/// - **Lifecycle events** (pairing, connection, failures, logout) are never
///   dropped silently: delivery waits for room in the queue for up to 30
///   seconds, then gives up with a logged warning. A caller that stopped
///   reading cannot miss the event that says the link died - but a consumer
///   that legitimately stops reading should drop the receiver instead of
///   letting the wait expire.
///
/// Drain the stream promptly, or use [`serve`], which hands messages to the
/// agent in spawned tasks.
///
/// [`serve`]: crate::serve
pub type Events = mpsc::Receiver<Event>;

/// A failure the engine reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailureKind {
    /// Another WhatsApp Web session took over this companion's stream. The
    /// link cannot continue until the other session stops.
    StreamReplaced,
    /// The account was rate-limited or temporarily banned.
    TemporaryBan,
    /// A connect attempt was rejected by the server. The engine retries with
    /// backoff.
    ConnectFailed,
    /// The server reported a protocol-level stream error. The engine retries
    /// with backoff.
    StreamError,
    /// The client version is no longer accepted. Upgrading the engine is the
    /// only fix.
    ClientOutdated,
    /// The QR code was scanned by a phone that does not support linked
    /// devices, so pairing can never complete.
    MultiDeviceUnsupported,
    /// A failure this crate does not classify yet.
    Other,
}

impl FailureKind {
    /// Whether the caller should stop and report the failure instead of
    /// letting the engine reconnect (as opposed to a drop the engine retries
    /// with backoff).
    ///
    /// [`TemporaryBan`](Self::TemporaryBan) is fatal in this sense: the link
    /// may work again after `expire`, but a bot has nothing to do until then,
    /// and the engine's reconnects would keep hitting the ban.
    pub fn is_fatal(self) -> bool {
        matches!(
            self,
            Self::StreamReplaced
                | Self::TemporaryBan
                | Self::ClientOutdated
                | Self::MultiDeviceUnsupported
        )
    }
}

/// Something that happened on the link.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Event {
    /// A pairing QR code was issued. Render `payload` with
    /// [`qr::render`](crate::qr::render) and show it to the user; a fresh
    /// code follows every `expires_in` until the phone scans one.
    QrCode {
        payload: String,
        expires_in: Duration,
    },
    /// A pairing code was issued (code-based linking). Type it on the phone
    /// under *Linked devices → Link with phone number instead*.
    PairingCode { code: String, expires_in: Duration },
    /// The phone rejected the link or the pairing request failed.
    PairingFailed { reason: String },
    /// Connected and authenticated; messages start flowing.
    Connected,
    /// The connection dropped. The engine reconnects with backoff on its
    /// own unless the link was closed or logged out.
    Disconnected,
    /// The device was unlinked from the phone; the stored session is dead
    /// and the next connect starts a fresh pairing.
    LoggedOut,
    /// The engine reported a failure. See [`FailureKind::is_fatal`] to tell
    /// recoverable drops from outcomes that need the caller to stop.
    Failure { kind: FailureKind, reason: String },
    /// An inbound, already-decrypted message.
    Message(IncomingMessage),
}

impl Event {
    /// Whether this reports a change the caller must not miss (as opposed to
    /// message traffic, which the queue may drop under pressure).
    pub(crate) fn is_lifecycle(&self) -> bool {
        !matches!(self, Self::Message(_))
    }

    /// Map an engine event onto this crate's vocabulary.
    ///
    /// Returns a list because the engine batches inbound messages: live
    /// traffic arrives as a batch of one, an offline drain as one batch per
    /// durable commit. Every other event maps to zero or one entry.
    pub(crate) fn from_engine(event: &whatsapp_rust::types::events::Event) -> Vec<Self> {
        use whatsapp_rust::types::events::Event as Engine;

        let one = |event: Self| vec![event];
        match event {
            Engine::PairingQrCode(qr) => one(Self::QrCode {
                payload: qr.code.clone(),
                expires_in: qr.timeout,
            }),
            Engine::PairingCode(code) => one(Self::PairingCode {
                code: code.code.clone(),
                expires_in: code.timeout,
            }),
            Engine::PairError(error) => one(Self::PairingFailed {
                reason: error.error.clone(),
            }),
            Engine::Connected(_) => one(Self::Connected),
            Engine::Disconnected(_) => one(Self::Disconnected),
            Engine::LoggedOut(_) => one(Self::LoggedOut),
            Engine::Messages(batch) => batch
                .messages
                .iter()
                .map(|message| {
                    Self::Message(IncomingMessage::from_engine(
                        &message.message,
                        &message.info,
                    ))
                })
                .collect(),
            Engine::StreamReplaced(_) => one(Self::Failure {
                kind: FailureKind::StreamReplaced,
                reason: "another WhatsApp Web session took over this companion".to_string(),
            }),
            Engine::TemporaryBan(ban) => one(Self::Failure {
                kind: FailureKind::TemporaryBan,
                reason: format!(
                    "temporarily banned: {:?} (expires in {:?})",
                    ban.code, ban.expire
                ),
            }),
            Engine::ConnectFailure(failure) => one(Self::Failure {
                kind: FailureKind::ConnectFailed,
                reason: match failure.message.as_deref() {
                    Some(details) if !details.is_empty() => {
                        format!("{:?}: {details}", failure.reason)
                    }
                    // The engine leaves the detail out for most reasons; a
                    // trailing separator would read as a truncated message.
                    _ => format!("{:?}", failure.reason),
                },
            }),
            Engine::StreamError(error) => one(Self::Failure {
                kind: FailureKind::StreamError,
                reason: format!("stream error {}", error.code),
            }),
            Engine::ClientOutdated(_) => one(Self::Failure {
                kind: FailureKind::ClientOutdated,
                reason: "this client version is no longer accepted by WhatsApp".to_string(),
            }),
            Engine::QrScannedWithoutMultidevice(_) => one(Self::Failure {
                kind: FailureKind::MultiDeviceUnsupported,
                reason: "the phone scanned the code but does not support linked devices"
                    .to_string(),
            }),
            other => {
                // Receipts, presence, history sync, app-state updates and raw
                // nodes are outside this crate's scope; trace keeps them
                // observable without turning the log into a firehose.
                log::trace!("morpheus-whatsapp: ignored engine event {other:?}");
                Vec::new()
            }
        }
    }
}

/// How long a lifecycle event waits for room in the queue before giving up.
/// Long enough for a slow UI to drain one event, short enough that a caller
/// who stopped reading cannot pin engine tasks for the process lifetime.
const LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Delivers events to the caller without ever blocking the protocol engine.
///
/// The engine spawns one task per event, so awaiting inside `send` does not
/// stall the protocol loop. Message traffic therefore stays lossy
/// (`try_send`) to avoid piling up tasks under a flood, while lifecycle
/// events wait for room with a timeout, so the caller cannot miss a
/// disconnected/logged-out link.
#[derive(Clone)]
pub(crate) struct EventSink {
    tx: mpsc::Sender<Event>,
    dropped: Arc<AtomicU64>,
    lifecycle_timeout: Duration,
}

impl EventSink {
    pub(crate) fn new(tx: mpsc::Sender<Event>) -> Self {
        Self {
            tx,
            dropped: Arc::new(AtomicU64::new(0)),
            lifecycle_timeout: LIFECYCLE_TIMEOUT,
        }
    }

    /// Deliver `event`, awaiting room for lifecycle events.
    pub(crate) async fn send(&self, event: Event) {
        if !event.is_lifecycle() {
            return self.send_lossy(event);
        }

        match tokio::time::timeout(self.lifecycle_timeout, self.tx.send(event)).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => log::debug!("morpheus-whatsapp: event receiver dropped"),
            Err(_) => log::warn!(
                "morpheus-whatsapp: event queue stayed full for {:?}, dropped a lifecycle event",
                self.lifecycle_timeout
            ),
        }
    }

    fn send_lossy(&self, event: Event) {
        match self.tx.try_send(event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                // Log the first drop and then once per 100, so sustained
                // overload does not flood the log it is trying to warn about.
                if dropped == 1 || dropped.is_multiple_of(100) {
                    log::warn!(
                        "morpheus-whatsapp: event queue is full, dropped {dropped} event(s); \
                         drain the `Events` stream faster to avoid missing messages"
                    );
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                log::debug!("morpheus-whatsapp: event receiver dropped");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::sync::Arc;

    use whatsapp_rust::types::events::{
        BatchOrigin, ClientOutdated, ConnectFailure, ConnectFailureReason, Connected, Disconnected,
        InboundMessage, MessageBatch, StreamError, StreamReplaced, TempBanReason, TemporaryBan,
    };
    use whatsapp_rust::types::message::MessageInfo;
    use whatsapp_rust::waproto::whatsapp as wa;

    use super::*;
    use crate::message::{ChatId, ChatKind, MessageContent, Sender};

    /// The single event most engine events map to.
    fn mapped(event: &whatsapp_rust::types::events::Event) -> Option<Event> {
        let mut events = Event::from_engine(event);
        assert!(events.len() <= 1, "expected at most one event: {events:?}");
        events.pop()
    }

    fn engine_message(message: wa::Message, info: MessageInfo) -> InboundMessage {
        InboundMessage::builder()
            .message(Arc::new(message))
            .info(Arc::new(info))
            .build()
    }

    fn batch(messages: Vec<InboundMessage>) -> whatsapp_rust::types::events::Event {
        whatsapp_rust::types::events::Event::Messages(
            MessageBatch::builder()
                .messages(Arc::from(messages))
                .origin(BatchOrigin::Live)
                .build(),
        )
    }

    #[test]
    fn lifecycle_events_keep_their_payload() {
        let qr = mapped(&whatsapp_rust::types::events::Event::PairingQrCode(
            whatsapp_rust::types::events::PairingQrCode::builder()
                .code("2@abc,def".to_string())
                .timeout(Duration::from_secs(60))
                .build(),
        ))
        .expect("qr is mapped");
        assert!(matches!(qr, Event::QrCode { ref payload, .. } if payload == "2@abc,def"));

        let code = mapped(&whatsapp_rust::types::events::Event::PairingCode(
            whatsapp_rust::types::events::PairingCode::builder()
                .code("ABCD-1234".to_string())
                .timeout(Duration::from_secs(180))
                .build(),
        ))
        .expect("pairing code is mapped");
        assert!(
            matches!(code, Event::PairingCode { ref code, .. } if code == "ABCD-1234"),
            "{code:?}"
        );

        assert!(matches!(
            mapped(&whatsapp_rust::types::events::Event::Connected(
                Connected::builder().build()
            )),
            Some(Event::Connected)
        ));
        assert!(matches!(
            mapped(&whatsapp_rust::types::events::Event::Disconnected(
                Disconnected::builder()
                    .reason(whatsapp_rust::wacore::net::DisconnectReason::StreamEnded)
                    .build()
            )),
            Some(Event::Disconnected)
        ));
    }

    #[test]
    fn message_events_are_normalized() {
        let mut info = MessageInfo::default();
        info.source.chat = whatsapp_rust::Jid::from_str("5511912345678@s.whatsapp.net").unwrap();
        info.source.sender = info.source.chat.clone();
        info.id = "MSG1".into();
        let raw = wa::Message {
            conversation: Some("hi".to_string()),
            ..Default::default()
        };

        let event = mapped(&batch(vec![engine_message(raw, info)])).expect("message is mapped");
        match event {
            Event::Message(message) => assert_eq!(message.text(), Some("hi")),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn message_batches_are_fanned_out_in_order() {
        let mut second = MessageInfo::default();
        second.source.chat = whatsapp_rust::Jid::from_str("5511912345678@s.whatsapp.net").unwrap();
        second.source.sender = second.source.chat.clone();
        second.id = "MSG2".into();
        let mut first = second.clone();
        first.id = "MSG1".into();

        let events = Event::from_engine(&batch(vec![
            engine_message(
                wa::Message {
                    conversation: Some("one".to_string()),
                    ..Default::default()
                },
                first,
            ),
            engine_message(
                wa::Message {
                    conversation: Some("two".to_string()),
                    ..Default::default()
                },
                second,
            ),
        ]));

        let texts: Vec<_> = events
            .iter()
            .map(|event| match event {
                Event::Message(message) => message.text().expect("text"),
                other => panic!("unexpected event: {other:?}"),
            })
            .collect();
        assert_eq!(texts, ["one", "two"]);
    }

    #[test]
    fn fatal_failures_are_surfaced() {
        let replaced = mapped(&whatsapp_rust::types::events::Event::StreamReplaced(
            StreamReplaced::builder().build(),
        ))
        .expect("stream replaced is mapped");
        match replaced {
            Event::Failure { kind, .. } => {
                assert_eq!(kind, FailureKind::StreamReplaced);
                assert!(kind.is_fatal());
            }
            other => panic!("unexpected event: {other:?}"),
        }

        let outdated = mapped(&whatsapp_rust::types::events::Event::ClientOutdated(
            ClientOutdated::builder().build(),
        ))
        .expect("client outdated is mapped");
        assert!(
            matches!(outdated, Event::Failure { kind, .. } if kind.is_fatal()),
            "{outdated:?}"
        );

        let ban = mapped(&whatsapp_rust::types::events::Event::TemporaryBan(
            TemporaryBan::builder()
                .code(TempBanReason::SentToTooManyPeople)
                .expire(chrono::TimeDelta::seconds(3600))
                .build(),
        ))
        .expect("temporary ban is mapped");
        match ban {
            Event::Failure { kind, reason } => {
                assert_eq!(kind, FailureKind::TemporaryBan);
                assert!(reason.contains("SentToTooManyPeople"), "{reason}");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn recoverable_failures_are_not_fatal() {
        let failure = mapped(&whatsapp_rust::types::events::Event::ConnectFailure(
            ConnectFailure::builder()
                .reason(ConnectFailureReason::Generic)
                .message("boom".to_string())
                .build(),
        ))
        .expect("connect failure is mapped");
        match failure {
            Event::Failure { kind, reason } => {
                assert_eq!(kind, FailureKind::ConnectFailed);
                assert!(!kind.is_fatal());
                assert!(reason.contains("boom"), "{reason}");
            }
            other => panic!("unexpected event: {other:?}"),
        }

        // A failure without details names the reason only, instead of
        // reporting "Generic: " as if the message had been cut off.
        let bare = mapped(&whatsapp_rust::types::events::Event::ConnectFailure(
            ConnectFailure::builder()
                .reason(ConnectFailureReason::Generic)
                .build(),
        ))
        .expect("connect failure is mapped");
        match bare {
            Event::Failure { reason, .. } => assert_eq!(reason, "Generic"),
            other => panic!("unexpected event: {other:?}"),
        }

        let stream = mapped(&whatsapp_rust::types::events::Event::StreamError(
            StreamError::builder().code("515".to_string()).build(),
        ))
        .expect("stream error is mapped");
        assert!(matches!(stream, Event::Failure { kind, .. } if !kind.is_fatal()));
    }

    #[test]
    fn out_of_scope_events_are_ignored() {
        let event = whatsapp_rust::types::events::Event::OfflineSyncCompleted(
            whatsapp_rust::types::events::OfflineSyncCompleted::builder()
                .count(3)
                .build(),
        );
        assert!(Event::from_engine(&event).is_empty());
    }

    #[test]
    fn scanned_without_multidevice_is_fatal() {
        let event = mapped(
            &whatsapp_rust::types::events::Event::QrScannedWithoutMultidevice(
                whatsapp_rust::types::events::QrScannedWithoutMultidevice::builder().build(),
            ),
        )
        .expect("mapped");
        assert!(
            matches!(event, Event::Failure { kind, .. } if kind == FailureKind::MultiDeviceUnsupported && kind.is_fatal()),
            "{event:?}"
        );
    }

    fn inbound() -> Event {
        let chat = ChatId::parse("5511912345678@s.whatsapp.net").expect("chat id");
        Event::Message(IncomingMessage {
            id: "MSG1".to_string(),
            chat: chat.clone(),
            chat_kind: ChatKind::Direct,
            sender: Sender {
                id: chat,
                name: None,
            },
            sender_alt: None,
            from_me: false,
            timestamp: chrono::Utc::now(),
            content: MessageContent::Text("hi".to_string()),
        })
    }

    #[tokio::test]
    async fn message_overflow_drops_instead_of_blocking() {
        let (tx, mut rx) = mpsc::channel(1);
        let sink = EventSink::new(tx);

        sink.send(inbound()).await;
        sink.send(inbound()).await;

        assert!(matches!(rx.recv().await, Some(Event::Message(_))));
        // The second message was dropped, not queued: message traffic is
        // lossy under pressure.
        assert!(rx.try_recv().is_err());
        drop(rx);
        // A closed receiver is tolerated as well.
        sink.send(inbound()).await;
    }

    #[tokio::test]
    async fn lifecycle_events_wait_for_room() {
        let (tx, mut rx) = mpsc::channel(1);
        let sink = EventSink::new(tx);

        sink.send(inbound()).await;
        let sender = sink.clone();
        let waiting = tokio::spawn(async move { sender.send(Event::LoggedOut).await });

        assert!(matches!(rx.recv().await, Some(Event::Message(_))));
        // `LoggedOut` was not dropped while the queue was full; it was
        // delivered as soon as the consumer made room.
        assert!(matches!(rx.recv().await, Some(Event::LoggedOut)));
        waiting.await.expect("sender task");
    }

    #[tokio::test]
    async fn stalled_consumer_cannot_pin_lifecycle_events_forever() {
        let (tx, mut rx) = mpsc::channel(1);
        let sink = EventSink {
            tx,
            dropped: Arc::new(AtomicU64::new(0)),
            lifecycle_timeout: Duration::from_millis(20),
        };

        sink.send(inbound()).await;
        let started = std::time::Instant::now();
        sink.send(Event::LoggedOut).await;

        assert!(started.elapsed() >= Duration::from_millis(20));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(matches!(rx.try_recv(), Ok(Event::Message(_))));
        assert!(rx.try_recv().is_err(), "the lifecycle event was dropped");
    }
}
