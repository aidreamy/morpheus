//! The voice daemon's protocol: methods, params, results, error codes.
//!
//! The surface is deliberately small: `hello`, `tts/synthesize`, `health` and
//! `shutdown`. Synthesis owns the resident model; playback and the session's
//! audio files stay with the client (the TUI), which needs to pause, resume
//! and restart the sound without holding an IPC connection open.

use serde::{Deserialize, Serialize};

/// The daemon's name — the socket file stem and the activation key.
pub const NAME: &str = "tts";

/// Protocol version. Monotonic; a major bump means an incompatible change.
pub const PROTOCOL: u32 = 1;

/// Method names on the wire.
pub mod method {
    pub const HELLO: &str = "hello";
    pub const SYNTHESIZE: &str = "tts/synthesize";
    pub const HEALTH: &str = "health";
    pub const SHUTDOWN: &str = "shutdown";
}

/// Application error codes (JSON-RPC reserved codes live in `ipc::RpcError`).
pub mod code {
    /// A method arrived before `hello`, or the protocol is incompatible.
    pub const HANDSHAKE: i32 = -32101;
    /// The voice model could not be loaded (missing bundle, bad speaker, …).
    /// Later requests retry after the daemon's own retry window.
    pub const MODEL: i32 = -32102;
    /// Another synthesis held the engine beyond the queue window.
    pub const BUSY: i32 = -32103;
    /// The model ran and failed, or the WAV could not be written.
    pub const SYNTHESIS: i32 = -32104;
    /// The daemon is draining for shutdown; the next call re-activates it.
    pub const DRAINING: i32 = -32105;
}

/// `hello` params: the first message on every connection.
#[derive(Debug, Serialize, Deserialize)]
pub struct HelloParams {
    /// The client's protocol version.
    pub protocol: u32,
    /// The connecting process — diagnostics only.
    pub pid: u32,
}

/// `hello` result.
#[derive(Debug, Serialize, Deserialize)]
pub struct HelloResult {
    /// The daemon's protocol version.
    pub protocol: u32,
    /// Always [`NAME`] — guards against a foreign daemon on our socket.
    pub daemon: String,
    /// The daemon binary's version.
    pub version: String,
}

/// `tts/synthesize` params.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesizeParams {
    /// The text to speak.
    pub text: String,
    /// Speaker override for this request; `None` uses the configured voice.
    #[serde(default)]
    pub voice: Option<i32>,
    /// Speed override for this request; `None` uses the configured speed.
    #[serde(default)]
    pub speed: Option<f32>,
    /// Absolute path where the daemon must write the WAV.
    pub output: String,
    /// How many equalizer bars the caller wants levels for.
    #[serde(default = "default_bars")]
    pub bars: u32,
    /// Animation frames per second for the returned bass levels.
    #[serde(default = "default_fps")]
    pub fps: u32,
}

const fn default_bars() -> u32 {
    8
}

const fn default_fps() -> u32 {
    30
}

/// `tts/synthesize` result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesizeResult {
    /// The WAV path that was written.
    pub wav: String,
    /// Utterance length in milliseconds.
    pub duration_ms: u64,
    /// Output sample rate in Hz.
    pub sample_rate: u32,
    /// Number of bands per frame in [`Self::bass`].
    pub bars: u32,
    /// Frames per second of [`Self::bass`].
    pub fps: u32,
    /// Frame-major bass levels, normalized to the utterance's own peak.
    pub bass: Vec<f32>,
}

/// `health` result — proves the resident model is shared.
#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResult {
    /// Seconds since daemon start.
    pub uptime_secs: u64,
    /// Whether the model is currently resident.
    pub loaded: bool,
    /// Syntheses served since start.
    pub requests_served: u64,
    /// Failed loads + failed syntheses since start.
    pub failed: u64,
}

/// `shutdown` result: the daemon accepted the drain.
#[derive(Debug, Serialize, Deserialize)]
pub struct ShutdownResult {
    /// Always true on the success path.
    pub draining: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthesize_defaults_fill_the_animation_knobs() {
        let params: SynthesizeParams = serde_json::from_value(serde_json::json!({
            "text": "hello",
            "output": "/tmp/a.wav"
        }))
        .expect("defaults");
        assert_eq!(params.bars, 8);
        assert_eq!(params.fps, 30);
        assert_eq!(params.voice, None);
        assert_eq!(params.speed, None);
    }

    #[test]
    fn synthesize_result_roundtrips() {
        let result = SynthesizeResult {
            wav: "/tmp/a.wav".into(),
            duration_ms: 1500,
            sample_rate: 24_000,
            bars: 8,
            fps: 30,
            bass: vec![0.0; 8 * 45],
        };
        let value = serde_json::to_value(&result).unwrap();
        let parsed: SynthesizeResult = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.duration_ms, 1500);
        assert_eq!(parsed.bass.len(), 8 * 45);
    }
}
