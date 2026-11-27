//! The synthesis engine: one loaded model, shared by its callers.
//!
//! [`Speak`] owns a backend engine behind [`BackendEngine`] and a
//! [`SpeakConfig`]. It keeps the backend-agnostic parts - validating the
//! session settings, tracking the speaker and speed, boxing the streaming
//! callback and owning the resulting samples - and delegates model work to
//! the backend selected in the configuration.
//!
//! Backends are `Send + Sync`, so one [`Speak`] can serve the TUI's worker
//! threads. A backend's native audio handle may be neither, so [`Speech`]
//! copies the samples into an owned `Vec<f32>` before returning: the caller
//! gets plain, movable data, not a native handle.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::backend::{BackendEngine, ChunkSink, Request};
use super::config::{SpeakConfig, SpeakError};
use super::player;
use super::voice::Voice;

/// A TTS engine bound to one model and one voice configuration.
///
/// Loading is the expensive step (a Kokoro model is ~300 MB), so build one
/// and keep it; synthesis calls only touch the loaded engine. Change the
/// speaker or speed on the instance with [`Speak::set_voice`] and
/// [`Speak::set_speed`] instead of reloading.
pub struct Speak {
    engine: Box<dyn BackendEngine>,
    config: SpeakConfig,
}

impl Speak {
    /// Load the backend selected in [`SpeakConfig::backend`].
    ///
    /// The session settings are validated here; the backend resolves and
    /// checks its own files before calling into the native engine, so a
    /// missing piece reports its own path instead of a generic failure.
    pub fn load(config: SpeakConfig) -> Result<Self, SpeakError> {
        if !(config.speed > 0.0 && config.speed.is_finite()) {
            return Err(SpeakError::BadSpeed(config.speed));
        }
        let engine = config.backend.load(&config)?;
        Ok(Self { engine, config })
    }

    /// The configuration this engine was loaded with.
    #[must_use]
    pub fn config(&self) -> &SpeakConfig {
        &self.config
    }

    /// Output sample rate in Hz (24000 for both Kokoro multilingual bundles).
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.engine.sample_rate()
    }

    /// Number of speakers the loaded model reports.
    #[must_use]
    pub fn num_speakers(&self) -> u32 {
        self.engine.num_speakers()
    }

    /// Language metadata recorded in the model, e.g. `"zh en-us"`.
    /// Empty when the model carries none.
    #[must_use]
    pub fn lang(&self) -> String {
        self.engine.lang()
    }

    /// Lexicon files the engine is tokenizing with (empty when the backend
    /// has no lexicons).
    #[must_use]
    pub fn lexicons(&self) -> &[PathBuf] {
        self.engine.lexicons()
    }

    /// Names for the loaded model's speakers; empty when the bundle is not a
    /// known release (the ids remain usable).
    #[must_use]
    pub fn voices(&self) -> &'static [Voice] {
        self.engine.voices()
    }

    /// Look up one of this model's voices by name, e.g. `"af_maple"`.
    #[must_use]
    pub fn voice(&self, name: &str) -> Option<Voice> {
        self.voices().iter().copied().find(|v| v.name == name)
    }

    /// Select a speaker for later synthesis calls.
    pub fn set_voice(&mut self, sid: i32) -> Result<(), SpeakError> {
        if sid < 0 || sid as u32 >= self.num_speakers() {
            return Err(SpeakError::VoiceOutOfRange {
                sid,
                speakers: self.num_speakers(),
            });
        }
        self.config.voice = sid;
        Ok(())
    }

    /// Set the speech pace for later synthesis calls (`> 0`; `1.0` natural).
    pub fn set_speed(&mut self, speed: f32) -> Result<(), SpeakError> {
        if !(speed > 0.0 && speed.is_finite()) {
            return Err(SpeakError::BadSpeed(speed));
        }
        self.config.speed = speed;
        Ok(())
    }

    /// Synthesize `text` into one owned buffer.
    pub fn synthesize(&self, text: &str) -> Result<Speech, SpeakError> {
        self.synthesize_streaming(text, |_, _| true)
    }

    /// Synthesize `text`, handing each generated chunk to `on_chunk`.
    ///
    /// The backend splits the text into sentences, runs them one at a time
    /// and calls back with that sentence's samples and the fraction finished
    /// (`0.0..=1.0`) - a player can start speaking before the last sentence
    /// is generated. Returning `false` stops generation early and keeps only
    /// what was produced so far. The callback sees each chunk once, not the
    /// accumulated audio.
    ///
    /// The callback must be `'static` because a native callback may outlive
    /// the borrow; send chunks through a channel if they have to reach a
    /// caller-owned sink.
    pub fn synthesize_streaming<F>(&self, text: &str, on_chunk: F) -> Result<Speech, SpeakError>
    where
        F: FnMut(&[f32], f32) -> bool + 'static,
    {
        if text.trim().is_empty() {
            return Err(SpeakError::NoAudio);
        }

        let request = Request {
            text,
            voice: self.config.voice,
            speed: self.config.speed,
            phonemizer: &self.config.phonemizer,
        };
        let sink: ChunkSink = Box::new(on_chunk);
        let speech = self.engine.synthesize(&request, sink)?;
        if speech.samples().is_empty() {
            return Err(SpeakError::NoAudio);
        }
        Ok(speech)
    }

    /// Synthesize `text` and play it through the system audio player,
    /// blocking until playback finishes.
    ///
    /// Call this from a worker thread: generation and playback together take
    /// roughly the duration of the speech plus the synthesis time.
    pub fn speak(&self, text: &str) -> Result<Speech, SpeakError> {
        let speech = self.synthesize(text)?;
        speech.play()?;
        Ok(speech)
    }
}

/// One synthesized utterance, owned and movable across threads.
#[derive(Debug, Clone, PartialEq)]
pub struct Speech {
    samples: Vec<f32>,
    sample_rate: u32,
}

impl Speech {
    /// Wrap raw mono samples produced by a backend.
    pub(crate) fn new(samples: Vec<f32>, sample_rate: u32) -> Self {
        Self {
            samples,
            sample_rate,
        }
    }

    /// Normalized `f32` PCM samples (mono).
    #[must_use]
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// Output sample rate in Hz.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Length of the utterance.
    #[must_use]
    pub fn duration(&self) -> Duration {
        if self.sample_rate == 0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(self.samples.len() as f64 / self.sample_rate as f64)
    }

    /// Consume the utterance and take the samples.
    #[must_use]
    pub fn into_samples(self) -> Vec<f32> {
        self.samples
    }

    /// Write the utterance as a WAV file with sherpa-onnx's own writer.
    pub fn save_wav(&self, path: impl AsRef<Path>) -> Result<(), SpeakError> {
        let path = path.as_ref();
        let name = path
            .to_str()
            .ok_or_else(|| SpeakError::NonUtf8Path(path.to_path_buf()))?;
        if sherpa_onnx::write(name, &self.samples, self.sample_rate as i32) {
            Ok(())
        } else {
            Err(SpeakError::Wav {
                path: path.to_path_buf(),
            })
        }
    }

    /// Play the utterance through the system player, blocking until done.
    pub fn play(&self) -> Result<(), SpeakError> {
        player::play(self)
    }
}

#[cfg(all(test, feature = "speak-phonemize"))]
mod tests {
    use super::*;
    use crate::speak::config::Phonemizer;

    /// The last line of defence: every advertised language must actually
    /// produce audio through a real v1.0 speaker, not just select a frontend.
    /// Ignored because it loads the ~300 MB Kokoro bundle; run it before
    /// releasing a voice change:
    ///
    /// ```text
    /// cargo test --features speak-phonemize --lib -- --ignored every_supported_language
    /// ```
    #[test]
    #[ignore = "loads the Kokoro model; run before releasing voice changes"]
    fn every_supported_language_synthesizes_audio() {
        let default = crate::speak::default_model_dir();
        let root = default.parent().expect("the model dir has a parent");
        let bundle = root.join("kokoro-multi-lang-v1_0");
        assert!(
            bundle.join("model.onnx").is_file(),
            "install kokoro-multi-lang-v1_0 in {} to run this guarantee test",
            root.display()
        );

        let cases = [
            (3, Phonemizer::Lexicon, "Hello from Morpheus."),
            (20, Phonemizer::Lexicon, "Hello from Morpheus."),
            (45, Phonemizer::Lexicon, "你好，世界。"),
            (
                28,
                Phonemizer::Piper { lang: "es".into() },
                "Hola, mundo. ¿Cómo estás?",
            ),
            (
                30,
                Phonemizer::Piper { lang: "fr".into() },
                "Bonjour, comment ça va ?",
            ),
            (
                31,
                Phonemizer::Piper { lang: "hi".into() },
                "नमस्ते, आप कैसे हैं?",
            ),
            (
                35,
                Phonemizer::Piper { lang: "it".into() },
                "Ciao, come stai?",
            ),
            (
                37,
                Phonemizer::Piper { lang: "ja".into() },
                "今日はいい天気ですね。",
            ),
            (
                42,
                Phonemizer::Piper {
                    lang: "pt-br".into(),
                },
                "Olá, tudo bem?",
            ),
        ];

        for (voice, phonemizer, text) in cases {
            let language = match &phonemizer {
                Phonemizer::Lexicon => "lexicon".to_owned(),
                Phonemizer::Piper { lang } => lang.clone(),
            };
            let config = SpeakConfig {
                backend: crate::speak::Backend::Kokoro(crate::speak::KokoroConfig {
                    model_dir: bundle.clone(),
                    ..Default::default()
                }),
                voice,
                speed: 1.0,
                num_threads: 2,
                phonemizer,
            };
            let speak = Speak::load(config)
                .unwrap_or_else(|error| panic!("{language} speaker {voice}: {error}"));
            let speech = speak
                .synthesize(text)
                .unwrap_or_else(|error| panic!("{language} speaker {voice}: {error}"));
            assert!(
                speech.duration() > Duration::from_millis(300),
                "{language} speaker {voice} produced only {:?} of audio",
                speech.duration()
            );
        }
    }
}
