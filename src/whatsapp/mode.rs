//! Harness mode spellings used by the WhatsApp interface.
//!
//! The TUI cycles modes with Tab; over WhatsApp the user names the mode
//! explicitly (`/mode build`). Every spelling is case-insensitive and the
//! table below is the single source for parsing, validation and the help
//! text, so a new harness mode is added in one place.

use crate::harness::Mode;

/// Every mode exposed over WhatsApp, in help order, with a one-line
/// description of what selecting it means for a remote chat.
pub const ALL: &[(&str, &str)] = &[
    (
        "build",
        "Build - normal agent. Tools that need interactive approval are denied.",
    ),
    (
        "ask",
        "Ask - read-only answers, no file writes or shell commands.",
    ),
    (
        "yolo",
        "Yolo - full access: writes and shell commands run without approval.",
    ),
    (
        "command",
        "Command - every message is run as a shell command; no LLM call.",
    ),
    (
        "manager",
        "Manager - plans and delegates; direct file writes are blocked.",
    ),
];

/// Parse a mode name (`build`, `Ask`, `YOLO`, …). Unknown input → `None`.
#[must_use]
pub fn parse(word: &str) -> Option<Mode> {
    match word.trim().to_ascii_lowercase().as_str() {
        "build" => Some(Mode::Build),
        "ask" => Some(Mode::Ask),
        "yolo" => Some(Mode::Yolo),
        "command" => Some(Mode::Command),
        "manager" => Some(Mode::Manager),
        _ => None,
    }
}

/// The canonical, lowercase name of `mode` (the spelling `/mode` accepts
/// and the help text lists).
#[must_use]
pub fn name(mode: Mode) -> &'static str {
    match mode {
        Mode::Build => "build",
        Mode::Ask => "ask",
        Mode::Yolo => "yolo",
        Mode::Command => "command",
        Mode::Manager => "manager",
    }
}

/// The one-line description shown by `/mode` and `/help`.
#[must_use]
pub fn description(mode: Mode) -> &'static str {
    ALL.iter()
        .find(|(name, _)| *name == self::name(mode))
        .map_or("", |(_, description)| *description)
}

/// The comma-separated list of valid spellings, for usage errors.
#[must_use]
pub fn options() -> String {
    ALL.iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing_is_case_insensitive() {
        for spelling in ["build", "BUILD", "Build", "  bUiLd  "] {
            assert_eq!(parse(spelling), Some(Mode::Build), "{spelling:?}");
        }
        for (word, mode) in [
            ("ask", Mode::Ask),
            ("yolo", Mode::Yolo),
            ("command", Mode::Command),
            ("manager", Mode::Manager),
        ] {
            assert_eq!(parse(word), Some(mode), "{word}");
            assert_eq!(parse(&word.to_uppercase()), Some(mode), "{word}");
        }
    }

    #[test]
    fn unknown_modes_are_rejected() {
        for word in ["", "buil", "build extra", "auto", "turbo"] {
            assert_eq!(parse(word), None, "{word:?}");
        }
    }

    #[test]
    fn names_round_trip_through_parse() {
        for (name, _) in ALL {
            let mode = parse(name).expect("every table entry parses");
            assert_eq!(self::name(mode), *name);
            assert!(!description(mode).is_empty());
        }
        assert!(options().contains("manager"));
    }
}
