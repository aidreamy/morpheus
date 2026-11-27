//! Client side of the voice daemon: one round trip per synthesis.
//!
//! The daemon never plays audio - it writes the WAV and returns where it is
//! plus the bass envelope for the animation. That keeps the connection short
//! (the TUI needs to pause and restart the sound, which a blocking IPC call
//! could not express) and leaves file cleanup to the session store.

use std::path::Path;
use std::time::Duration;

use crate::daemon::client::DaemonClient;
use crate::daemon::ipc;
use crate::daemon::ipc_error::Ipc;

use super::protocol::{self, ShutdownResult, SynthesizeParams, SynthesizeResult, code, method};

/// Draining is a flag flip plus a response; a short budget is enough. The
/// server joins in-flight connections before exiting.
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);

/// The first synthesis may load the model (multi-second) plus run it.
const SYNTH_BUDGET: Duration = Duration::from_secs(180);

/// How long to sleep before the single BUSY retry.
const BUSY_RETRY_BACKOFF: Duration = Duration::from_millis(250);

/// Synthesis client for the resident voice daemon.
pub struct TtsClient {
    client: DaemonClient,
}

impl Default for TtsClient {
    fn default() -> Self {
        Self::new()
    }
}

impl TtsClient {
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: DaemonClient::new(protocol::NAME, protocol::PROTOCOL),
        }
    }

    /// Synthesize `text` into `output` and return its metadata.
    ///
    /// A BUSY (another synthesis held the engine past the queue window) is
    /// retried once, like the decision daemon's busy policy; every other
    /// failure is returned to the caller, which decides whether to surface it.
    pub fn synthesize(
        &self,
        text: &str,
        output: &Path,
        voice: Option<i32>,
        speed: Option<f32>,
        bars: u32,
        fps: u32,
    ) -> Result<SynthesizeResult, String> {
        let params = SynthesizeParams {
            text: text.to_owned(),
            voice,
            speed,
            output: output.to_string_lossy().into_owned(),
            bars,
            fps,
        };
        match self.call(method::SYNTHESIZE, &params) {
            Err(error)
                if DaemonClient::remote_error(&error).is_some_and(|e| e.code == code::BUSY) =>
            {
                std::thread::sleep(BUSY_RETRY_BACKOFF);
                self.call(method::SYNTHESIZE, &params)
            }
            other => other,
        }
        .map_err(|error| error.to_string())
    }

    /// Ask the resident daemon to drain and exit.
    ///
    /// The daemon reads `setup.json` (voice, speed, language, model dir) only
    /// at startup, so a settings change takes effect on the next synthesis
    /// only after this. The next request activates a fresh daemon.
    pub fn shutdown(&self) -> Result<ShutdownResult, String> {
        let value = self
            .client
            .call_with_budget(
                method::SHUTDOWN,
                serde_json::Value::Object(serde_json::Map::new()),
                SHUTDOWN_BUDGET,
            )
            .map_err(|error| error.to_string())?;
        ipc::from_value(value).map_err(|error| error.to_string())
    }

    fn call(&self, method: &str, params: &SynthesizeParams) -> Result<SynthesizeResult, Ipc> {
        let value = self
            .client
            .call_with_budget(method, ipc::to_value(params)?, SYNTH_BUDGET)?;
        ipc::from_value(value)
    }
}
