//! The WhatsApp interface: `morpheus whatsapp`.
//!
//! Links this machine to a personal WhatsApp account as a companion device
//! and answers the linked conversations with the Morpheus agent. There is no
//! TUI: startup prints the pairing QR code, and after pairing the terminal
//! only reports lifecycle events while messages are handled in the
//! background.
//!
//! The account can talk to its own agent: a message to the "Message yourself"
//! chat arrives as `from_me`, addressed to the account's own chat, and that is
//! what `WhatsApp::is_prompt` recognises - the same rule the crate's own
//! `serve` loop uses. The engine does not deliver this device's own sends back
//! to it, and the ids every send reserves are kept as a guard, so a self chat
//! cannot loop. The reply keeps the chat id it arrived with (see the crate
//! docs).
//!
//! # Who may use the agent
//!
//! `setup.json` → `whatsapp` is an allowlist, checked after `is_prompt` and
//! before the message reaches a session (see [`access`]). The default is the
//! self-chat only: no contact and no group is answered until it is named in
//! `contacts` or `groups`. A denied message is logged and never answered, so
//! an unauthorized sender learns nothing about the agent.
//!
//! # Architecture
//!
//! ```text
//! WhatsApp events ──► run()                 (this file: QR, lifecycle, fan-out)
//!                       │
//!                       ▼
//!                   WhatsAppAgent           (agent.rs: chat → worker routing)
//!                       │
//!          ┌────────────┴────────────┐
//!          ▼                         ▼
//!   ChatControl                  ChatSession
//!   (control.rs: commands,        (session.rs: main turn, ACP turns,
//!    mode, roster, sessions,       @all fan-out)
//!    context, stop signals)            │
//!          │                           ▼
//!          ▼                    crate::assembly::assemble
//!   commands::parse             (the SAME harness assembly as the TUI)
//!   selection.rs / state.rs     executor.rs (ACP: permissions, resume)
//! ```
//!
//! - **`commands.rs`** owns the slash-command grammar and the allowlist that
//!   is published over WhatsApp. It is pure: parsing, argument validation
//!   and help text are unit-tested without a network or a harness.
//! - **`control.rs`** owns each chat's mode, external-agent roster, ACP
//!   session ids, main-agent context and stop signal, and answers every slash
//!   command immediately, outside the turn queue.
//! - **`selection.rs`** parses and validates `/agent` rosters and routes an
//!   inbound message (`@name`, `@name#k`, `@all`, main); **`state.rs`**
//!   persists roster, sessions and context per chat, atomically.
//! - **`executor.rs`** wraps one ACP instance: open/resume, mode and model
//!   mapping, permission policy and turn event draining.
//! - **`mode.rs`** owns the harness mode spellings (`build`, `ask`, `yolo`,
//!   `command`, `manager`), all case-insensitive.
//! - **`config.rs`** resolves `setup.json` into one immutable
//!   [`config::RuntimeConfig`] and turns it into a `HarnessAssembly`.
//! - **`session.rs`** runs one chat's turns. Main turns assemble a fresh
//!   harness and carry the model-facing context forward (persisted per chat);
//!   addressed turns drive ACP instances; `@all` runs them concurrently.
//! - **`agent.rs`** serializes prompts per chat and filters traffic the
//!   agent should not answer (echoes, broadcasts, channels, reactions).
//! - **`delivery.rs`** is the one outbound path: it drops empty replies,
//!   optionally shows the simulated typing state, sends, and logs failures.
//!   **`typing.rs`** owns the pure, bounded delay calculation behind it.
//! - **`access.rs`** owns the authorization policy (`setup.json` →
//!   `whatsapp`): which chats and which one-to-one contacts may reach the
//!   agent at all.
//!
//! # Commands
//!
//! `/help` lists the surface; `/mode <mode>` switches the harness mode;
//! `/agent` selects the external ACP agents for the chat (`/agent` and
//! `/agent ls` list, `/agent ls all` includes not-installed agents,
//! `/agent off` clears the roster, `/agent reset <name>` ends one session,
//! `/agent model <name>` reports one model and `/agent model <name> <model>`
//! picks it); `/human [on|off]` turns the simulated typing state on or off for
//! the chat (off by default); `/stop` cancels the turns running in this chat.
//!
//! A message with no prefix always goes to the main agent. An external agent
//! is reached only with `@name` (or `@name#k` for an instance); `@all` runs
//! every selected instance. External replies are labeled, so an unlabeled
//! reply is always the main agent's. An unknown `@name` is ordinary text for
//! the main agent, never a guess.
//! TUI-only commands (`theme`, `spinner`, …) are deliberately not
//! published. Detection is conservative: a message is a command only when it
//! starts with `/`, names a **registered** command, and that name is a bare
//! word - so `/usr/bin/env` or `/home/me/report.txt` stay normal messages.
//!
//! # Tools that need approval
//!
//! The TUI answers permission requests with a dialog. WhatsApp has no such
//! surface, so both the main harness and every ACP agent deny them and tell
//! the user in the reply. `/mode yolo` is the explicit opt-in to run tools
//! unattended; outside it, an ACP permission request is always rejected and
//! `permission_rx: None` is never used.

mod access;
mod agent;
mod commands;
mod config;
mod control;
mod delivery;
mod executor;
mod mode;
mod reply;
mod selection;
mod session;
mod state;
mod typing;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use morpheus_whatsapp::{Config, Event, WhatsApp, qr};

use agent::WhatsAppAgent;
use config::RuntimeConfig;

/// How many conversations may be answered at once. The same ceiling the
/// crate's `serve` uses: a slow model call must not block pairing or other
/// chats, but a burst must not open unbounded turns.
const MAX_IN_FLIGHT: usize = 4;

/// Run the WhatsApp interface until the device is unlinked or the link dies.
///
/// Returns the process exit code: `0` for the intended stop (the account was
/// logged out), `1` for a fatal link failure or a startup error.
///
/// A stored session can be dead while the device store still holds its
/// credentials: that happens when the companion was removed from the phone
/// between runs. The engine then reports [`Event::LoggedOut`] immediately and
/// never issues a QR code. Instead of stopping, this clears the stale store
/// and keeps the same process waiting for a fresh pairing code.
///
/// On Windows the engine keeps its SQLite handles open until the process
/// exits, so the delete can fail with a sharing violation when the dead store
/// is detected mid-run. That case leaves a marker file and exits; the next
/// start deletes the store before opening it, when nothing holds it.
pub async fn run() -> i32 {
    let Some(store_path) = Config::default_store_path() else {
        eprintln!(
            "morpheus whatsapp: cannot resolve the data directory for the device store; \
             is HOME set?"
        );
        return 1;
    };

    // A previous run found this session unlinked from the phone but could not
    // delete the store under its own open handles. Do it now, before anything
    // opens it again.
    if stale_marker(&store_path).exists() {
        if let Err(error) = clear_device_store(&store_path).await {
            eprintln!(
                "morpheus whatsapp: could not clear the unlinked device store at {}: {error}",
                store_path.display()
            );
            eprintln!("Is another `morpheus whatsapp` still running? Stop it and try again.");
            return 1;
        }
        let _ = tokio::fs::remove_file(stale_marker(&store_path)).await;
    }

    let cwd = std::env::current_dir()
        .ok()
        .and_then(|path| path.to_str().map(String::from))
        .unwrap_or_else(|| ".".to_string());

    println!("Morpheus - WhatsApp interface");
    println!("Device store: {}", store_path.display());
    println!("Working directory: {cwd}");
    println!("Waiting for a pairing code…");
    println!();

    let runtime = RuntimeConfig::load(&cwd);
    let access = runtime.access.clone();
    let agent = Arc::new(WhatsAppAgent::new(runtime));
    // At most one automatic recovery: if a fresh store is reported dead too,
    // something else is wrong and looping would spin forever.
    let mut repaired = false;

    loop {
        let config = Config::new(store_path.clone()).with_push_name("Morpheus");
        let (whatsapp, mut events) = match WhatsApp::connect(config).await {
            Ok(link) => link,
            Err(error) => {
                eprintln!("morpheus whatsapp: could not start the link: {error}");
                return 1;
            }
        };

        let slots = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
        let mut tasks = JoinSet::new();
        let mut connected = false;

        let outcome = loop {
            reap(&mut tasks);

            let Some(event) = events.recv().await else {
                break Err("the WhatsApp link closed unexpectedly".to_string());
            };

            match event {
                Event::QrCode { payload, .. } => print_qr(&payload),
                Event::PairingCode { code, .. } => {
                    println!(
                        "On the phone: WhatsApp → Linked devices → Link with phone number instead, \
                     then type {code}"
                    );
                    flush_stdout();
                }
                Event::PairingFailed { reason } => {
                    eprintln!("morpheus whatsapp: pairing failed: {reason}");
                }
                Event::Connected => {
                    connected = true;
                    println!(
                        "Connected. Message your own chat (`/help` lists the commands). Contacts \
                         and groups are answered only when allowed in setup.json → whatsapp."
                    );
                    flush_stdout();
                }
                Event::Disconnected => {
                    eprintln!("morpheus whatsapp: connection dropped; reconnecting…");
                }
                Event::LoggedOut => {
                    // The device store still holds the dead credentials; clearing
                    // them is what lets the next pairing code be issued. If this
                    // happened before the link was ever usable, the session was
                    // already stale when the process started, so recover in
                    // place instead of making the user run the command again.
                    if connected || repaired {
                        println!("Device unlinked from the phone.");
                        break Ok(Stop::Exit);
                    }
                    println!("The stored session is no longer valid; restarting the pairing…");
                    flush_stdout();
                    break Ok(Stop::Repair);
                }
                Event::Failure { kind, reason } => {
                    if kind.is_fatal() {
                        break Err(format!("unrecoverable link failure: {reason}"));
                    }
                    eprintln!("morpheus whatsapp: {reason}; reconnecting…");
                }
                Event::Message(message) => {
                    // The crate owns the routing rule: normal chats and this
                    // account's own chat are prompts; status posts, channels,
                    // protocol chatter and our own sends are not. In a self
                    // chat that is what prevents a loop.
                    //
                    // The message body is deliberately not logged: this file
                    // is plain text and every `info` record lands in it, so
                    // its wording is recorded by id and content kind only.
                    let is_prompt = whatsapp.is_prompt(&message);
                    log::info!(
                        "morpheus whatsapp: inbound id={} chat={} from_me={} chat_kind={:?} \
                         content={} prompt={}",
                        message.id,
                        message.chat,
                        message.from_me,
                        message.chat_kind,
                        message.content.kind(),
                        is_prompt
                    );
                    if !is_prompt {
                        continue;
                    }
                    // Authorization is separate from routing: a prompt from a
                    // chat the owner did not allow is dropped before a slot or
                    // a session is involved, and without a reply (see
                    // `access.rs`).
                    if !access.allows(&message) {
                        log::warn!(
                            "morpheus whatsapp: message {} from {} in {} is not allowed by \
                             setup.json → whatsapp; ignored",
                            message.id,
                            message.sender.id,
                            message.chat
                        );
                        continue;
                    }
                    let Some(permit) = Arc::clone(&slots).try_acquire_owned().ok() else {
                        log::warn!(
                            "morpheus whatsapp: all {MAX_IN_FLIGHT} conversation slots are busy; \
                         replying busy for message {}",
                            message.id
                        );
                        let link = whatsapp.clone();
                        let chat = message.chat.clone();
                        tokio::spawn(async move {
                            let _ = link
                            .send_text(
                                &chat,
                                "I'm busy with other requests right now. Please resend your message in a moment.",
                            )
                            .await;
                        });
                        continue;
                    };

                    let agent = Arc::clone(&agent);
                    let link = whatsapp.clone();
                    tasks.spawn(async move {
                        let _permit = permit;
                        let batch = agent.handle(&message).await;
                        delivery::send(&link, &message.chat, batch).await;
                    });
                }
                // `Event` is `#[non_exhaustive]`: a future variant this build
                // does not know yet is ignored instead of failing to compile.
                _ => {}
            }
        };

        // Do not let in-flight turns outlive the link they would reply on.
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        whatsapp.close().await;
        // Windows keeps the SQLite handles open until the last clone is
        // dropped, so the store cannot be deleted while `whatsapp` is alive.
        drop(whatsapp);

        match outcome {
            Ok(Stop::Exit) => {
                // Best effort: a dead session must not be resumed by the next
                // run, but failing to delete it is not worth a nonzero exit.
                if let Err(error) = clear_device_store(&store_path).await {
                    log::warn!(
                        "morpheus whatsapp: could not clear the dead device store at {}: {error}; \
                         it will be removed on the next start",
                        store_path.display()
                    );
                    mark_stale_store(&store_path).await;
                }
                return 0;
            }
            Ok(Stop::Repair) => {
                repaired = true;
                if let Err(error) = clear_device_store(&store_path).await {
                    // The engine still holds the store, so this process cannot
                    // pair again; the marker makes the next start do the
                    // delete before opening it.
                    eprintln!(
                        "morpheus whatsapp: the unlinked device store is still in use ({error})."
                    );
                    mark_stale_store(&store_path).await;
                    eprintln!(
                        "Run `morpheus whatsapp` again to pair: the dead session will be removed."
                    );
                    return 1;
                }
                println!();
                continue;
            }
            Err(error) => {
                eprintln!("morpheus whatsapp: {error}");
                return 1;
            }
        }
    }
}

/// How a link run ended: a normal stop, or a stale session that should be
/// cleared and paired again.
enum Stop {
    Exit,
    Repair,
}

/// Delete a device store whose session the phone has invalidated.
///
/// SQLite runs in WAL mode, so the `-wal` and `-shm` sidecars belong to the
/// session too; leaving them behind can keep the dead state alive. Missing
/// files are fine - the store may never have written them.
async fn clear_device_store(path: &Path) -> std::io::Result<()> {
    for candidate in [
        path.to_path_buf(),
        with_suffix(path, "-wal"),
        with_suffix(path, "-shm"),
    ] {
        match tokio::fs::remove_file(&candidate).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            // On Windows a just-closed handle can take a moment to be
            // released; retry briefly before giving up.
            Err(error) => {
                let mut last = error;
                let mut removed = false;
                for _ in 0..5 {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    match tokio::fs::remove_file(&candidate).await {
                        Ok(()) => {
                            removed = true;
                            break;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            removed = true;
                            break;
                        }
                        Err(error) => last = error,
                    }
                }
                if !removed {
                    return Err(last);
                }
            }
        }
    }
    Ok(())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_owned();
    os.push(suffix);
    PathBuf::from(os)
}

/// Marker telling the next start that `path` holds an unlinked session.
fn stale_marker(path: &Path) -> PathBuf {
    with_suffix(path, ".stale")
}

/// Record that the device store must be deleted before its next use. Failure
/// is only logged: the worst case is the old behaviour of a dead store that
/// the next run detects again.
async fn mark_stale_store(path: &Path) {
    let marker = stale_marker(path);
    if let Some(parent) = marker.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Err(error) = tokio::fs::write(&marker, b"pair again\n").await {
        log::warn!(
            "morpheus whatsapp: could not mark the unlinked device store at {}: {error}",
            marker.display()
        );
    }
}

/// Print a fresh pairing QR code, with the instructions that make it
/// scannable. A new payload arrives periodically until the phone scans one.
fn print_qr(payload: &str) {
    match qr::render(payload) {
        Ok(code) => {
            println!("Scan from WhatsApp → Linked devices → Link a device:");
            println!("{code}");
            flush_stdout();
        }
        Err(error) => {
            // Rendering only fails on an oversized payload, which the
            // server never issues; keep the link alive and report it.
            eprintln!("morpheus whatsapp: could not render the pairing QR code: {error}");
        }
    }
}

fn flush_stdout() {
    let _ = std::io::stdout().flush();
}

/// Reap finished conversation tasks, logging panics instead of losing them.
fn reap(tasks: &mut JoinSet<()>) {
    while let Some(finished) = tasks.try_join_next() {
        if let Err(error) = finished
            && error.is_panic()
        {
            log::error!("morpheus whatsapp: a conversation task panicked: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "morpheus-whatsapp-store-{name}-{}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[tokio::test]
    async fn a_dead_store_is_removed_with_its_sidecars() {
        let dir = temp_dir("clear");
        let db = dir.join("device.db");
        for path in [
            db.clone(),
            with_suffix(&db, "-wal"),
            with_suffix(&db, "-shm"),
        ] {
            std::fs::write(&path, b"dead session").expect("write store file");
        }

        clear_device_store(&db).await.expect("clear store");

        assert!(!db.exists());
        assert!(!with_suffix(&db, "-wal").exists());
        assert!(!with_suffix(&db, "-shm").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn clearing_an_absent_store_is_not_an_error() {
        let dir = temp_dir("absent");
        let db = dir.join("device.db");
        clear_device_store(&db).await.expect("nothing to remove");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn marking_a_stale_store_creates_the_marker_directory() {
        let dir = temp_dir("mark");
        let db = dir.join("nested").join("device.db");

        mark_stale_store(&db).await;

        assert!(stale_marker(&db).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
