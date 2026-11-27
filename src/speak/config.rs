//! Engine configuration and errors for the `speak` module.
//!
//! [`SpeakConfig`] holds only the choices that belong to a voice session:
//! which [`Backend`] to load, the speaker, the speed, the inference threads
//! and the text frontend. Everything specific to a model family (where its
//! files live, its lexicons, its rule FSTs) lives in that backend's own
//! configuration type. No native handles are kept here, so it is cheap to
//! clone and can be built before the model is loaded.

use std::path::PathBuf;

use super::backend::{Backend, MODEL_DIR_ENV};

/// Which text frontend turns written text into the phonemes the model
/// consumes.
///
/// sherpa-onnx 2.x no longer embeds espeak-ng: the built-in frontend is a
/// lexicon table, and anything else must be phonemized before generation.
/// Kokoro accepts pre-phonemized Unicode codepoints for exactly that case.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Phonemizer {
    /// The backend's own frontend: for Kokoro this is sherpa-onnx's lexicon
    /// table, which tokenizes with the `lexicon-*.txt` files shipped inside
    /// the model bundle (English and Chinese in both multilingual bundles)
    /// and needs nothing else.
    Lexicon,
    /// Phonemize outside the engine and hand the model the codepoints.
    ///
    /// This is the path for Kokoro's other languages: their voices are
    /// present in `voices.bin` (Spanish, French, Hindi, Italian, Japanese,
    /// Brazilian Portuguese) but sherpa-onnx ships no lexicon for them.
    /// Spanish, French, Hindi, Italian and Brazilian Portuguese use
    /// `piper-phonemize` (espeak-ng, GPL-3.0-or-later); `"ja"` uses the
    /// built-in dictionary-based Japanese frontend instead, because espeak-ng
    /// cannot read kanji and the model was trained on cutlet phonemes.
    /// Requires the `speak-phonemize` feature.
    #[cfg(feature = "speak-phonemize")]
    Piper {
        /// espeak-ng voice name, e.g. `"en-us"`, `"pt-br"`, `"fr"`.
        lang: String,
    },
}

/// One TTS engine configuration.
///
/// [`Default`] selects the Kokoro backend pointed at its
/// [`default_model_dir`](super::default_model_dir), with speaker 0, natural
/// speed and the lexicon frontend.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakConfig {
    /// Which model family to load and where its files are. See [`Backend`].
    pub backend: Backend,
    /// Speaker id for the loaded model. [`super::Speak::voices`] maps the
    /// ids of the loaded model to their names.
    pub voice: i32,
    /// Speech speed factor: `1.0` is the model's natural pace, larger is
    /// faster and smaller is slower (sherpa-onnx's `GenerationConfig::speed`
    /// semantics — not the inverse `length_scale` of the model config). Must
    /// be greater than zero.
    pub speed: f32,
    /// ONNX Runtime inference threads. Two is a good default on a laptop;
    /// speech generation is bursty, so more threads rarely help.
    pub num_threads: i32,
    /// Text frontend. See [`Phonemizer`].
    pub phonemizer: Phonemizer,
}

impl Default for SpeakConfig {
    fn default() -> Self {
        Self {
            backend: Backend::default(),
            voice: 0,
            speed: 1.0,
            num_threads: 2,
            phonemizer: Phonemizer::Lexicon,
        }
    }
}

/// Everything `speak` can fail with.
///
/// The model errors carry the offending path so the message names what the
/// user has to fix instead of only which step failed. Backends that need a
/// failure mode of their own add a variant here.
#[derive(Debug, thiserror::Error)]
pub enum SpeakError {
    #[error(
        "model directory `{0}` does not exist. Download a bundle from \
         https://github.com/k2-fsa/sherpa-onnx/releases/tag/tts-models and extract \
         it there, or point `{env}` at an existing extracted bundle",
        env = MODEL_DIR_ENV
    )]
    ModelDir(PathBuf),
    #[error(
        "`{0}` is missing from `{1}`. Expected a model bundle with \
         model.onnx (or model.int8.onnx), tokens.txt and voices.bin"
    )]
    ModelFile(String, PathBuf),
    #[error("required model file `{0}` does not exist")]
    MissingFile(PathBuf),
    #[error(
        "`{0}` contains no `lexicon-*.txt` and the lexicon frontend needs one. \
         Use a bundle that ships lexicons, list them in the backend config, or \
         enable the `speak-phonemize` feature and select `Phonemizer::Piper`"
    )]
    NoLexicon(PathBuf),
    #[error("path `{0}` is not valid UTF-8; sherpa-onnx takes file paths as UTF-8")]
    NonUtf8Path(PathBuf),
    #[error("sherpa-onnx could not load the model in `{0}` (see its log above)")]
    Engine(PathBuf),
    #[error("voice id {sid} is out of range: this model has {speakers} speakers")]
    VoiceOutOfRange { sid: i32, speakers: u32 },
    #[error("speed must be greater than zero, got {0}")]
    BadSpeed(f32),
    #[error("nothing was synthesized: the text is empty or has no speakable content")]
    NoAudio,
    #[error("could not write the WAV file `{path}`")]
    Wav { path: PathBuf },
    #[error("no audio player played the generated audio (tried: {0})")]
    NoPlayer(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[cfg(feature = "speak-phonemize")]
    #[error(
        "piper-phonemize (espeak-ng) could not phonemize the text for language \
         `{lang}`. The selected voice needs an external frontend (espeak-ng or \
         the Japanese dictionary); check that `voice.language` matches the \
         speaker's language (see the voice docs)"
    )]
    Phonemize { lang: String },
}
