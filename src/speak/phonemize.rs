//! The external phonemizer used by [`Phonemizer::Piper`].
//!
//! sherpa-onnx 2.x removed espeak-ng, so a Kokoro language without a lexicon
//! has to be phonemized before generation and passed as Unicode codepoints.
//! Spanish, French, Hindi, Italian and Brazilian Portuguese go through
//! `piper-phonemize`, which embeds espeak-ng and its own minimal
//! `espeak-ng-data`. That embedded data has no per-language dictionaries, so
//! [`initialize`] points espeak-ng at the complete `espeak-ng-data` shipped
//! inside the model bundle before the first synthesis.
//!
//! Japanese is different: espeak-ng does not read kanji (it spells them as
//! "chinese letter") and the v1.0 bundle was trained on misaki's *cutlet*
//! phonemes, not on espeak-ng's Japanese. [`japanese`] therefore reads the
//! text with an embedded Japanese dictionary and maps the resulting kana to
//! the cutlet phoneme set, exactly the symbols `tokens.txt` contains.
//!
//! [`Phonemizer::Piper`]: super::config::Phonemizer::Piper

use std::path::Path;

use super::config::SpeakError;

/// Point espeak-ng at a complete `espeak-ng-data` directory.
///
/// `piper-phonemize` ships a minimal data archive without the per-language
/// dictionaries (`es_dict`, `fr_dict`, `hi_dict`, `it_dict`, `ja_dict`, ...),
/// which makes those languages phonemize to nothing. The Kokoro bundles carry
/// the full data next to the model, so the first call wins (espeak-ng
/// initializes once per process) and later calls are no-ops.
pub(crate) fn initialize(data_dir: &Path) -> Result<(), String> {
    let dir = data_dir
        .to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", data_dir.display()))?;
    piper_phonemize::initialize(dir)
        .map(|_| ())
        .map_err(|reason| format!("espeak-ng rejected {}: {reason}", data_dir.display()))
}

/// Phonemize `text` into one codepoint vector per sentence, the shape
/// `GenerationConfig::phoneme_codepoints` expects.
pub(crate) fn codepoints(text: &str, lang: &str) -> Result<Vec<Vec<i32>>, SpeakError> {
    if lang == "ja" {
        return japanese::codepoints(text);
    }
    let result = piper_phonemize::phonemize(text, lang).ok_or_else(|| SpeakError::Phonemize {
        lang: lang.to_string(),
    })?;
    let sentences: Vec<Vec<i32>> = (0..result.num_sentences())
        .map(|index| result.get_phonemes(index).unwrap_or_default())
        .filter(|phonemes| !phonemes.is_empty())
        .collect();
    if sentences.is_empty() {
        return Err(SpeakError::NoAudio);
    }
    Ok(sentences)
}

/// Japanese frontend: kanji/kana to the cutlet phonemes Kokoro v1.0 was
/// trained on.
#[cfg(feature = "speak-phonemize")]
mod japanese {
    use std::sync::OnceLock;

    use super::SpeakError;

    static CONVERTER: OnceLock<Option<sabiyomi::Sabiyomi>> = OnceLock::new();

    pub(super) fn codepoints(text: &str) -> Result<Vec<Vec<i32>>, SpeakError> {
        let converter = CONVERTER
            .get_or_init(|| sabiyomi::Sabiyomi::new().ok())
            .as_ref()
            .ok_or_else(|| SpeakError::Phonemize {
                lang: "ja".to_owned(),
            })?;
        let tokens = converter
            .tokenize(text)
            .map_err(|_| SpeakError::Phonemize {
                lang: "ja".to_owned(),
            })?;
        let mut kana = String::new();
        for token in tokens {
            // `pronunciation` is the spoken katakana (は as ワ), while
            // `reading` keeps the written kana. Punctuation has neither.
            match token.pronunciation.or(token.reading) {
                Some(reading) => kana.push_str(&kata_to_hira(&reading)),
                None => kana.push_str(&token.surface),
            }
        }
        // One entry per sentence, the shape `piper_phonemize` returns.
        let phonemes = kana_to_phonemes(&kana);
        let sentences: Vec<&str> = phonemes
            .split_inclusive(['.', '!', '?'])
            .map(str::trim)
            .filter(|sentence| !sentence.is_empty())
            .collect();
        if sentences.is_empty() {
            return Err(SpeakError::NoAudio);
        }
        Ok(sentences
            .into_iter()
            .map(|sentence| sentence.chars().map(|c| c as i32).collect())
            .collect())
    }

    /// Katakana to hiragana, the script the table is keyed by.
    fn kata_to_hira(text: &str) -> String {
        text.chars()
            .map(|c| match c {
                '\u{30A1}'..='\u{30F6}' => char::from_u32(c as u32 - 0x60).unwrap_or(c),
                '\u{30F7}' => 'わ',
                '\u{30F8}' => 'ゐ',
                '\u{30F9}' => 'ゑ',
                '\u{30FA}' => 'を',
                _ => c,
            })
            .collect()
    }

    /// Map kana (plus punctuation and ASCII) to Kokoro's cutlet phonemes.
    ///
    /// Ported from misaki's `cutlet.py` `HEPBURN` table (MIT) and its
    /// `_get_single_mapping`: the `ん` allophone follows the next phoneme, and
    /// digraphs (きゃ, しょ, ...) map as one unit.
    fn kana_to_phonemes(text: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if let Some(next) = chars.get(i + 1).copied() {
                let pair = format!("{c}{next}");
                if let Some(mapped) = pair_of(&pair) {
                    out.push_str(mapped);
                    i += 2;
                    continue;
                }
            }
            match c {
                'ん' => out.push_str(nasal(chars.get(i + 1).copied())),
                _ => {
                    if let Some(mapped) = single(c) {
                        out.push_str(mapped);
                    } else if c == '\n' || c == '\t' {
                        out.push(' ');
                    } else if c.is_ascii() {
                        out.push(c);
                    }
                }
            }
            i += 1;
        }
        out
    }

    /// The `ん` allophone, chosen from the next kana's phoneme.
    fn nasal(next: Option<char>) -> &'static str {
        let Some(next) = next else {
            return "ɴ";
        };
        let Some(mapped) = single(next) else {
            return "ɴ";
        };
        match mapped.chars().next() {
            Some('m' | 'p' | 'b') => "m",
            Some('k' | 'ɡ') => "ŋ",
            Some('ɲ' | 'ʨ' | 'ʥ') => "ɲ",
            Some('n' | 't' | 'd' | 'ɾ' | 'z') => "n",
            _ => "ɴ",
        }
    }

    fn single(c: char) -> Option<&'static str> {
        Some(match c {
            'ぁ' | 'あ' => "a",
            'ぃ' | 'い' => "i",
            'ぅ' | 'う' => "ɯ",
            'ぇ' | 'え' => "e",
            'ぉ' | 'お' => "o",
            'か' => "ka",
            'が' => "ɡa",
            'き' => "kʲi",
            'ぎ' => "ɡʲi",
            'く' => "kɯ",
            'ぐ' => "ɡɯ",
            'け' => "ke",
            'げ' => "ɡe",
            'こ' => "ko",
            'ご' => "ɡo",
            'さ' => "sa",
            'ざ' => "ʣa",
            'し' => "ɕi",
            'じ' => "ʥi",
            'す' => "sɨ",
            'ず' => "zɨ",
            'せ' => "se",
            'ぜ' => "ʣe",
            'そ' => "so",
            'ぞ' => "ʣo",
            'た' => "ta",
            'だ' => "da",
            'ち' => "ʨi",
            'ぢ' => "ʥi",
            'つ' => "ʦɨ",
            'づ' => "zɨ",
            'て' => "te",
            'で' => "de",
            'と' => "to",
            'ど' => "do",
            'な' => "na",
            'に' => "ɲi",
            'ぬ' => "nɯ",
            'ね' => "ne",
            'の' => "no",
            'は' => "ha",
            'ば' => "ba",
            'ぱ' => "pa",
            'ひ' => "çi",
            'び' => "bʲi",
            'ぴ' => "pʲi",
            'ふ' => "ɸɯ",
            'ぶ' => "bɯ",
            'ぷ' => "pɯ",
            'へ' => "he",
            'べ' => "be",
            'ぺ' => "pe",
            'ほ' => "ho",
            'ぼ' => "bo",
            'ぽ' => "po",
            'ま' => "ma",
            'み' => "mʲi",
            'む' => "mɯ",
            'め' => "me",
            'も' => "mo",
            'ゃ' | 'や' => "ja",
            'ゅ' | 'ゆ' => "jɯ",
            'ょ' | 'よ' => "jo",
            'ら' => "ɾa",
            'り' => "ɾʲi",
            'る' => "ɾɯ",
            'れ' => "ɾe",
            'ろ' => "ɾo",
            'ゎ' | 'わ' => "βa",
            'ゐ' => "i",
            'ゑ' => "e",
            'を' => "o",
            'ゔ' => "vɯ",
            'ゕ' => "ka",
            'ゖ' => "ke",
            'っ' => "ʔ",
            'ー' => "ː",
            // Punctuation, in the cutlet table's normalized forms.
            '。' => ".",
            '、' | '，' => ",",
            '？' => "?",
            '！' => "!",
            '「' | '『' => "“",
            '」' | '』' => "”",
            '：' => ":",
            '；' => ";",
            '（' | '《' => "(",
            '）' | '》' => ")",
            '【' => "[",
            '】' => "]",
            '・' | '　' => " ",
            '～' | '〜' | '—' => "—",
            '«' => "“",
            '»' => "”",
            '゚' | '゙' => "",
            _ => return None,
        })
    }

    fn pair_of(pair: &str) -> Option<&'static str> {
        Some(match pair {
            "いぇ" => "je",
            "うぃ" => "βi",
            "うぇ" => "βe",
            "うぉ" => "βo",
            "きぇ" => "kʲe",
            "きゃ" => "kʲa",
            "きゅ" => "kʲɨ",
            "きょ" => "kʲo",
            "ぎゃ" => "ɡʲa",
            "ぎゅ" => "ɡʲɨ",
            "ぎょ" => "ɡʲo",
            "くぁ" => "kᵝa",
            "くぃ" => "kᵝi",
            "くぇ" => "kᵝe",
            "くぉ" => "kᵝo",
            "ぐぁ" => "ɡᵝa",
            "ぐぃ" => "ɡᵝi",
            "ぐぇ" => "ɡᵝe",
            "ぐぉ" => "ɡᵝo",
            "しぇ" => "ɕe",
            "しゃ" => "ɕa",
            "しゅ" => "ɕɨ",
            "しょ" => "ɕo",
            "じぇ" => "ʥe",
            "じゃ" => "ʥa",
            "じゅ" => "ʥɨ",
            "じょ" => "ʥo",
            "ちぇ" => "ʨe",
            "ちゃ" => "ʨa",
            "ちゅ" => "ʨɨ",
            "ちょ" => "ʨo",
            "ぢゃ" => "ʥa",
            "ぢゅ" => "ʥɨ",
            "ぢょ" => "ʥo",
            "つぁ" => "ʦa",
            "つぃ" => "ʦʲi",
            "つぇ" => "ʦe",
            "つぉ" => "ʦo",
            "てぃ" => "tʲi",
            "てゅ" => "tʲɨ",
            "でぃ" => "dʲi",
            "でゅ" => "dʲɨ",
            "とぅ" => "tɯ",
            "どぅ" => "dɯ",
            "にぇ" => "ɲe",
            "にゃ" => "ɲa",
            "にゅ" => "ɲɨ",
            "にょ" => "ɲo",
            "ひぇ" => "çe",
            "ひゃ" => "ça",
            "ひゅ" => "çɨ",
            "ひょ" => "ço",
            "びゃ" => "bʲa",
            "びゅ" => "bʲɨ",
            "びょ" => "bʲo",
            "ぴゃ" => "pʲa",
            "ぴゅ" => "pʲɨ",
            "ぴょ" => "pʲo",
            "ふぁ" => "ɸa",
            "ふぃ" => "ɸʲi",
            "ふぇ" => "ɸe",
            "ふぉ" => "ɸo",
            "ふゅ" => "ɸʲɨ",
            "ふょ" => "ɸʲo",
            "みゃ" => "mʲa",
            "みゅ" => "mʲɨ",
            "みょ" => "mʲo",
            "りゃ" => "ɾʲa",
            "りゅ" => "ɾʲɨ",
            "りょ" => "ɾʲo",
            "ゔぁ" => "va",
            "ゔぃ" => "vʲi",
            "ゔぇ" => "ve",
            "ゔぉ" => "vo",
            "ゔゅ" => "bʲɨ",
            "ゔょ" => "bʲo",
            _ => return None,
        })
    }
}

#[cfg(all(test, feature = "speak-phonemize"))]
mod tests {
    use super::*;

    fn sentence(codepoints: Vec<i32>) -> String {
        codepoints
            .into_iter()
            .filter_map(|cp| u32::try_from(cp).ok().and_then(char::from_u32))
            .collect()
    }

    fn sentences(text: &str, lang: &str) -> Vec<String> {
        codepoints(text, lang)
            .unwrap_or_else(|error| panic!("{lang}: {error}"))
            .into_iter()
            .map(sentence)
            .collect()
    }

    fn phonemes(text: &str, lang: &str) -> String {
        sentences(text, lang).concat()
    }

    /// Kanji goes through the embedded Japanese dictionary, not espeak-ng.
    #[test]
    fn japanese_reads_kanji_with_cutlet_phonemes() {
        // IPADIC pronunciation: 今日 = キョー, は as the particle = ワ,
        // 天気 = テンキ (ん is ŋ before k).
        let hello = phonemes("今日はいい天気ですね。", "ja");
        assert!(
            hello.starts_with("kʲoːβa"),
            "今日は should be きょー+わ, got {hello:?}"
        );
        assert!(hello.contains("teŋkʲi"), "天気, got {hello:?}");

        // 東京 = トーキョー, へ as the particle = エ, 行こう = イコウ.
        let tokyo = phonemes("東京へ行こう", "ja");
        assert_eq!(tokyo, "toːkʲoːeikoɯ", "へ is the particle e");
    }

    /// A Japanese sentence ends at its own terminator, so the model gets the
    /// same one-sentence-per-entry shape `piper_phonemize` returns.
    #[test]
    fn japanese_splits_one_sentence_per_terminator() {
        let split = sentences("今日はいい天気ですね。明日は雨です！", "ja");
        assert_eq!(split.len(), 2, "two sentences, got {split:?}");
        assert!(split[0].ends_with('.'), "got {split:?}");
        assert!(split[1].ends_with('!'), "got {split:?}");
        assert!(
            split.iter().all(|sentence| !sentence.trim().is_empty()),
            "got {split:?}"
        );
    }

    /// One sample sentence per supported language. The table is checked
    /// against [`crate::speak::voice::LANGUAGES`], so a new voice language
    /// cannot ship without a frontend test.
    const SAMPLES: &[(&str, &str)] = &[
        ("en-us", "Hello, how are you?"),
        ("en-gb", "Hello, how are you?"),
        ("zh", "你好，你好吗？"),
        ("es", "Hola, ¿cómo estás?"),
        ("fr", "Bonjour, comment ça va ?"),
        ("hi", "नमस्ते, आप कैसे हैं?"),
        ("it", "Ciao, come stai?"),
        ("ja", "今日はいい天気ですね。"),
        ("pt-br", "Olá, tudo bem?"),
    ];

    /// Every language the v1.0 bundle ships voices for must produce
    /// non-empty phonemes. The espeak-ng languages need the bundle's complete
    /// data directory and the lexicon languages need the model, so each is
    /// skipped when its data is not installed; CI has no model.
    #[test]
    fn every_supported_language_phonemizes() {
        let mut declared: Vec<&str> = crate::speak::voice::LANGUAGES
            .iter()
            .map(|language| language.language)
            .collect();
        declared.sort_unstable();
        let mut sampled: Vec<&str> = SAMPLES.iter().map(|(lang, _)| *lang).collect();
        sampled.sort_unstable();
        assert_eq!(declared, sampled, "every language needs a sample");

        let bundle = crate::speak::default_model_dir();
        let data = bundle.join("espeak-ng-data");
        let espeak_installed = data.join("phontab").is_file();
        if espeak_installed {
            initialize(&data).expect("the bundle ships complete espeak-ng data");
        } else {
            eprintln!(
                "skipping espeak-ng languages: {} is not installed",
                bundle.display()
            );
        }

        for (lang, text) in SAMPLES {
            let support = crate::speak::voice::LANGUAGES
                .iter()
                .find(|entry| entry.language == *lang)
                .expect("the sample table and LANGUAGES agree");
            if support.lexicon {
                // English and Chinese run on the bundle lexicon inside the
                // loaded model, which neither this build nor CI has.
                continue;
            }
            if *lang != "ja" && !espeak_installed {
                continue;
            }
            let out = phonemes(text, lang);
            let bare: String = out
                .chars()
                .filter(|c| !c.is_whitespace() && !",.?!;:()[]“”".contains(*c))
                .collect();
            assert!(
                !bare.is_empty(),
                "{lang} produced only punctuation: {out:?}"
            );
        }
    }
}
