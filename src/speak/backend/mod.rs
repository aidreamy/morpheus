//! TTS backends: one module per model family.
//!
//! [`Backend`] is the public selector carried by
//! [`SpeakConfig`](super::SpeakConfig); [`BackendEngine`] is what
//! [`Speak`](super::Speak) talks to once the model is loaded. Playback,
//! phonemization, [`Speech`], the error type and the public API of `Speak`
//! know nothing about any particular model.
//!
//! # Adding a model family
//!
//! 1. Add `mod family;` below and write the loader in `family.rs`.
//! 2. Add a variant to [`Backend`] carrying that family's config type.
//! 3. Add the one match arm in [`Backend::load`].
//!
//! Its config type implements [`Default`] the same way [`KokoroConfig`] does,
//! and its engine implements [`BackendEngine`]. No other file changes.

mod kokoro;

use std::path::PathBuf;

use super::config::{Phonemizer, SpeakConfig, SpeakError};
use super::engine::Speech;
use super::voice::Voice;

pub use kokoro::{
    DEFAULT_MODEL_DIR_NAME, KokoroConfig, MODEL_DIR_ENV, default_model_dir, models_dir,
};

/// Which model family a [`Speak`](super::Speak) loads, with that family's
/// own settings.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Backend {
    /// Kokoro through sherpa-onnx (`kokoro-multi-lang-v1_0`,
    /// `kokoro-multi-lang-v1_1`, ...).
    Kokoro(KokoroConfig),
}

impl Default for Backend {
    fn default() -> Self {
        Self::Kokoro(KokoroConfig::default())
    }
}

impl Backend {
    /// Load the selected model. The per-session settings (`voice`, `speed`,
    /// `num_threads`, `phonemizer`) come from `config`.
    pub(crate) fn load(&self, config: &SpeakConfig) -> Result<Box<dyn BackendEngine>, SpeakError> {
        match self {
            Self::Kokoro(model) => model.load(config).map(|engine| Box::new(engine) as _),
        }
    }
}

/// What a model family has to provide: synthesis plus the facts about the
/// loaded engine that callers ask for. Everything backend-specific stops at
/// this boundary.
pub(crate) trait BackendEngine: Send + Sync {
    /// Synthesize `request.text`, optionally streaming chunks to `sink`.
    ///
    /// A backend that cannot stream may ignore `sink` and generate
    /// everything at once.
    fn synthesize(&self, request: &Request<'_>, sink: ChunkSink) -> Result<Speech, SpeakError>;

    /// Output sample rate in Hz.
    fn sample_rate(&self) -> u32;

    /// Number of speakers the loaded model reports (at least one).
    fn num_speakers(&self) -> u32;

    /// Language metadata recorded in the model, e.g. `"zh en-us"`. Empty when
    /// the model carries none.
    fn lang(&self) -> String;

    /// Published names for the model's speakers; empty when unknown (the ids
    /// remain usable).
    fn voices(&self) -> &'static [Voice];

    /// Lexicon files the engine tokenizes with, when the backend has them.
    fn lexicons(&self) -> &[PathBuf] {
        &[]
    }
}

/// One synthesis request: the caller's text plus the session settings that
/// shape it. A backend uses what applies to it and ignores the rest.
pub(crate) struct Request<'a> {
    pub text: &'a str,
    pub voice: i32,
    pub speed: f32,
    /// Only read by backends whose text frontend is configurable.
    #[cfg_attr(not(feature = "speak-phonemize"), allow(dead_code))]
    pub phonemizer: &'a Phonemizer,
}

/// Per-chunk callback handed to the backend. Boxed and `'static` because
/// sherpa-onnx's C callback outlives the borrow.
pub(crate) type ChunkSink = Box<dyn FnMut(&[f32], f32) -> bool + 'static>;
