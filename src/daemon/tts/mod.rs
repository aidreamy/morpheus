//! The voice daemon (`morpheus-ttsd`).
//!
//! One process owns the loaded TTS model (the Kokoro ONNX graph plus its
//! voices/lexicons) and serves synthesis to every morpheus instance, so two
//! open sessions no longer pay for two copies of the model. It writes each
//! utterance to the path the client asks for and returns the real bass
//! envelope of the generated audio for the TUI equalizer.
//!
//! - [`protocol`] — the wire schema (methods, params, results, error codes).
//! - [`server`] — accept loop, routing, the resident model and single
//!   synthesis slot, drain.
//! - [`client`] — the TUI-facing [`TtsClient`].
//! - `main` (the binary) — bind the socket, serve, idle-exit.
//!
//! Playback deliberately does NOT live here: the TUI plays the WAV through
//! the system player so it can pause/resume it, which an in-flight IPC
//! request could not do.

pub(crate) mod client;
pub mod protocol;
pub(crate) mod server;

use std::time::Duration;

pub use client::TtsClient;

/// The binary entrypoint (`morpheus-ttsd`): bind the daemon's socket and serve
/// until idle or drained.
///
/// Losing the bind race (a live daemon already owns the socket) is a SUCCESS
/// outcome, exactly like the decision daemon: the spawner that lost exits 0
/// and the winner serves.
pub fn run() -> i32 {
    let _ = crate::util::logger::try_init("ttsd");

    // A live daemon HOLDS this flock for its lifetime, so contending here
    // means one already owns the socket path; exit without touching it.
    let _activation_lock = match crate::daemon::activation::acquire_activation_lock(protocol::NAME)
    {
        crate::daemon::activation::ActivationLock::Contended => return 0,
        crate::daemon::activation::ActivationLock::Held(file) => file,
    };

    let path = crate::daemon::activation::socket_path(protocol::NAME);
    std::fs::create_dir_all(path.parent().unwrap_or(&path)).ok();
    let listener = match crate::daemon::ipc::bind(&path) {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => return 0,
        Err(e) => {
            eprintln!("morpheus-ttsd: cannot bind {}: {e}", path.display());
            return 1;
        }
    };

    // The voice settings are read once at startup: they describe how the
    // model loads, and the client sends per-request voice/speed overrides.
    // A broken setup.json must not take the daemon down — it starts with the
    // defaults and the first synthesis reports the model error.
    let voice = crate::setup::Setup::load().voice;
    let settings = server::Settings {
        voice: voice.voice,
        speed: voice.speed(),
        language: voice.language,
        model_dir: voice.model_dir,
    };

    let idle_timeout = std::env::var("MORPHEUS_TTSD_IDLE_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(server::DEFAULT_IDLE_TIMEOUT);

    log::info!("tts daemon: serving at {}", path.display());
    let server = std::sync::Arc::new(server::Server::new(settings));
    server.serve(listener, idle_timeout);

    let _ = std::fs::remove_file(&path);
    0
}
