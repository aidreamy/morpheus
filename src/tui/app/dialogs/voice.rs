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
        let selected = voices
            .iter()
            .position(|voice| voice.speaker == current)
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
        self.setup.voice.language = voice.language;
        self.setup.save();
        self.restart_tts_daemon();
        self.toast_state.show(crate::ui::toast::ToastOptions {
            title: Some("Voice".into()),
            message: format!(
                "{} · {} · {}",
                voice.name, voice.language_label, voice.model
            ),
            variant: crate::ui::toast::ToastVariant::Info,
            duration_ms: 4000,
        });
    }
}

/// The catalog entries for `model_dir` (empty = the `speak` default), mapped
/// to the picker's row model. Kept free-standing so the Settings value
/// renderer and tests build the exact same rows the dialog does.
pub(in crate::app) fn voice_choices(model_dir: &str) -> Vec<VoiceChoice> {
    let dir = model_dir_path(model_dir);
    morpheus::speak::voice_entries(&dir)
        .into_iter()
        .map(|voice| VoiceChoice {
            speaker: voice.speaker,
            name: voice.name.to_string(),
            language: voice.language.to_string(),
            language_label: voice.language_label.to_string(),
            gender: voice.gender.to_string(),
            model: voice.bundle_label.to_string(),
        })
        .collect()
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
