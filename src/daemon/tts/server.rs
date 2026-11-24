//! The voice daemon's server: accept loop, protocol routing, resident model,
//! synthesis serialization, drain.
//!
//! Threading model mirrors the decision daemon: the accept loop runs on the
//! main thread and each connection gets a thread. The engine state is a mutex
//! that holds BOTH the lazily-loaded [`Speak`] and the synthesis slot — two
//! syntheses would only contend for the same cores, so serializing at the
//! engine is free. A call that cannot acquire the engine within the queue
//! window answers [`code::BUSY`]; the client retries once.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
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

/// Idle shutdown default (the backstop for a daemon whose clients never
/// greeted): long enough that a working session never bounces the daemon,
/// short enough that an abandoned one does not linger. A daemon whose
/// greeted clients all exited does not wait for this — see
/// [`Server::clients_gone`].
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
    /// The bundle directory the engine loads (the configured one, or the
    /// `speak` default when `setup.json` leaves `voice.model_dir` empty).
    fn model_dir(&self) -> PathBuf {
        if self.model_dir.trim().is_empty() {
            crate::speak::default_model_dir()
        } else {
            PathBuf::from(self.model_dir.trim())
        }
    }

    /// The language the text frontend should use, without having to repeat it
    /// in `setup.json`: an explicit `voice.language` wins, otherwise the
    /// language is the one the selected speaker was trained for. English and
    /// Chinese voices keep the bundle's lexicon (`None`).
    fn phonemizer_language(&self, model_dir: &Path) -> Option<String> {
        let explicit = self.language.trim();
        if !explicit.is_empty() {
            self.warn_on_language_mismatch(model_dir, explicit);
            // A consistent pair whose language the bundle covers with a
            // lexicon (`en-us`, `en-gb`, `zh`) stays on that lexicon: the
            // picker used to persist the language of every voice, which
            // pushed English and Chinese onto the external espeak-ng
            // frontend even though the bundle ships the phonemes those
            // voices were built around.
            let native_lexicon = crate::speak::voice_entry(model_dir, self.voice)
                .is_some_and(|entry| entry.language == explicit && !entry.needs_phonemize());
            if !native_lexicon {
                return Some(explicit.to_string());
            }
        }
        crate::speak::espeak_language(model_dir, self.voice).map(str::to_string)
    }

    /// A `speaker` and `language` that disagree speak the right id with the
    /// wrong phonemes, so the screenshot of a manual `setup.json` edit is
    /// logged once per model load instead of failing silently.
    fn warn_on_language_mismatch(&self, model_dir: &Path, language: &str) {
        let Some(entry) = crate::speak::voice_entry(model_dir, self.voice) else {
            return;
        };
        if entry.language != language {
            log::warn!(
                "tts daemon: voice.speaker {} is `{}` ({}), but voice.language is `{}`; \
                 the voice will sound wrong. Set voice.language to `{}` or empty to derive it",
                self.voice,
                entry.name,
                entry.language_label,
                language,
                entry.language
            );
        }
    }

    fn speak_config(&self) -> SpeakConfig {
        let model_dir = self.model_dir();
        let backend = crate::speak::Backend::Kokoro(KokoroConfig {
            model_dir: model_dir.clone(),
            ..KokoroConfig::default()
        });
        let language = self.phonemizer_language(&model_dir);
        let phonemizer = match language {
            #[cfg(feature = "speak-phonemize")]
            Some(lang) => Phonemizer::Piper { lang },
            // Without the external frontend the language cannot be honored;
            // the bundle lexicon still produces audio, so the engine keeps
            // working and the log names the build feature to enable.
            #[cfg(not(feature = "speak-phonemize"))]
            Some(lang) => {
                log::warn!(
                    "tts daemon: voice language `{lang}` needs the `speak-phonemize` cargo \
                     feature (espeak-ng or the Japanese dictionary); this build uses \
                     the bundle lexicon instead"
                );
                Phonemizer::Lexicon
            }
            None => Phonemizer::Lexicon,
        };
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
    /// The `morpheus` processes that greeted this daemon, from each
    /// connection's `hello`. Once none is alive the daemon exits, so closing
    /// the app does not leave a resident process holding the `.exe`.
    clients: Mutex<HashSet<u32>>,
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
            clients: Mutex::new(HashSet::new()),
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
                    connections.retain(|handle| !handle.is_finished());
                    if self.clients_gone() {
                        log::info!("tts daemon: every client exited, exiting");
                        break;
                    }
                    if self.idle_for(last_activity) >= idle_timeout {
                        log::info!("tts daemon: idle for {idle_timeout:?}, exiting");
                        break;
                    }
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

    /// Register a greeting client and forget the ones that died.
    ///
    /// `true` only after at least one client was seen and all of them are
    /// gone: a daemon that was activated but never greeted stays on the idle
    /// budget, so the activation race cannot make it exit under a client
    /// that is still connecting.
    fn clients_gone(&self) -> bool {
        let mut clients = self
            .clients
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if clients.is_empty() {
            return false;
        }
        clients.retain(|pid| crate::daemon::activation::is_process_alive(*pid));
        clients.is_empty()
    }

    fn note_client(&self, pid: u32) {
        self.clients
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(pid);
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
        // A greeted client keeps the daemon alive; once the last one exits
        // the serve loop returns, so no resident process holds the `.exe`.
        self.note_client(params.pid);
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
            // The three load-time knobs a wrong-sounding voice comes from:
            // bundle, speaker and text frontend. A log line makes "English
            // with a Portuguese speaker" obvious without re-reading
            // setup.json.
            let frontend = match &config.phonemizer {
                #[cfg(feature = "speak-phonemize")]
                Phonemizer::Piper { lang } => format!("phonemize/{lang}"),
                Phonemizer::Lexicon => "lexicon".to_string(),
            };
            log::info!(
                "tts daemon: loading speaker {} from {} ({frontend})",
                config.voice,
                self.settings.model_dir().display()
            );
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
        let Backend::Kokoro(kokoro) = config.backend;
        assert_eq!(
            kokoro.model_dir,
            PathBuf::from(r"C:\models\kokoro-multi-lang-v1_0")
        );
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

    /// `setup.json` does not have to repeat what the speaker already says: a
    /// Portuguese/Spanish/Japanese/... id alone selects its frontend language, while
    /// an English or Chinese id keeps the lexicon frontend.
    #[test]
    fn an_empty_language_is_derived_from_the_speaker() {
        let portuguese = Settings {
            voice: 42, // pf_dora
            language: String::new(),
            model_dir: "/models/kokoro-multi-lang-v1_0".into(),
            ..Settings::default()
        };
        let config = portuguese.speak_config();
        #[cfg(feature = "speak-phonemize")]
        assert_eq!(
            config.phonemizer,
            Phonemizer::Piper {
                lang: "pt-br".into()
            }
        );
        #[cfg(not(feature = "speak-phonemize"))]
        assert_eq!(config.phonemizer, Phonemizer::Lexicon);

        // af_heart (3) and zf_xiaobei (45) are English/Chinese: no external
        // frontend, exactly as before.
        for voice in [3, 45] {
            let settings = Settings {
                voice,
                language: String::new(),
                model_dir: "/models/kokoro-multi-lang-v1_0".into(),
                ..Settings::default()
            };
            assert_eq!(settings.speak_config().phonemizer, Phonemizer::Lexicon);
        }

        // An explicit language still wins over the derived one.
        let explicit = Settings {
            voice: 42,
            language: "es".into(),
            model_dir: "/models/kokoro-multi-lang-v1_0".into(),
            ..Settings::default()
        };
        #[cfg(feature = "speak-phonemize")]
        assert_eq!(
            explicit.speak_config().phonemizer,
            Phonemizer::Piper {
                lang: "es".into()
            }
        );
        #[cfg(not(feature = "speak-phonemize"))]
        assert_eq!(explicit.speak_config().phonemizer, Phonemizer::Lexicon);
    }

    /// An explicit language that is the voice's own is not a request to swap
    /// frontends: English and Chinese voices keep the bundle lexicon, while a
    /// genuinely mismatched language is still honored for manual overrides.
    #[test]
    fn an_explicit_lexicon_language_keeps_the_bundle_lexicon() {
        let english = Settings {
            voice: 3, // af_heart (en-us)
            language: "en-us".into(),
            model_dir: "/models/kokoro-multi-lang-v1_0".into(),
            ..Settings::default()
        };
        assert_eq!(english.speak_config().phonemizer, Phonemizer::Lexicon);

        let mismatched = Settings {
            voice: 3,
            language: "pt-br".into(),
            ..english
        };
        #[cfg(feature = "speak-phonemize")]
        assert_eq!(
            mismatched.speak_config().phonemizer,
            Phonemizer::Piper {
                lang: "pt-br".into()
            }
        );
        #[cfg(not(feature = "speak-phonemize"))]
        assert_eq!(mismatched.speak_config().phonemizer, Phonemizer::Lexicon);
    }

    /// One speaker per supported language, checked against the frontend the
    /// daemon must pick: English and Chinese stay on the bundle lexicon, every
    /// other language is routed to the external phonemizer with its own code.
    /// A new advertised language cannot ship without passing here.
    #[test]
    fn every_supported_language_selects_its_frontend() {
        let cases = [
            (3, "en-us", true),
            (20, "en-gb", true),
            (45, "zh", true),
            (28, "es", false),
            (30, "fr", false),
            (31, "hi", false),
            (35, "it", false),
            (37, "ja", false),
            (42, "pt-br", false),
        ];
        for (voice, language, lexicon) in cases {
            let settings = Settings {
                voice,
                language: String::new(),
                model_dir: "/models/kokoro-multi-lang-v1_0".into(),
                ..Settings::default()
            };
            let config = settings.speak_config();
            if lexicon {
                assert_eq!(config.phonemizer, Phonemizer::Lexicon, "{language}");
            } else {
                #[cfg(feature = "speak-phonemize")]
                assert_eq!(
                    config.phonemizer,
                    Phonemizer::Piper {
                        lang: language.into()
                    },
                    "{language}"
                );
                #[cfg(not(feature = "speak-phonemize"))]
                assert_eq!(config.phonemizer, Phonemizer::Lexicon, "{language}");
            }
        }
    }

    /// The serve loop must keep the daemon alive while one greeted client is
    /// alive and close it once none is — this is what stops a closed TUI from
    /// leaving `morpheus-ttsd.exe` resident (and locked) on Windows.
    #[test]
    fn the_daemon_exits_only_after_every_greeted_client_is_gone() {
        let server = Server::new(Settings::default());
        assert!(
            !server.clients_gone(),
            "a daemon nobody greeted is the idle timer's business"
        );

        server.note_client(std::process::id());
        assert!(!server.clients_gone(), "the test process itself is alive");

        server.note_client(u32::MAX);
        assert!(
            !server.clients_gone(),
            "one live client is enough to keep the daemon"
        );

        server
            .clients
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        server.note_client(u32::MAX);
        assert!(server.clients_gone(), "no live client left");
    }
}
