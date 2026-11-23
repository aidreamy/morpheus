//! The voice daemon's server: accept loop, protocol routing, resident model,
//! synthesis serialization, drain.
//!
//! Threading model mirrors the decision daemon: the accept loop runs on the
//! main thread and each connection gets a thread. The engine state is a mutex
//! that holds BOTH the lazily-loaded [`Speak`] and the synthesis slot — two
//! syntheses would only contend for the same cores, so serializing at the
//! engine is free. A call that cannot acquire the engine within the queue
//! window answers [`code::BUSY`]; the client retries once.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::daemon::ipc::{self, Message, RpcError};
use crate::daemon::transport::Listener;
use crate::speak::{BassEnvelope, KokoroConfig, Phonemizer, Speak, SpeakConfig};

use super::protocol::{
    self, HealthResult, HelloParams, HelloResult, ShutdownResult, SynthesizeParams,
    SynthesizeResult, code, method,
};

/// How long a synthesis waits for the engine before BUSY.
const SYNTH_QUEUE_WINDOW: Duration = Duration::from_secs(30);

/// Per-call read/write budget. The first call may load the model (seconds).
const CONNECTION_BUDGET: Duration = Duration::from_secs(180);

/// Idle shutdown default: long enough that a working session never bounces the
/// daemon, short enough that an abandoned one does not linger.
pub(crate) const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// The daemon's version, from the crate itself.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Load-time settings the daemon reads from `setup.json` at startup.
#[derive(Debug, Clone)]
pub(crate) struct Settings {
    pub voice: i32,
    pub speed: f32,
    #[cfg_attr(not(feature = "speak-phonemize"), allow(dead_code))]
    pub language: String,
    pub model_dir: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            voice: 0,
            speed: 1.0,
            language: String::new(),
            model_dir: String::new(),
        }
    }
}

impl Settings {
    fn speak_config(&self) -> SpeakConfig {
        let backend = if self.model_dir.trim().is_empty() {
            crate::speak::Backend::default()
        } else {
            crate::speak::Backend::Kokoro(KokoroConfig {
                model_dir: PathBuf::from(self.model_dir.trim()),
                ..KokoroConfig::default()
            })
        };
        #[cfg(feature = "speak-phonemize")]
        let phonemizer = if self.language.trim().is_empty() {
            Phonemizer::Lexicon
        } else {
            Phonemizer::Piper {
                lang: self.language.trim().to_string(),
            }
        };
        #[cfg(not(feature = "speak-phonemize"))]
        let phonemizer = Phonemizer::Lexicon;
        SpeakConfig {
            backend,
            voice: self.voice,
            speed: self.speed,
            phonemizer,
            ..SpeakConfig::default()
        }
    }
}

/// The resident engine plus the in-flight synthesis slot.
struct Engine {
    speak: Option<Speak>,
    /// When a load failed, the retry window before trying again.
    failed_at: Option<Instant>,
}

impl Engine {
    const LOAD_RETRY: Duration = Duration::from_secs(60);
}

/// Per-connection handler state (the `S` of `ipc::serve_connection`).
struct ConnState {
    handshaken: bool,
}

pub(crate) struct Server {
    settings: Settings,
    engine: Mutex<Engine>,
    draining: AtomicBool,
    started: Instant,
    last_request_millis: AtomicU64,
    requests_served: AtomicU64,
    failed: AtomicU64,
}

impl Server {
    pub(crate) fn new(settings: Settings) -> Self {
        Self {
            settings,
            engine: Mutex::new(Engine {
                speak: None,
                failed_at: None,
            }),
            draining: AtomicBool::new(false),
            started: Instant::now(),
            last_request_millis: AtomicU64::new(0),
            requests_served: AtomicU64::new(0),
            failed: AtomicU64::new(0),
        }
    }

    /// Serve until the listener errors, the idle budget is exceeded, or
    /// `shutdown` drains the server.
    pub(crate) fn serve(self: Arc<Self>, listener: Listener, idle_timeout: Duration) {
        listener
            .set_nonblocking(true)
            .expect("listener nonblocking");
        let mut last_activity = Instant::now();
        let mut connections = Vec::new();

        loop {
            match listener.accept() {
                Ok((stream, _addr)) => {
                    last_activity = Instant::now();
                    let server = Arc::clone(&self);
                    connections.push(std::thread::spawn(move || {
                        stream.set_nonblocking(false).ok();
                        stream.set_read_timeout(Some(CONNECTION_BUDGET)).ok();
                        stream.set_write_timeout(Some(CONNECTION_BUDGET)).ok();
                        let mut state = ConnState { handshaken: false };
                        if let Err(e) =
                            ipc::serve_connection(stream, &mut state, |state, request| {
                                server.handle(request, &mut state.handshaken)
                            })
                        {
                            log::debug!("tts daemon: connection ended: {e}");
                        }
                    }));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if self.idle_for(last_activity) >= idle_timeout {
                        log::info!("tts daemon: idle for {idle_timeout:?}, exiting");
                        break;
                    }
                    connections.retain(|handle| !handle.is_finished());
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    log::warn!("tts daemon: accept failed: {e}");
                    std::thread::sleep(Duration::from_millis(500));
                }
                Err(e) => {
                    log::warn!("tts daemon: accept failed: {e}");
                    break;
                }
            }

            if self.draining.load(Ordering::SeqCst) {
                break;
            }
        }

        for handle in connections.drain(..) {
            let _ = handle.join();
        }
    }

    fn idle_for(&self, last_accept: Instant) -> Duration {
        let last_request_millis = self.last_request_millis.load(Ordering::Relaxed);
        if last_request_millis == 0 {
            last_accept.elapsed()
        } else {
            let since_request =
                Instant::now() - self.started - Duration::from_millis(last_request_millis);
            since_request.min(last_accept.elapsed())
        }
    }

    fn handle(&self, request: Message, handshaken: &mut bool) -> Option<Message> {
        let id = request.request_id()?.clone();
        let Message::Request { method, params, .. } = request else {
            return Some(Message::err(
                id,
                RpcError::new(RpcError::INVALID_REQUEST, "not a request"),
            ));
        };

        if !*handshaken && method != method::HELLO {
            return Some(Message::err(
                id,
                RpcError::new(code::HANDSHAKE, "handshake required: send hello first"),
            ));
        }

        self.last_request_millis
            .store(self.started.elapsed().as_millis() as u64, Ordering::Relaxed);

        if self.draining.load(Ordering::SeqCst) && method == method::SYNTHESIZE {
            return Some(Message::err(
                id,
                RpcError::new(code::DRAINING, "daemon is draining for shutdown"),
            ));
        }

        let response = match method.as_str() {
            method::HELLO => self.hello(id, params),
            method::SYNTHESIZE => self.synthesize(id, params),
            method::HEALTH => self.health(id),
            method::SHUTDOWN => self.shutdown(id),
            _ => Message::err(
                id,
                RpcError::new(
                    RpcError::METHOD_NOT_FOUND,
                    format!("unknown method: {method}"),
                ),
            ),
        };
        if method == method::HELLO
            && matches!(
                response,
                Message::Response {
                    result: Some(_),
                    error: None,
                    ..
                }
            )
        {
            *handshaken = true;
        }
        Some(response)
    }

    fn hello(&self, id: Value, params: Option<Value>) -> Message {
        let params: HelloParams = match params.map(ipc::from_value) {
            Some(Ok(params)) => params,
            _ => {
                return Message::err(
                    id,
                    RpcError::new(RpcError::INVALID_PARAMS, "hello params required"),
                );
            }
        };
        if params.protocol > protocol::PROTOCOL {
            return Message::err(
                id,
                RpcError::new(
                    code::HANDSHAKE,
                    format!(
                        "protocol {} newer than daemon {}",
                        params.protocol,
                        protocol::PROTOCOL
                    ),
                ),
            );
        }
        Message::ok(
            id,
            ipc::to_value(&HelloResult {
                protocol: protocol::PROTOCOL,
                daemon: protocol::NAME.to_owned(),
                version: VERSION.to_owned(),
            })
            .unwrap_or(Value::Null),
        )
    }

    fn synthesize(&self, id: Value, params: Option<Value>) -> Message {
        let params: SynthesizeParams = match params.map(ipc::from_value) {
            Some(Ok(params)) => params,
            _ => {
                return Message::err(
                    id,
                    RpcError::new(RpcError::INVALID_PARAMS, "synthesize params required"),
                );
            }
        };
        if params.text.trim().is_empty() {
            self.failed.fetch_add(1, Ordering::Relaxed);
            return Message::err(
                id,
                RpcError::new(code::SYNTHESIS, "nothing to synthesize: empty text"),
            );
        }

        let mut engine = match acquire_engine(&self.engine, SYNTH_QUEUE_WINDOW) {
            Ok(engine) => engine,
            Err(()) => {
                return Message::err(
                    id,
                    RpcError::new(code::BUSY, "synthesis slot not acquired in time"),
                );
            }
        };

        // Lazy load, with a retry window after a failure so one broken bundle
        // does not pay a multi-second load attempt on every turn.
        if engine.speak.is_none() {
            if engine
                .failed_at
                .is_some_and(|at| at.elapsed() < Engine::LOAD_RETRY)
            {
                self.failed.fetch_add(1, Ordering::Relaxed);
                return Message::err(
                    id,
                    RpcError::new(code::MODEL, "voice model unavailable; retry is throttled"),
                );
            }
            let config = self.settings.speak_config();
            match Speak::load(config) {
                Ok(speak) => {
                    log::info!(
                        "tts daemon: voice model loaded ({} speakers, {} Hz)",
                        speak.num_speakers(),
                        speak.sample_rate()
                    );
                    engine.speak = Some(speak);
                    engine.failed_at = None;
                }
                Err(error) => {
                    log::warn!("tts daemon: voice model load failed: {error}");
                    engine.failed_at = Some(Instant::now());
                    self.failed.fetch_add(1, Ordering::Relaxed);
                    return Message::err(
                        id,
                        RpcError::new(code::MODEL, format!("voice model load failed: {error}")),
                    );
                }
            }
        }
        let Some(speak) = engine.speak.as_mut() else {
            self.failed.fetch_add(1, Ordering::Relaxed);
            return Message::err(id, RpcError::new(code::MODEL, "voice model unavailable"));
        };

        // Per-request voice/speed overrides: the engine keeps one resident
        // model, the session settings only change its knobs.
        if let Some(voice) = params.voice
            && let Err(error) = speak.set_voice(voice)
        {
            self.failed.fetch_add(1, Ordering::Relaxed);
            return Message::err(
                id,
                RpcError::new(code::SYNTHESIS, format!("invalid voice: {error}")),
            );
        }
        if let Some(speed) = params.speed
            && let Err(error) = speak.set_speed(speed)
        {
            self.failed.fetch_add(1, Ordering::Relaxed);
            return Message::err(
                id,
                RpcError::new(code::SYNTHESIS, format!("invalid speed: {error}")),
            );
        }

        let speech = match speak.synthesize(&params.text) {
            Ok(speech) => speech,
            Err(error) => {
                self.failed.fetch_add(1, Ordering::Relaxed);
                return Message::err(
                    id,
                    RpcError::new(code::SYNTHESIS, format!("synthesis failed: {error}")),
                );
            }
        };

        let output = PathBuf::from(&params.output);
        if let Some(parent) = output.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            self.failed.fetch_add(1, Ordering::Relaxed);
            return Message::err(
                id,
                RpcError::new(
                    code::SYNTHESIS,
                    format!("cannot create {}: {error}", parent.display()),
                ),
            );
        }
        if let Err(error) = speech.save_wav(&output) {
            let _ = std::fs::remove_file(&output);
            self.failed.fetch_add(1, Ordering::Relaxed);
            return Message::err(
                id,
                RpcError::new(code::SYNTHESIS, format!("cannot write wav: {error}")),
            );
        }

        let fps = params.fps.max(1);
        let bars = params.bars.max(1);
        let envelope: BassEnvelope =
            crate::speak::bass_envelope(speech.samples(), speech.sample_rate(), fps, bars as usize);
        self.requests_served.fetch_add(1, Ordering::Relaxed);
        Message::ok(
            id,
            ipc::to_value(&SynthesizeResult {
                wav: output.to_string_lossy().into_owned(),
                duration_ms: speech.duration().as_millis() as u64,
                sample_rate: speech.sample_rate(),
                bars,
                fps,
                bass: envelope.values,
            })
            .unwrap_or(Value::Null),
        )
    }

    fn health(&self, id: Value) -> Message {
        let loaded = self
            .engine
            .lock()
            .map(|engine| engine.speak.is_some())
            .unwrap_or(false);
        Message::ok(
            id,
            ipc::to_value(&HealthResult {
                uptime_secs: self.started.elapsed().as_secs(),
                loaded,
                requests_served: self.requests_served.load(Ordering::Relaxed),
                failed: self.failed.load(Ordering::Relaxed),
            })
            .unwrap_or(Value::Null),
        )
    }

    fn shutdown(&self, id: Value) -> Message {
        self.draining.store(true, Ordering::SeqCst);
        Message::ok(
            id,
            ipc::to_value(&ShutdownResult { draining: true }).unwrap_or(Value::Null),
        )
    }
}

/// Poll for the engine/synthesis slot.
fn acquire_engine<'a>(
    slot: &'a Mutex<Engine>,
    window: Duration,
) -> Result<std::sync::MutexGuard<'a, Engine>, ()> {
    let deadline = Instant::now() + window;
    loop {
        match slot.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return Err(());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(std::sync::TryLockError::Poisoned(_)) => return Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speak::Backend;

    /// `setup.json`'s `voice` block is the daemon's only input: the speaker,
    /// the pace, the espeak language and the bundle directory must all land in
    /// the engine configuration. A field dropped here speaks the wrong voice
    /// or the wrong language without any error.
    #[test]
    fn the_setup_voice_block_reaches_the_engine() {
        let settings = Settings {
            voice: 42,
            speed: 0.9,
            language: "pt-br".to_string(),
            model_dir: r"C:\models\kokoro-multi-lang-v1_0".to_string(),
        };
        let config = settings.speak_config();
        assert_eq!(config.voice, 42);
        assert_eq!(config.speed, 0.9);
        match config.backend {
            Backend::Kokoro(kokoro) => {
                assert_eq!(
                    kokoro.model_dir,
                    PathBuf::from(r"C:\models\kokoro-multi-lang-v1_0")
                );
            }
            other => panic!("unexpected backend: {other:?}"),
        }
        #[cfg(feature = "speak-phonemize")]
        assert_eq!(
            config.phonemizer,
            Phonemizer::Piper {
                lang: "pt-br".to_string()
            }
        );
        #[cfg(not(feature = "speak-phonemize"))]
        assert_eq!(config.phonemizer, Phonemizer::Lexicon);
    }

    /// An empty `model_dir` means "the default bundle", not the current
    /// directory: the engine must keep its own default path.
    #[test]
    fn an_empty_model_dir_keeps_the_default_bundle() {
        let config = Settings::default().speak_config();
        assert_eq!(config.backend, Backend::default());
        // No language selected: the bundle's own lexicon frontend.
        assert_eq!(config.phonemizer, Phonemizer::Lexicon);
    }
}
