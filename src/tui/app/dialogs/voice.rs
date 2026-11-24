//! Settings → Voice: the searchable voice picker.
//!
//! The picker lists the installed Kokoro bundle's published voices (name,
//! language, gender and model) through the SAME shared list component as
//! `/models`. Picking one applies the speaker id AND the espeak-ng language
//! together, so the user never has to know an id or a language code.

use std::path::PathBuf;

use crossterm::event::KeyCode;

use crate::ui::dialogs::DialogType;
use crate::ui::voice_list::{self, VoiceChoice};

use super::super::App;

impl App {
    /// Open the voice picker for the configured bundle, with the current
    /// speaker preselected so an immediate Enter is a no-op instead of a
    /// silent switch to the first catalog row.
    pub(in crate::app) fn open_voice_dialog(&mut self) {
        let voices = voice_choices(self.setup.voice.model_dir.trim());
        let current = self.setup.voice.speaker;
        // Rows of the configured bundle have `model_dir: None`; preferring
        // them keeps the preselection stable when the same id also exists in
        // another installed bundle.
        let selected = voices
            .iter()
            .position(|voice| voice.speaker == current && voice.model_dir.is_none())
            .or_else(|| voices.iter().position(|voice| voice.speaker == current))
            .unwrap_or(0);
        self.dialog.show(DialogType::VoiceList {
            voices,
            current,
            filter: crate::component::list_dialog::FilterState {
                selected,
                ..Default::default()
            },
        });
    }

    pub(in crate::app) fn is_voice_dialog_visible(&self) -> bool {
        self.dialog
            .current()
            .is_some_and(|d| matches!(d.dialog_type, DialogType::VoiceList { .. }))
    }

    /// Keyboard handling while the VoiceList is on top: Up/Down walk the
    /// FILTERED voices (wrapping), Enter applies the highlighted one, Esc
    /// closes, and typing/Backspace edits the shared Search filter. The
    /// selection is a voice index, so headers can never be confirmed.
    pub(in crate::app) fn handle_voice_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_voice_dialog_visible() {
            return false;
        }
        match key {
            KeyCode::Up => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::VoiceList { voices, filter, .. } = &mut d.dialog_type
                {
                    let len = voice_list::filtered(voices, &filter.filter).len();
                    filter.select_prev(len);
                }
                true
            }
            KeyCode::Down => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::VoiceList { voices, filter, .. } = &mut d.dialog_type
                {
                    let len = voice_list::filtered(voices, &filter.filter).len();
                    filter.select_next(len);
                }
                true
            }
            KeyCode::Enter => {
                self.confirm_voice_selection();
                true
            }
            KeyCode::Esc => {
                self.dialog.pop();
                true
            }
            KeyCode::Backspace => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::VoiceList { filter, .. } = &mut d.dialog_type
                {
                    filter.pop_char();
                }
                true
            }
            KeyCode::Char(ch) => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::VoiceList { filter, .. } = &mut d.dialog_type
                {
                    filter.push_char(ch);
                }
                true
            }
            _ => false,
        }
    }

    /// Apply the highlighted voice: speaker + language from the SAME row,
    /// persisted together so the daemon (which reads setup.json at startup)
    /// is restarted with a consistent pair.
    fn confirm_voice_selection(&mut self) {
        let selected = self.dialog.current().and_then(|d| match &d.dialog_type {
            DialogType::VoiceList { voices, filter, .. } => {
                let matching = voice_list::filtered(voices, &filter.filter);
                matching
                    .get(filter.clamped(matching.len()))
                    .map(|voice| (*voice).clone())
            }
            _ => None,
        });
        self.dialog.pop();
        let Some(voice) = selected else {
            return;
        };
        self.setup.voice.speaker = voice.speaker;
        // English and Chinese have a lexicon inside the bundle, which is the
        // frontend those voices were built around; an empty language selects
        // it. Only the externally phonemized languages write their code.
        self.setup.voice.language = if voice.needs_phonemize {
            voice.language.clone()
        } else {
            String::new()
        };
        if let Some(dir) = &voice.model_dir {
            self.setup.voice.model_dir = dir.to_string_lossy().into_owned();
        }
        self.setup.save();
        self.restart_tts_daemon();
        // The language comes from a catalog entry, but a build without the
        // external frontend cannot honor it. Say so instead of letting the
        // first synthesis fail to the lexicon without an explanation.
        let (message, variant) = if voice.needs_phonemize
            && !cfg!(feature = "speak-phonemize")
        {
            (
                format!(
                    "{} · {} · {} — this build has no external phonemizer; rebuild with \
                     `--features speak-phonemize` to hear it in this language",
                    voice.name, voice.language_label, voice.model
                ),
                crate::ui::toast::ToastVariant::Warning,
            )
        } else {
            (
                format!(
                    "{} · {} · {}",
                    voice.name, voice.language_label, voice.model
                ),
                crate::ui::toast::ToastVariant::Info,
            )
        };
        self.toast_state.show(crate::ui::toast::ToastOptions {
            title: Some("Voice".into()),
            message,
            variant,
            duration_ms: 4000,
        });
    }
}

/// The catalog entries for the configured bundle (empty = the `speak`
/// default) PLUS every other complete Kokoro bundle installed beside it.
///
/// Listing the neighbours is what keeps the picker balanced across languages:
/// the recommended v1.1 bundle only has Chinese and English voices, so
/// Portuguese, Spanish, French, Hindi, Italian and Japanese live exclusively
/// in the v1.0 bundle. A row from another bundle carries that bundle's
/// directory, and confirming it switches `voice.model_dir` — the user never
/// has to edit `setup.json` to reach those voices. Bundles reached through two
/// directories (the configured path and the discovered one) are listed once,
/// and the configured path wins. A configured bundle that no longer exists is
/// dropped when another complete bundle is installed, because the engine would
/// fall back to that other bundle anyway.
///
/// Kept free-standing so the Settings value renderer and tests build the exact
/// same rows the dialog does.
pub(in crate::app) fn voice_choices(model_dir: &str) -> Vec<VoiceChoice> {
    let configured = model_dir_path(model_dir);
    let installed = configured
        .parent()
        .map(morpheus::speak::installed_bundles)
        .unwrap_or_default();
    // A configured bundle that is not on disk (deleted after setup) only
    // yields dead rows whose ids may not exist in the bundle the engine will
    // actually load; list the real bundles instead. With nothing installed the
    // configured rows stay, so the picker still shows what to download.
    let mut dirs = Vec::new();
    if configured.is_dir() || installed.is_empty() {
        dirs.push(configured.clone());
    }
    for found in installed {
        let duplicate = dirs.iter().any(|dir| bundle_of(dir) == bundle_of(&found));
        if !duplicate {
            dirs.push(found);
        }
    }
    let mut choices = Vec::new();
    for dir in dirs {
        let is_configured = dir == configured;
        for voice in morpheus::speak::voice_entries(&dir) {
            choices.push(VoiceChoice {
                speaker: voice.speaker,
                name: voice.name.to_string(),
                language: voice.language.to_string(),
                language_label: voice.language_label.to_string(),
                gender: voice.gender.to_string(),
                model: voice.bundle_label.to_string(),
                model_dir: (!is_configured).then(|| dir.clone()),
                needs_phonemize: voice.needs_phonemize(),
            });
        }
    }
    choices
}

/// The published bundle id a directory resolves to (`kokoro-multi-lang-v1_0`),
/// used to de-duplicate the configured and discovered paths of one bundle.
fn bundle_of(dir: &std::path::Path) -> Option<&'static str> {
    morpheus::speak::voice_entries(dir)
        .first()
        .map(|voice| voice.bundle)
}

/// The bundle directory the picker reads: the configured one, or the
/// `speak` default when setup leaves it blank.
pub(in crate::app) fn model_dir_path(model_dir: &str) -> PathBuf {
    if model_dir.trim().is_empty() {
        morpheus::speak::default_model_dir()
    } else {
        PathBuf::from(model_dir.trim())
    }
}
