//! Local voice output: text-to-speech through `sherpa-onnx`, one backend per
//! model family.
//!
//! The module owns one loaded model and turns text into playable audio
//! without leaving the machine. Everything native lives in the backend:
//! `sherpa-onnx` resolves the model files and synthesizes, while this module
//! keeps the parts that do not depend on a model — the speaker catalog, the
//! stopwatch-free playback handoff and the optional external phonemizer.
//!
//! [`Backend`] selects the model family and carries its own settings;
//! [`Speak`] dispatches to it and works the same for every backend. Adding a
//! model family means a new file under `backend/`, one enum variant and one
//! match arm — see the `backend` module docs.
//!
//! # Enabling
//!
//! `speak` is behind the `speak` cargo feature because `sherpa-onnx` links a
//! static ONNX Runtime (`~100 MB`), which the default build deliberately
//! avoids. The optional `speak-phonemize` feature adds `piper-phonemize`
//! (espeak-ng); that crate is **GPL-3.0-or-later**, so it is off by default
//! and only the build that enables it takes on its obligations.
//!
//! ```toml
//! morpheus = { version = "1", features = ["speak"] }
//! ```
//!
//! # Model
//!
//! Download the multilingual Kokoro bundle once and extract it where
//! [`default_model_dir`] looks (`<cache>/morpheus/models/kokoro-multi-lang-v1_1`,
//! overridable with [`MODEL_DIR_ENV`]):
//!
//! ```text
//! curl -L -o kokoro.tar.bz2 \
//!   https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-multi-lang-v1_1.tar.bz2
//! mkdir -p ~/.cache/morpheus/models
//! tar xf kokoro.tar.bz2 -C ~/.cache/morpheus/models
//! ```
//!
//! The directory must contain `model.onnx` (or the int8 `model.int8.onnx`),
//! `tokens.txt`, `voices.bin` and at least one `lexicon-*.txt`. The v1.0
//! bundle works the same way and is the one to use for its Brazilian
//! Portuguese, Spanish, French, Hindi, Italian and Japanese voices.
//!
//! # Languages
//!
//! sherpa-onnx 2.x dropped its embedded espeak-ng. Its own text frontend is
//! the lexicon table, which is why the multilingual bundles cover **Chinese
//! and English** — the only languages they ship lexicons for. Voices for the
//! other Kokoro languages are still present in `voices.bin`; driving them
//! needs the optional external phonemizer:
//!
//! ```text
//! SpeakConfig {
//!     phonemizer: Phonemizer::Piper { lang: "pt-br".into() },
//!     ..SpeakConfig::default()
//! }
//! ```
//!
//! # Example
//!
//! ```no_run
//! use morpheus::speak::{Speak, SpeakConfig};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let speak = Speak::load(SpeakConfig::default())?;
//! println!("{} speakers at {} Hz", speak.num_speakers(), speak.sample_rate());
//!
//! // Speak on the current thread (blocking) ...
//! speak.speak("Hello from morpheus.")?;
//!
//! // ... or synthesize now, play or save the owned samples later.
//! let speech = speak.synthesize("This part is only synthesized.")?;
//! speech.save_wav("greeting.wav")?;
//! # Ok(())
//! # }
//! ```
//!
//! # Threading
//!
//! `Speak` is `Send + Sync` and one instance can serve several worker
//! threads; loading it is the expensive step, so keep it alive. `Speech` is
//! an owned `Vec<f32>` (a backend's `GeneratedAudio` handle is neither
//! `Send` nor `Sync`), safe to hand to a playback thread. `Speech::play` and
//! `Speak::speak` block until the audio finishes, so call them off the TUI
//! event loop.

mod backend;
mod bass;
mod config;
mod engine;
#[cfg(feature = "speak-phonemize")]
mod phonemize;
mod player;
mod voice;

pub use backend::{
    Backend, DEFAULT_MODEL_DIR_NAME, KokoroConfig, MODEL_DIR_ENV, default_model_dir,
};
pub use bass::{BassEnvelope, bass_envelope};
pub use config::{Phonemizer, SpeakConfig, SpeakError};
pub use engine::{Speak, Speech};
pub use player::Playback;
pub use voice::{
    Voice, VoiceEntry, entries as voice_entries, entry as voice_entry, find as find_voice,
};
