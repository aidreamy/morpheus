//! Kokoro backend: one loaded `sherpa-onnx` offline TTS engine.
//!
//! This module owns everything that is true of Kokoro bundles only: how the
//! directory is laid out, which files are required, the lexicons and rule
//! FSTs, the speaker catalogs and the sherpa-onnx model configuration.
//! `sherpa_onnx::OfflineTts` is `Send + Sync`, so the engine built here can
//! serve the TUI's worker threads.

use std::env;
use std::path::{Path, PathBuf};

use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsModelConfig,
};

use super::{BackendEngine, ChunkSink, Request};
use crate::speak::config::{Phonemizer, SpeakConfig, SpeakError};
use crate::speak::engine::Speech;
use crate::speak::voice::{self, Voice};

/// Environment variable that overrides [`default_model_dir`].
pub const MODEL_DIR_ENV: &str = "MORPHEUS_KOKORO_MODEL_DIR";

/// Bundle name of the recommended Kokoro model: v1.1, Chinese + English,
/// 103 speakers. The v1.0 bundle (54 speakers) loads through the same code;
/// set [`KokoroConfig::model_dir`] to it.
pub const DEFAULT_MODEL_DIR_NAME: &str = "kokoro-multi-lang-v1_1";

/// Where the Kokoro bundle is expected when no explicit directory is given:
/// `$MORPHEUS_KOKORO_MODEL_DIR` when set, otherwise
/// `<user cache>/morpheus/models/kokoro-multi-lang-v1_1`.
///
/// When the recommended v1.1 bundle is not installed, another complete bundle
/// beside it is used instead (newest first). That keeps the historical v1.0
/// install working without an env var: v1.0 is the only release with
/// Brazilian Portuguese, Spanish, French, Hindi, Italian and Japanese voices,
/// while v1.1 only adds Chinese and English ones. With no bundle installed the
/// v1.1 path is returned unchanged, so the error message still names the
/// directory the user is expected to create.
#[must_use]
pub fn default_model_dir() -> PathBuf {
    if let Some(dir) = env::var_os(MODEL_DIR_ENV).filter(|value| !value.is_empty()) {
        return PathBuf::from(dir);
    }
    let root = directories::BaseDirs::new()
        .map(|dirs| dirs.cache_dir().to_path_buf())
        .unwrap_or_else(env::temp_dir);
    let models = root.join("morpheus").join("models");
    let preferred = models.join(DEFAULT_MODEL_DIR_NAME);
    if preferred.is_dir() {
        return preferred;
    }
    if let Some(found) = crate::speak::installed_bundles(&models).into_iter().next() {
        log::debug!(
            "kokoro: {} is not installed; using the installed bundle {}",
            preferred.display(),
            found.display()
        );
        return found;
    }
    preferred
}

/// Settings for the Kokoro backend.
///
/// [`Default`] points at [`default_model_dir`] and discovers the lexicons and
/// rule FSTs inside the bundle.
#[derive(Debug, Clone, PartialEq)]
pub struct KokoroConfig {
    /// Directory holding the extracted bundle (`model.onnx`, `voices.bin`,
    /// `tokens.txt`, `lexicon-*.txt`).
    pub model_dir: PathBuf,
    /// Lexicon files to use instead of the auto-discovered
    /// `<model_dir>/lexicon-*.txt`. Each path is validated at load time.
    pub lexicons: Vec<PathBuf>,
    /// Text normalization rule FSTs (comma-joined into sherpa-onnx's
    /// `rule_fsts`). The Chinese bundles ship `date-zh.fst`, `phone-zh.fst`
    /// and `number-zh.fst` for dates, phone numbers and digits; empty leaves
    /// normalization to the lexicon.
    pub rule_fsts: Vec<PathBuf>,
}

impl Default for KokoroConfig {
    fn default() -> Self {
        Self {
            model_dir: default_model_dir(),
            lexicons: Vec::new(),
            rule_fsts: Vec::new(),
        }
    }
}

impl KokoroConfig {
    /// Resolve the bundle files, load the model and validate the speaker.
    ///
    /// All files are checked before sherpa-onnx is called, so a missing piece
    /// reports its own path instead of a generic engine failure.
    pub(crate) fn load(&self, config: &SpeakConfig) -> Result<KokoroEngine, SpeakError> {
        let files = ModelFiles::resolve(self, &config.phonemizer)?;

        #[cfg(feature = "speak-phonemize")]
        {
            // piper-phonemize's embedded espeak-ng data ships no per-language
            // dictionaries, so Spanish, French, Hindi and Italian would
            // phonemize to nothing. The bundle carries the full data; the
            // first initialization in the process wins.
            let data = self.model_dir.join("espeak-ng-data");
            if data.join("phontab").is_file()
                && let Err(error) = crate::speak::phonemize::initialize(&data)
            {
                log::warn!(
                    "kokoro: could not initialize espeak-ng from {}: {error}",
                    data.display()
                );
            }
        }

        let mut sherpa_config = OfflineTtsConfig {
            model: OfflineTtsModelConfig {
                kokoro: OfflineTtsKokoroModelConfig {
                    model: Some(files.model),
                    voices: Some(files.voices),
                    tokens: Some(files.tokens),
                    lexicon: files.lexicon,
                    // The per-request `speed` already applies the pace;
                    // leaving the model-level scale at its neutral value
                    // keeps the two from multiplying.
                    length_scale: 1.0,
                    ..Default::default()
                },
                num_threads: config.num_threads.max(1),
                ..Default::default()
            },
            rule_fsts: files.rule_fsts,
            // Kokoro ignores anything but 1 and logs a warning otherwise:
            // it batch-splits sentences itself, one sentence per Process call.
            max_num_sentences: 1,
            ..Default::default()
        };
        sherpa_config.silence_scale = 1.0;

        let tts = OfflineTts::create(&sherpa_config)
            .ok_or_else(|| SpeakError::Engine(self.model_dir.clone()))?;
        let speakers = tts.num_speakers().max(1) as u32;
        if config.voice < 0 || config.voice as u32 >= speakers {
            return Err(SpeakError::VoiceOutOfRange {
                sid: config.voice,
                speakers,
            });
        }

        Ok(KokoroEngine {
            voices: voice::catalog(&self.model_dir, speakers),
            lexicons: files.lexicon_paths,
            tts,
        })
    }
}

/// The loaded Kokoro engine. `Speak` drives it through [`BackendEngine`].
pub(crate) struct KokoroEngine {
    tts: OfflineTts,
    voices: &'static [Voice],
    lexicons: Vec<PathBuf>,
}

impl KokoroEngine {
    /// Build the per-request options, moving the text through the external
    /// phonemizer first when one is selected.
    fn generation(&self, request: &Request<'_>) -> Result<(String, GenerationConfig), SpeakError> {
        let generation = GenerationConfig {
            sid: request.voice,
            speed: request.speed,
            silence_scale: 1.0,
            ..Default::default()
        };

        #[cfg(feature = "speak-phonemize")]
        if let Phonemizer::Piper { lang } = request.phonemizer {
            let codepoints = crate::speak::phonemize::codepoints(request.text, lang)?;
            // The text was consumed by the phonemizer; sherpa-onnx reads the
            // codepoints instead.
            return Ok((
                String::new(),
                GenerationConfig {
                    phoneme_codepoints: Some(codepoints),
                    ..generation
                },
            ));
        }

        Ok((request.text.to_string(), generation))
    }
}

impl BackendEngine for KokoroEngine {
    fn synthesize(&self, request: &Request<'_>, sink: ChunkSink) -> Result<Speech, SpeakError> {
        if request.text.trim().is_empty() {
            return Err(SpeakError::NoAudio);
        }

        let (text, generation) = self.generation(request)?;

        // sherpa-onnx owns the callback for the whole generation, so the
        // boxed sink moves into a `'static` closure.
        let mut sink = sink;
        let callback = move |samples: &[f32], progress: f32| sink(samples, progress);

        let audio = self
            .tts
            .generate_with_config(&text, &generation, Some(callback))
            .ok_or(SpeakError::NoAudio)?;
        let speech = Speech::new(audio.samples().to_vec(), audio.sample_rate().max(0) as u32);
        if speech.samples().is_empty() {
            return Err(SpeakError::NoAudio);
        }
        Ok(speech)
    }

    fn sample_rate(&self) -> u32 {
        self.tts.sample_rate().max(0) as u32
    }

    fn num_speakers(&self) -> u32 {
        self.tts.num_speakers().max(1) as u32
    }

    fn lang(&self) -> String {
        self.tts.lang()
    }

    fn voices(&self) -> &'static [Voice] {
        self.voices
    }

    fn lexicons(&self) -> &[PathBuf] {
        &self.lexicons
    }
}

/// The bundle files, resolved and validated before the model is loaded.
#[derive(Debug)]
struct ModelFiles {
    model: String,
    voices: String,
    tokens: String,
    lexicon: Option<String>,
    lexicon_paths: Vec<PathBuf>,
    rule_fsts: Option<String>,
}

impl ModelFiles {
    fn resolve(config: &KokoroConfig, phonemizer: &Phonemizer) -> Result<Self, SpeakError> {
        let dir = &config.model_dir;
        if !dir.is_dir() {
            return Err(SpeakError::ModelDir(dir.clone()));
        }

        // Both releases ship `model.onnx`; the int8 tarball ships
        // `model.int8.onnx`. Accept either without an extra setting.
        let model = ["model.onnx", "model.int8.onnx"]
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path.is_file())
            .ok_or_else(|| {
                SpeakError::ModelFile("model.onnx or model.int8.onnx".into(), dir.clone())
            })?;
        let tokens = required(dir, "tokens.txt")?;
        let voices = required(dir, "voices.bin")?;

        let lexicons: Vec<PathBuf> = if config.lexicons.is_empty() {
            discovered_lexicons(dir)?
        } else {
            for path in &config.lexicons {
                if !path.is_file() {
                    return Err(SpeakError::MissingFile(path.clone()));
                }
            }
            config.lexicons.clone()
        };
        if matches!(phonemizer, Phonemizer::Lexicon) && lexicons.is_empty() {
            return Err(SpeakError::NoLexicon(dir.clone()));
        }

        let mut rule_fsts = config.rule_fsts.clone();
        rule_fsts.retain(|path| path.is_file());

        Ok(Self {
            model: utf8(&model)?,
            tokens: utf8(&tokens)?,
            voices: utf8(&voices)?,
            lexicon: join_utf8(&lexicons)?,
            lexicon_paths: lexicons,
            rule_fsts: join_utf8(&rule_fsts)?,
        })
    }
}

fn required(dir: &Path, name: &str) -> Result<PathBuf, SpeakError> {
    let path = dir.join(name);
    if path.is_file() {
        Ok(path)
    } else {
        Err(SpeakError::MissingFile(path))
    }
}

/// Every `lexicon-*.txt` in the bundle, in stable order.
///
/// The official bundles ship `lexicon-us-en.txt`, `lexicon-gb-en.txt` and
/// `lexicon-zh.txt`; sherpa-onnx merges them into one lookup table, so the
/// order only decides which entry wins on a collision (the later file).
/// Sorted order puts `us-en` after `gb-en`, matching the upstream English
/// examples.
fn discovered_lexicons(dir: &Path) -> Result<Vec<PathBuf>, SpeakError> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("lexicon-") && name.ends_with(".txt"))
        })
        .collect();
    found.sort();
    Ok(found)
}

fn utf8(path: &Path) -> Result<String, SpeakError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| SpeakError::NonUtf8Path(path.to_path_buf()))
}

fn join_utf8(paths: &[PathBuf]) -> Result<Option<String>, SpeakError> {
    if paths.is_empty() {
        return Ok(None);
    }
    let mut joined = Vec::with_capacity(paths.len());
    for path in paths {
        joined.push(utf8(path)?);
    }
    Ok(Some(joined.join(",")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory shaped like an extracted bundle, without the huge model.
    fn bundle(lexicons: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for name in ["model.onnx", "tokens.txt", "voices.bin"] {
            std::fs::write(dir.path().join(name), b"").unwrap();
        }
        for name in lexicons {
            std::fs::write(dir.path().join(name), b"").unwrap();
        }
        dir
    }

    fn config(dir: &Path) -> KokoroConfig {
        KokoroConfig {
            model_dir: dir.to_path_buf(),
            ..KokoroConfig::default()
        }
    }

    fn resolve(dir: &Path, phonemizer: &Phonemizer) -> Result<ModelFiles, SpeakError> {
        ModelFiles::resolve(&config(dir), phonemizer)
    }

    #[test]
    fn a_missing_directory_is_named_in_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not-downloaded");
        let error = resolve(&missing, &Phonemizer::Lexicon).unwrap_err();
        assert!(matches!(error, SpeakError::ModelDir(ref path) if path == &missing));
        let message = error.to_string();
        assert!(message.contains("releases/tag/tts-models"));
        assert!(message.contains(MODEL_DIR_ENV));
    }

    #[test]
    fn lexicons_are_discovered_sorted_and_comma_joined() {
        let dir = bundle(&[
            "lexicon-zh.txt",
            "lexicon-us-en.txt",
            "lexicon-gb-en.txt",
            "notes.txt",
        ]);
        let files = resolve(dir.path(), &Phonemizer::Lexicon).unwrap();
        assert_eq!(files.lexicon_paths.len(), 3);
        let joined = [
            dir.path().join("lexicon-gb-en.txt"),
            dir.path().join("lexicon-us-en.txt"),
            dir.path().join("lexicon-zh.txt"),
        ]
        .iter()
        .map(|path| path.to_str().unwrap())
        .collect::<Vec<_>>()
        .join(",");
        assert_eq!(files.lexicon.as_deref(), Some(joined.as_str()));
    }

    #[test]
    fn the_lexicon_frontend_requires_a_lexicon() {
        let dir = bundle(&[]);
        let error = resolve(dir.path(), &Phonemizer::Lexicon).unwrap_err();
        assert!(matches!(error, SpeakError::NoLexicon(ref path) if path == dir.path()));
        assert!(error.to_string().contains("speak-phonemize"));
    }

    #[test]
    fn an_explicit_lexicon_must_exist() {
        let dir = bundle(&[]);
        let mut config = config(dir.path());
        config.lexicons = vec![dir.path().join("lexicon-missing.txt")];
        let error = ModelFiles::resolve(&config, &Phonemizer::Lexicon).unwrap_err();
        assert!(matches!(error, SpeakError::MissingFile(_)));
    }

    #[test]
    fn the_int8_checkpoint_is_accepted() {
        let dir = bundle(&["lexicon-us-en.txt"]);
        std::fs::remove_file(dir.path().join("model.onnx")).unwrap();
        std::fs::write(dir.path().join("model.int8.onnx"), b"").unwrap();
        let files = resolve(dir.path(), &Phonemizer::Lexicon).unwrap();
        assert!(files.model.ends_with("model.int8.onnx"));
    }

    #[test]
    fn the_phonemizer_path_needs_no_lexicon() {
        let dir = bundle(&[]);
        assert!(resolve(dir.path(), &Phonemizer::Lexicon).is_err());
        #[cfg(feature = "speak-phonemize")]
        {
            let phonemizer = Phonemizer::Piper {
                lang: "pt-br".into(),
            };
            let files = resolve(dir.path(), &phonemizer).unwrap();
            assert!(files.lexicon.is_none());
        }
    }

    #[test]
    fn the_main_config_defaults_to_kokoro() {
        let config = SpeakConfig::default();
        assert!(matches!(config.backend, super::super::Backend::Kokoro(_)));
    }
}
