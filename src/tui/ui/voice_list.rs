//! Data model of the voice picker (`Settings` → `Voice`).
//!
//! One entry per speaker of the installed Kokoro bundle, carrying the
//! metadata the list renders and searches: the voice name, its language
//! (code and label), gender and the bundle. The picker itself is the shared
//! [`list_dialog`](crate::component::list_dialog) box — the same component
//! `/models` uses — so this module only owns the rows and the filter.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::component::list_dialog::ListRow;

/// One selectable voice in the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceChoice {
    /// Speaker id persisted in `setup.json` → `voice.speaker`.
    pub speaker: i32,
    /// Published voice name (`pf_dora`).
    pub name: String,
    /// espeak-ng language code (`pt-br`) written to `voice.language`.
    pub language: String,
    /// Human-readable language (`Portuguese (Brazil)`), shown in the header.
    pub language_label: String,
    /// `female` or `male`.
    pub gender: String,
    /// Bundle label (`Kokoro v1.0`), shown in the header and searchable.
    pub model: String,
    /// The bundle directory this row belongs to when it is NOT the configured
    /// one; confirming the row switches `voice.model_dir` to it so a
    /// Portuguese voice of the v1.0 bundle can be picked from a list that was
    /// built around the v1.1 default.
    pub model_dir: Option<PathBuf>,
    /// Whether the voice needs an external frontend (every language except
    /// the bundles' English and Chinese lexicons).
    pub needs_phonemize: bool,
}

/// Case-insensitive match against everything the user can see or type: the
/// voice name, its id, the language code/label, the gender and the model.
#[must_use]
pub fn matches(voice: &VoiceChoice, filter: &str) -> bool {
    let needle = filter.to_lowercase();
    if needle.is_empty() {
        return true;
    }
    let speaker = voice.speaker.to_string();
    [
        voice.name.as_str(),
        speaker.as_str(),
        voice.language.as_str(),
        voice.language_label.as_str(),
        voice.gender.as_str(),
        voice.model.as_str(),
    ]
    .iter()
    .any(|value| value.to_lowercase().contains(&needle))
}

/// The voices matching `filter`, in catalog order.
#[must_use]
pub fn filtered<'a>(voices: &'a [VoiceChoice], filter: &str) -> Vec<&'a VoiceChoice> {
    voices
        .iter()
        .filter(|voice| matches(voice, filter))
        .collect()
}

/// Paint rows for the filtered voices: a scope header per
/// `language · model` group, then its voices. Headers occupy visual rows but
/// no selection index.
#[must_use]
pub fn rows(voices: &[&VoiceChoice], current: i32) -> Vec<ListRow> {
    let mut grouped: BTreeMap<(&str, &str), Vec<&VoiceChoice>> = BTreeMap::new();
    for voice in voices {
        grouped
            .entry((voice.language_label.as_str(), voice.model.as_str()))
            .or_default()
            .push(voice);
    }
    let mut rows = Vec::with_capacity(voices.len() + grouped.len());
    for ((language, model), group) in grouped {
        rows.push(ListRow::scope(format!("{language} · {model}")));
        for voice in group {
            // The frontend note explains why some voices need a build with
            // `speak-phonemize`: without it they fall back to a lexicon that
            // does not cover their language.
            let frontend = if voice.needs_phonemize {
                " · espeak-ng"
            } else {
                " · lexicon"
            };
            rows.push(ListRow::item(
                voice.name.clone(),
                format!("  ·  {} · {}{frontend}", voice.speaker, voice.gender),
                voice.speaker == current && voice.model_dir.is_none(),
            ));
        }
    }
    rows
}

/// Visual row of the `selected`-th selectable (non-header) row.
#[must_use]
pub fn visual_index(rows: &[ListRow], selected: usize) -> usize {
    let mut item = 0usize;
    for (index, row) in rows.iter().enumerate() {
        if row.header {
            continue;
        }
        if item == selected {
            return index;
        }
        item += 1;
    }
    0
}

/// The selectable (voice) index painted at `visual`, skipping headers.
#[must_use]
pub fn index_at_visual(rows: &[ListRow], visual: usize) -> Option<usize> {
    let row = rows.get(visual)?;
    if row.header {
        return None;
    }
    Some(rows[..visual].iter().filter(|row| !row.header).count())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice(speaker: i32, name: &str, language: &str, label: &str, gender: &str) -> VoiceChoice {
        VoiceChoice {
            speaker,
            name: name.into(),
            language: language.into(),
            language_label: label.into(),
            gender: gender.into(),
            model: "Kokoro v1.0".into(),
            model_dir: None,
            needs_phonemize: !matches!(language, "en-us" | "en-gb" | "zh"),
        }
    }

    fn catalog() -> Vec<VoiceChoice> {
        vec![
            voice(42, "pf_dora", "pt-br", "Portuguese (Brazil)", "female"),
            voice(43, "pm_alex", "pt-br", "Portuguese (Brazil)", "male"),
            voice(44, "pm_santa", "pt-br", "Portuguese (Brazil)", "male"),
            voice(3, "af_heart", "en-us", "English (US)", "female"),
        ]
    }

    #[test]
    fn search_matches_name_language_model_gender_and_id() {
        let voices = catalog();
        for filter in [
            "dora",
            "pt-br",
            "portuguese",
            "brazil",
            "kokoro",
            "female",
            "43",
        ] {
            assert!(
                !filtered(&voices, filter).is_empty(),
                "filter {filter:?} must match something"
            );
        }
        assert_eq!(filtered(&voices, "dora").len(), 1);
        assert_eq!(filtered(&voices, "pt-br").len(), 3);
        assert_eq!(filtered(&voices, "unknown-voice").len(), 0);
        assert_eq!(filtered(&voices, "").len(), voices.len());
    }

    #[test]
    fn rows_group_by_language_and_model_without_burning_selection_rows() {
        let voices = catalog();
        let matching = filtered(&voices, "");
        let rows = rows(&matching, 42);
        assert_eq!(rows.len(), 6, "4 voices + 2 language headers");
        // Headers sort by language label: English before Portuguese.
        let headers: Vec<&str> = rows
            .iter()
            .filter(|row| row.header)
            .map(|row| row.label.as_str())
            .collect();
        assert_eq!(
            headers,
            [
                "English (US) · Kokoro v1.0",
                "Portuguese (Brazil) · Kokoro v1.0"
            ]
        );
        // EN header, af_heart, PT header, pf_dora, pm_alex, pm_santa.
        assert_eq!(rows[1].label, "af_heart");
        assert!(rows[3].current, "the configured speaker carries ●");

        // Selection indexes voices, not visual rows.
        assert_eq!(visual_index(&rows, 0), 1);
        assert_eq!(visual_index(&rows, 3), 5);
        assert_eq!(index_at_visual(&rows, 1), Some(0));
        assert_eq!(
            index_at_visual(&rows, 0),
            None,
            "headers are not selectable"
        );
        assert_eq!(index_at_visual(&rows, 5), Some(3));
        assert_eq!(index_at_visual(&rows, 2), None);
    }
}
