//! The live link to WhatsApp.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use whatsapp_rust::bot::{Bot, BotHandle};
use whatsapp_rust::pair_code::PairCodeOptions;
use whatsapp_rust::send::SendOptions;
use whatsapp_rust::store::SqliteStore;
use whatsapp_rust::store::traits::Backend;
use whatsapp_rust::transport::TokioWebSocketTransportFactory;
use whatsapp_rust::waproto::whatsapp as wa;
use whatsapp_rust::{Client, Jid, TokioRuntime};

use crate::config::{Config, Pairing};
use crate::error::{Error, Result};
use crate::event::{BUFFER, Event, EventSink, Events};
use crate::http::ReqwestHttpClient;
use crate::message::{ChatId, ChatKind, IncomingMessage, MessageContent};

/// A message that was handed to WhatsApp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentMessage {
    /// Server message id, usable for edits and reactions.
    pub id: String,
    /// The chat it was sent to.
    pub to: ChatId,
}

/// A linked WhatsApp account.
///
/// Cheap to clone (it is an `Arc` inside); every clone talks over the same
/// connection and shares the same lifecycle.
///
/// Call [`close`](WhatsApp::close) (or [`logout`](WhatsApp::logout)) to stop
/// the link: that is the graceful path, which flushes the device store and
/// lets the engine disconnect. Dropping the last clone is only a safety net -
/// it aborts the engine's run loop mid-flight, so nothing is flushed. A
/// long-running host should close explicitly.
#[derive(Clone, Debug)]
pub struct WhatsApp {
    inner: Arc<Inner>,
}

struct Inner {
    client: Arc<Client>,
    /// Kept alive so dropping it does not abort the run loop; `stop` aborts
    /// explicitly once the caller is done.
    run: Mutex<Option<BotHandle>>,
    /// Ids of the messages this process sent, so their echoes are recognized.
    sent: SentIds,
}

/// How many of this process's sends stay recognizable as its own.
///
/// A message id is the only way to tell this process's own output apart from
/// something the user typed on their phone if such a stanza were ever
/// delivered back here: both are `from_me`, and the text may come back
/// reformatted. The engine excludes the sending device from a direct
/// message's fan-out, so a normal send is not redelivered today - this set is
/// the guard that keeps the self chat from answering its own reply (forever)
/// should that invariant ever change, and it costs a few hundred ids.
const SENT_IDS: usize = 256;

/// Message ids this process sent, oldest first.
#[derive(Default)]
struct SentIds {
    ids: Mutex<VecDeque<String>>,
}

impl SentIds {
    /// Record an id **before** the message goes on the wire, so its echo can
    /// never arrive while the id is still unknown.
    fn remember(&self, id: &str) {
        let mut ids = self.lock();
        ids.push_back(id.to_string());
        while ids.len() > SENT_IDS {
            ids.pop_front();
        }
    }

    fn contains(&self, id: &str) -> bool {
        self.lock().iter().any(|sent| sent == id)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<String>> {
        self.ids
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The account's own address forms.
///
/// A WhatsApp account answers to two ids: the phone-number JID
/// (`…@s.whatsapp.net`) and the LID (`…@lid`) that newer conversations - the
/// "Message yourself" chat included - are addressed by. Both are matched, so
/// the self chat is recognized whichever form its chat id carries.
#[derive(Debug)]
struct Account {
    pn: Option<String>,
    lid: Option<String>,
}

impl Account {
    fn is_known(&self) -> bool {
        self.pn.is_some() || self.lid.is_some()
    }

    /// Whether `chat` is this account's own conversation.
    fn is_self(&self, chat: &ChatId) -> bool {
        chat.is_direct()
            && (self.pn.as_deref() == Some(chat.user()) || self.lid.as_deref() == Some(chat.user()))
    }
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WhatsApp")
            .field("connected", &self.client.is_connected())
            .finish()
    }
}

impl Inner {
    fn stop(&self) {
        if let Ok(mut run) = self.run.lock()
            && let Some(handle) = run.take()
        {
            handle.abort();
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.stop();
    }
}

impl WhatsApp {
    /// Open the device store and start the link.
    ///
    /// Returns the handle and the event stream to drive pairing and
    /// messages with. Pairing itself happens asynchronously: watch for
    /// [`Event::QrCode`] (or [`Event::PairingCode`]) and show the payload to
    /// the user, then wait for [`Event::Connected`].
    ///
    /// An existing session in the device store reconnects without pairing.
    ///
    /// The returned [`Events`] queue holds at most 1024 pending events and is
    /// never allowed to block the protocol engine. If the caller falls behind,
    /// message events are dropped with a logged warning; lifecycle events
    /// (pairing, connection, failures, logout) wait for room for up to 30
    /// seconds instead. Drain it promptly; a slow handler should move its work
    /// into a spawned task.
    pub async fn connect(config: Config) -> Result<(Self, Events)> {
        // Fail before touching the disk or the engine: an empty or truncated
        // number would otherwise surface as an opaque engine error.
        config.validate()?;

        prepare_store_dir(config.store_path()).await?;

        let store = SqliteStore::new(&config.store_path().to_string_lossy())
            .await
            .map_err(Error::storage)?;
        restrict_store_permissions(config.store_path());

        let (tx, events) = mpsc::channel(BUFFER);
        let sink = EventSink::new(tx);

        let mut builder = Bot::builder()
            .with_backend_arc(Arc::new(store) as Arc<dyn Backend>)
            .with_transport_factory(TokioWebSocketTransportFactory::new())
            .with_http_client(ReqwestHttpClient::new().map_err(Error::session)?)
            .with_runtime(TokioRuntime)
            .on_event(move |engine_event: std::sync::Arc<_>, _client| {
                let sink = sink.clone();
                async move {
                    for event in Event::from_engine(&engine_event) {
                        sink.send(event).await;
                    }
                }
            });

        if let Some(name) = config.push_name() {
            builder = builder.with_push_name(name);
        }
        if config.skips_history_sync() {
            builder = builder.skip_history_sync();
        }
        if let Pairing::Code { phone } = config.pairing() {
            builder = builder.with_pair_code(PairCodeOptions {
                phone_number: phone.clone(),
                ..Default::default()
            });
        }

        let bot = builder.build().await.map_err(Error::session)?;
        // The store may have written its WAL files while opening; harden
        // them again now that they exist.
        restrict_store_permissions(config.store_path());
        // `spawn` starts the run loop on the engine's task; the handle aborts
        // it if dropped, so it is kept next to the client it drives.
        let handle = bot.spawn();
        let client = handle.client();

        Ok((
            Self {
                inner: Arc::new(Inner {
                    client,
                    run: Mutex::new(Some(handle)),
                    sent: SentIds::default(),
                }),
            },
            events,
        ))
    }

    /// Send a text message to `chat`.
    ///
    /// Empty or whitespace-only text is rejected with [`Error::EmptyMessage`]
    /// instead of uploading a blank message.
    ///
    /// The message id is chosen here and recorded **before** the send, so an
    /// echo of it could never be mistaken for a prompt (see
    /// [`WhatsApp::is_prompt`]).
    ///
    /// Replies go to the chat exactly as it arrived. The self chat is
    /// displayed by the phone under its own id (LID on modern accounts), so
    /// rewriting it to the phone number would land the stanza in a chat the
    /// phone never shows.
    pub async fn send_text(&self, chat: &ChatId, text: impl Into<String>) -> Result<SentMessage> {
        let message = wa::Message {
            conversation: Some(text_body(text)?),
            ..Default::default()
        };
        let jid = chat.to_jid()?;

        let id = self.inner.client.generate_message_id();
        self.inner.sent.remember(&id);

        // The engine sends the id it is given and reports that same id back.
        let result = self
            .inner
            .client
            .send_message_with_options(jid, message, SendOptions::default().with_message_id(id))
            .await
            .map_err(Error::send)?;

        Ok(SentMessage {
            id: result.message_id,
            to: ChatId::from_jid(&result.to),
        })
    }

    /// Show or hide the "typing…" chat state in `chat`.
    ///
    /// This is presence, not a message: the server expires a composing state
    /// after a few seconds and nothing is persisted, so callers must treat it
    /// as a hint and never as durable state. `true` sends `composing`, `false`
    /// sends `paused`.
    ///
    /// Not every client renders it for every chat kind (a group or the self
    /// chat may hide it), and a failure here is never fatal to a reply - the
    /// message can still be sent.
    pub async fn set_typing(&self, chat: &ChatId, typing: bool) -> Result<()> {
        let jid = chat.to_jid()?;
        let chatstate = self.inner.client.chatstate();
        let result = if typing {
            chatstate.send_composing(&jid).await
        } else {
            chatstate.send_paused(&jid).await
        };
        result.map_err(Error::chat_state)
    }

    /// Whether `message` is a prompt the agent should answer.
    ///
    /// This is the one routing rule consumers share, and it differs from
    /// "did this account send it?" in exactly one place: the account's own
    /// chat ("Message yourself") is a prompt, so the agent can be driven from
    /// the same number it runs on. A message typed there on the phone arrives
    /// as `from_me` and is addressed to the account's own chat, which is what
    /// `Account::is_self` recognises.
    ///
    /// The agent's own replies are not delivered back here - the engine
    /// excludes the sending device from a direct message's fan-out - so the
    /// ids [`WhatsApp::send_text`] reserves are a guard for that invariant
    /// (see `SentIds`), not the mechanism the self chat relies on.
    ///
    /// Everything else `from_me` - the user's messages to contacts and groups
    /// - is left alone, as are status/broadcast posts, channels, protocol
    /// chatter and reactions.
    pub fn is_prompt(&self, message: &IncomingMessage) -> bool {
        is_prompt(
            message,
            account(&self.inner.client).as_ref(),
            &self.inner.sent,
        )
    }

    /// Wait until the link is connected, authenticated and done with its
    /// critical app-state sync (or until `timeout` elapses).
    pub async fn wait_until_connected(&self, timeout: Duration) -> Result<()> {
        self.inner
            .client
            .wait_for_connected(timeout)
            .await
            .map_err(Error::session)
    }

    /// Whether a network connection is currently up.
    pub fn is_connected(&self) -> bool {
        self.inner.client.is_connected()
    }

    /// Whether this device completed pairing and is authenticated.
    pub fn is_linked(&self) -> bool {
        self.inner.client.is_logged_in()
    }

    /// Unlink this companion from the phone and stop.
    ///
    /// The engine's logout is infallible by design (the deregistration is
    /// best-effort and the local teardown runs either way), so there is no
    /// failure to report. The Signal keys are wiped on the server side; the
    /// local device store keeps its bytes but is useless, so delete it to
    /// start over cleanly.
    pub async fn logout(&self) {
        self.inner.client.logout().await;
        self.inner.stop();
    }

    /// Disconnect gracefully and stop the link.
    ///
    /// Flushes the device state first, so the next connect resumes the
    /// session without re-pairing.
    pub async fn close(&self) {
        self.inner.client.disconnect().await;
        self.inner.stop();
    }
}

/// Create the device-store directory when it is missing.
///
/// Only a directory created by this call is hardened to `0700`: an existing
/// directory belongs to the caller (it may be a shared path such as `/tmp`),
/// and changing its mode would be a surprising, system-wide side effect.
async fn prepare_store_dir(store_path: &Path) -> Result<()> {
    let Some(parent) = store_path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }

    let existed = parent.exists();
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(Error::storage)?;
    if !existed {
        restrict_dir_permissions(parent);
    }
    Ok(())
}

/// [`WhatsApp::is_prompt`], with the account and the sent ids injected so the
/// rule is testable without a link.
fn is_prompt(message: &IncomingMessage, account: Option<&Account>, sent: &SentIds) -> bool {
    // An echo of a message this process sent, in whichever chat it lands.
    // Checked first and unconditionally: gating echo detection on a chat kind
    // (groups only, say) is exactly how a self chat ends up answering itself.
    if sent.contains(&message.id) {
        return false;
    }
    // Chats nobody can answer: status/broadcast and channels.
    if matches!(
        message.chat_kind,
        ChatKind::Broadcast | ChatKind::Newsletter
    ) {
        return false;
    }
    // Protocol chatter: edits, deletes, receipts, payloads we do not model.
    if matches!(
        message.content,
        MessageContent::System | MessageContent::Unsupported | MessageContent::Reaction { .. }
    ) {
        return false;
    }
    // Anything the account did not send is simply a conversation.
    if !message.from_me {
        return true;
    }
    // `from_me` but not this process's own send: another device of the account
    // wrote it (the phone, usually). Only the self chat is addressed to the
    // agent; what the user sends to contacts and groups is not ours to answer.
    let Some(account) = account else {
        // No identity to compare the chat against - the link is not paired, or
        // the device store has not learned one of its two ids. A silent drop
        // is what makes a dead self chat look like a bot ignoring you, so say
        // why.
        log::warn!(
            "morpheus-whatsapp: own message {} in {} dropped: the account identity is unknown",
            message.id,
            message.chat
        );
        return false;
    };
    account.is_self(&message.chat)
}

/// The user part of an own JID, when the device store reports one.
fn own_user(jid: Option<Jid>) -> Option<String> {
    let user = ChatId::from_jid(&jid?).user().to_string();
    (!user.is_empty()).then_some(user)
}

/// This account's own address forms, read on demand.
///
/// `Client::pn` and `Client::lid` read the engine's in-memory device snapshot
/// (an `Arc` clone), so there is nothing worth caching - and a form the engine
/// learns later, the LID usually, is picked up by the next message instead of
/// being frozen at first use. `None` until pairing gives the link an identity.
fn account(client: &Client) -> Option<Account> {
    let account = Account {
        pn: own_user(client.pn()),
        lid: own_user(client.lid()),
    };
    account.is_known().then_some(account)
}

/// The text of a message, rejecting blank input.
fn text_body(text: impl Into<String>) -> Result<String> {
    let text = text.into();
    if text.trim().is_empty() {
        return Err(Error::EmptyMessage);
    }
    Ok(text)
}

/// The device store holds Signal keys and the device identity, so keep it
/// readable only by its owner where the platform supports it.
///
/// SQLite runs the store in WAL mode, which puts recently written state in
/// `-wal` and `-shm` files next to the database; those are created after the
/// database file (with the process umask), so they need the same hardening.
#[cfg(unix)]
fn restrict_store_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    for candidate in [
        path.to_path_buf(),
        with_suffix(path, "-wal"),
        with_suffix(path, "-shm"),
    ] {
        match std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o600)) {
            Ok(()) => {}
            // The sidecars only exist once the store has written something.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => log::warn!(
                "morpheus-whatsapp: could not restrict permissions of {}: {err}",
                candidate.display()
            ),
        }
    }
}

#[cfg(not(unix))]
fn restrict_store_permissions(_path: &Path) {}

#[cfg(unix)]
fn with_suffix(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut os = path.as_os_str().to_owned();
    os.push(suffix);
    std::path::PathBuf::from(os)
}

#[cfg(unix)]
fn restrict_dir_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    if let Err(err) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)) {
        log::warn!(
            "morpheus-whatsapp: could not restrict permissions of {}: {err}",
            path.display()
        );
    }
}

#[cfg(not(unix))]
fn restrict_dir_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::message::Sender;

    const OWN_PN: &str = "5511912345678";
    const OWN_LID: &str = "99887766554433";
    const FRIEND: &str = "5511987654321";

    fn account() -> Account {
        Account {
            pn: Some(OWN_PN.to_string()),
            lid: Some(OWN_LID.to_string()),
        }
    }

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

    fn sent_from_me(chat: &str, id: &str) -> IncomingMessage {
        let mut message = message(chat, MessageContent::Text("written on the phone".into()));
        message.id = id.to_string();
        message.from_me = true;
        message
    }

    #[test]
    fn text_traffic_reaches_the_agent() {
        let own = account();
        assert!(is_prompt(
            &message(
                "5511987654321@s.whatsapp.net",
                MessageContent::Text("hi".to_string())
            ),
            Some(&own),
            &SentIds::default()
        ));
    }

    #[test]
    fn protocol_noise_is_ignored() {
        let own = account();
        for content in [
            MessageContent::System,
            MessageContent::Unsupported,
            MessageContent::Reaction {
                emoji: "👍".to_string(),
                target_message_id: Some("MSG0".to_string()),
            },
        ] {
            assert!(
                !is_prompt(
                    &message("5511987654321@s.whatsapp.net", content.clone()),
                    Some(&own),
                    &SentIds::default()
                ),
                "{content:?} should not reach the agent"
            );
        }
    }

    #[test]
    fn status_and_channels_are_ignored() {
        let own = account();
        for chat in ["status@broadcast", "120363012345678901@newsletter"] {
            assert!(!is_prompt(
                &message(chat, MessageContent::Text("post".to_string())),
                Some(&own),
                &SentIds::default()
            ));
        }
    }

    #[test]
    fn the_self_chat_is_a_prompt_whichever_id_addresses_it() {
        // This is the behaviour that used to require a second number: the
        // phone's own message to "Message yourself" arrives as `from_me`.
        let own = account();
        for chat in [
            format!("{OWN_PN}@s.whatsapp.net"),
            format!("{OWN_PN}:3@s.whatsapp.net"),
            format!("{OWN_LID}@lid"),
        ] {
            assert!(
                is_prompt(
                    &sent_from_me(&chat, "FROM-PHONE"),
                    Some(&own),
                    &SentIds::default()
                ),
                "the self chat at {chat} should reach the agent"
            );
        }
    }

    #[test]
    fn a_pn_only_identity_cannot_match_the_lid_chat() {
        // Pairing can store an empty LID, and the self chat is usually
        // addressed by LID. Until the engine learns it, that chat is
        // indistinguishable from a stranger's, so it is not answered - but
        // nothing is frozen: every message re-reads the identity, so the LID
        // is honoured as soon as the engine knows it.
        let pn_only = Account {
            pn: Some(OWN_PN.to_string()),
            lid: None,
        };
        assert!(
            !is_prompt(
                &sent_from_me(&format!("{OWN_LID}@lid"), "FROM-PHONE"),
                Some(&pn_only),
                &SentIds::default()
            ),
            "without the LID the self chat is unrecognizable"
        );
        assert!(
            is_prompt(
                &sent_from_me(&format!("{OWN_PN}@s.whatsapp.net"), "FROM-PHONE"),
                Some(&pn_only),
                &SentIds::default()
            ),
            "the phone-number form of the self chat is still recognized"
        );
    }

    #[test]
    fn our_own_reply_echo_never_reaches_the_agent() {
        // The reply was sent to the self chat; its echo carries the reserved
        // id, so answering it (forever) is impossible.
        let own = account();
        let sent = SentIds::default();
        sent.remember("REPLY-1");

        let echo = sent_from_me(&format!("{OWN_PN}@s.whatsapp.net"), "REPLY-1");
        assert!(!is_prompt(&echo, Some(&own), &sent));
    }

    #[test]
    fn own_sends_to_other_chats_are_not_prompts() {
        // The user writing to a friend from the phone echoes back here with
        // `from_me` and an id this process never sent; it is not our turn.
        let own = account();
        assert!(!is_prompt(
            &sent_from_me(&format!("{FRIEND}@s.whatsapp.net"), "FROM-PHONE"),
            Some(&own),
            &SentIds::default()
        ));
        assert!(!is_prompt(
            &sent_from_me("120363012345678901@g.us", "FROM-PHONE"),
            Some(&own),
            &SentIds::default()
        ));
    }

    #[test]
    fn an_unpaired_link_cannot_recognize_the_self_chat() {
        // Fail closed: without an identity there is no way to tell the self
        // chat from a conversation, so nothing `from_me` is answered.
        assert!(!is_prompt(
            &sent_from_me(&format!("{OWN_PN}@s.whatsapp.net"), "FROM-PHONE"),
            None,
            &SentIds::default()
        ));
    }

    #[test]
    fn sent_ids_stay_bounded_and_recognizable() {
        let sent = SentIds::default();
        for serial in 0..(SENT_IDS + 10) {
            sent.remember(&format!("ID-{serial}"));
        }
        assert!(sent.contains(&format!("ID-{}", SENT_IDS + 9)));
        assert!(!sent.contains("ID-0"), "oldest ids are evicted");
    }

    #[test]
    fn blank_text_is_rejected() {
        assert!(matches!(text_body(""), Err(Error::EmptyMessage)));
        assert!(matches!(text_body("  \n\t "), Err(Error::EmptyMessage)));
        assert_eq!(text_body(" hi ").expect("non-blank text"), " hi ");
    }

    fn temp_store_dir(name: &str) -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "morpheus-whatsapp-{name}-{}-{serial}",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn missing_store_directories_are_created_and_hardened() {
        let dir = temp_store_dir("created").join("nested");
        let db = dir.join("device.db");

        prepare_store_dir(&db).await.expect("create directory");
        assert!(dir.is_dir());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "a directory we created is owner-only");
        }

        let _ = std::fs::remove_dir_all(dir.parent().expect("parent"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn existing_store_directories_keep_their_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_store_dir("existing");
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        prepare_store_dir(&dir.join("device.db"))
            .await
            .expect("prepare");

        let mode = std::fs::metadata(&dir)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o755,
            "an existing directory (e.g. /tmp) must not be re-permissioned"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn sidecar_files_are_hardened_with_the_store() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "morpheus-whatsapp-perms-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let db = dir.join("device.db");
        let wal = with_suffix(&db, "-wal");
        let shm = with_suffix(&db, "-shm");

        // Missing sidecars (the common case before the first write) are fine.
        restrict_store_permissions(&db);

        for path in [&db, &wal, &shm] {
            std::fs::write(path, b"x").expect("write store file");
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        }
        restrict_store_permissions(&db);

        for path in [&db, &wal, &shm] {
            let mode = std::fs::metadata(path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{} is readable by others", path.display());
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
