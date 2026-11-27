//! Shared keyboard engine for the pipeline computer tools.
//!
//! `computer_control` and `computer_act` both chain keyboard steps (tap a
//! key, optionally with modifiers, or type literal text) through the same
//! recursive `then`/`wait` skeleton, so the parsing, validation helpers and
//! the dispatch live HERE, parameterized by the calling tool's error prefix.
//! The keystrokes go into whatever element holds keyboard focus NOW -
//! aiming them is the caller's job (a real click in `computer_control`, or
//! a semantic action in `computer_act`).
use std::sync::{Mutex, OnceLock};

use xa11y::Key;

/// Maximum duration of a `wait` step, in milliseconds - the pipeline stays
/// one tool call, so a wait is a pause for the app to catch up, not a
/// sleep primitive.
pub const MAX_WAIT_MS: u64 = 10_000;

/// `release` value that puts back EVERY key a `hold` left down.
pub const RELEASE_ALL: &str = "all";

/// Keys `hold` pressed and has not released yet, tracked by key IDENTITY (the
/// parsed [`Key`]) rather than by spelling: releasing `ctrl` must clear the
/// `control` a `hold` recorded, and the report must never claim a key is down
/// because an alias was spelled differently.
///
/// The input simulator is process-wide, so a held key outlives the call that
/// set it: reporting it lets a chain that was interrupted be understood (and
/// undone) instead of silently corrupting later input.
fn held_keys() -> &'static Mutex<Vec<Key>> {
    static HELD: OnceLock<Mutex<Vec<Key>>> = OnceLock::new();
    HELD.get_or_init(|| Mutex::new(Vec::new()))
}

/// Test seam: forget what is tracked (the OS keys are NOT touched) so a test
/// starts from a known state.
#[cfg(test)]
pub(crate) fn forget_held() {
    if let Ok(mut held) = held_keys().lock() {
        held.clear();
    }
}

/// Human-facing report of what is still pressed, or `""` when nothing is.
pub fn held_report() -> String {
    let held = held_keys()
        .lock()
        .map(|keys| keys.clone())
        .unwrap_or_default();
    if held.is_empty() {
        return String::new();
    }
    let names: Vec<String> = held.iter().map(key_name).collect();
    format!(
        " - still held: {} (release with {{\"release\": [\"{}\"]}} or \"all\")",
        names.join(", "),
        names.join("\", \"")
    )
}

/// True when `names` is exactly the `all` sentinel.
fn is_release_all(names: &[String]) -> bool {
    names.len() == 1 && names[0].trim().eq_ignore_ascii_case(RELEASE_ALL)
}

/// Validate a `release` step: either the `all` sentinel or ordinary key names.
pub fn validate_release(names: &[String], tool: &str) -> Result<(), String> {
    if is_release_all(names) {
        return Ok(());
    }
    if names
        .iter()
        .any(|n| n.trim().eq_ignore_ascii_case(RELEASE_ALL))
    {
        return Err(format!(
            "{tool}: `release` accepts \"{RELEASE_ALL}\" only on its own - either release \
             every key or name the keys explicitly"
        ));
    }
    parse_hold_keys(names, tool).map(|_| ())
}

/// The keyboard fields of ONE pipeline step, tool-agnostic: both
/// `ComputerControl` and `ComputerAct` narrow into this before handing the
/// step to [`run_keyboard_step`].
#[derive(Debug, Clone, Default)]
pub struct KeyboardStep<'a> {
    /// Key to tap (single lowercase character or a named key).
    pub key: Option<&'a str>,
    /// Literal text to type.
    pub text: Option<&'a str>,
    /// Modifier keys held while tapping `key`.
    pub held: Option<&'a [String]>,
}

/// True when the step's keyboard fields make it a KEYBOARD step. Matches
/// the validators exactly (which trim `key` but take `text` as-is), so
/// validation and execution can never disagree about the step's kind.
pub fn is_keyboard_step(key: Option<&str>, text: Option<&str>) -> bool {
    key.is_some_and(|k| !k.trim().is_empty()) || text.is_some_and(|t| !t.is_empty())
}

/// True when the step's `wait` field makes it a WAIT step.
pub fn is_wait_step(wait_ms: Option<u64>) -> bool {
    wait_ms.is_some()
}

/// Execute ONE validated keyboard step and return its human-facing report.
///
/// `tool` prefixes every error (e.g. `computer_control`) so a shared
/// failure reads as coming from the tool the model actually called.
pub fn run_keyboard_step(
    sim: &xa11y::InputSim,
    step: &KeyboardStep,
    tool: &str,
) -> Result<String, String> {
    if let Some(text) = step.text.filter(|t| !t.is_empty()) {
        sim.keyboard()
            .type_text(text)
            .map_err(|e| super::errors::render(tool, "type text", &e))?;
        return Ok(format!("typed {text:?}"));
    }

    let key = step.key.unwrap_or_default().trim();
    let parsed = parse_key(key, tool)?;
    let held = parse_keys(step.held.unwrap_or_default(), tool)?;
    if held.is_empty() {
        sim.keyboard()
            .press(parsed)
            .map_err(|e| super::errors::render(tool, &format!("press {key}"), &e))?;
        Ok(format!("pressed {key}"))
    } else {
        let names: Vec<String> = held.iter().map(key_name).collect();
        sim.keyboard()
            .chord(parsed, &held)
            .map_err(|e| super::errors::render(tool, &format!("chord {key} + {names:?}"), &e))?;
        Ok(format!("pressed {key} with {} held", names.join(",")))
    }
}

/// Pause for a validated `wait` step and return its report fragment.
pub fn run_wait_step(wait_ms: u64) -> String {
    std::thread::sleep(std::time::Duration::from_millis(wait_ms));
    format!("waited {wait_ms} ms")
}

/// Execute ONE validated `hold` step: press every key DOWN and leave it
/// held. The keys keep their state in the OS keyboard engine across steps
/// and calls - a later `release` with the same names puts them back up.
/// Order is honored: listed first = pressed first.
pub fn run_hold_step(
    sim: &xa11y::InputSim,
    names: &[String],
    tool: &str,
) -> Result<String, String> {
    let keys = parse_hold_keys(names, tool)?;
    for (name, key) in names.iter().zip(keys.iter().cloned()) {
        sim.keyboard()
            .down(key.clone())
            .map_err(|e| super::errors::render(tool, &format!("hold {name}"), &e))?;
        if let Ok(mut held) = held_keys().lock()
            && !held.contains(&key)
        {
            held.push(key);
        }
    }
    Ok(format!("holding {}", names.join(",")))
}

/// Execute ONE validated `release` step: release every key a previous
/// `hold` left down. Releasing a key that is not held is a no-op on every
/// platform, so this is forgiving by construction.
pub fn run_release_step(
    sim: &xa11y::InputSim,
    names: &[String],
    tool: &str,
) -> Result<String, String> {
    // `all`: release everything this process left down, even keys a previous
    // (possibly aborted) call pressed. The tracking list is only updated once
    // each `up` SUCCEEDS, so a failed release keeps the key reported as held
    // instead of turning a stuck key into an invisible one.
    if is_release_all(names) {
        let tracked = held_keys()
            .lock()
            .map(|keys| keys.clone())
            .unwrap_or_default();
        for key in &tracked {
            sim.keyboard().up(key.clone()).map_err(|e| {
                super::errors::render(tool, &format!("release {}", key_name(key)), &e)
            })?;
            if let Ok(mut held) = held_keys().lock() {
                held.retain(|tracked| tracked != key);
            }
        }
        return Ok(if tracked.is_empty() {
            format!("released {RELEASE_ALL} (no key was held)")
        } else {
            let names: Vec<String> = tracked.iter().map(key_name).collect();
            format!("released {}", names.join(","))
        });
    }

    let keys = parse_hold_keys(names, tool)?;
    for (name, key) in names.iter().zip(keys.iter().cloned()) {
        sim.keyboard()
            .up(key.clone())
            .map_err(|e| super::errors::render(tool, &format!("release {name}"), &e))?;
        if let Ok(mut held) = held_keys().lock() {
            held.retain(|tracked| *tracked != key);
        }
    }
    Ok(format!("released {}", names.join(",")))
}

/// Parse a key name into a [`Key`].
///
/// Named keys are matched case-insensitively; a single CHARACTER is taken
/// literally - an uppercase letter is REJECTED (the caller must hold
/// `shift` explicitly), matching the `Key::Char` contract in xa11y.
pub fn parse_key(name: &str, tool: &str) -> Result<Key, String> {
    let lower = name.to_ascii_lowercase();
    let key = match lower.as_str() {
        "enter" | "return" => Key::Enter,
        "escape" | "esc" => Key::Escape,
        "backspace" => Key::Backspace,
        "tab" => Key::Tab,
        "space" => Key::Space,
        "delete" | "del" => Key::Delete,
        "insert" => Key::Insert,
        "up" | "arrowup" => Key::ArrowUp,
        "down" | "arrowdown" => Key::ArrowDown,
        "left" | "arrowleft" => Key::ArrowLeft,
        "right" | "arrowright" => Key::ArrowRight,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        _ if name.chars().count() == 1 => {
            // Validate the ORIGINAL character, not the lowercased one - a
            // silently-lowercased uppercase letter would send the wrong key
            // and report success.
            let ch = name.chars().next().unwrap_or_default();
            if ch.is_uppercase() {
                return Err(format!(
                    "{tool}: uppercase `{name}` - pass the lowercase key with `held: [\"shift\"]`"
                ));
            }
            Key::Char(ch)
        }
        _ if lower.starts_with('f')
            && lower.len() <= 3
            && lower[1..]
                .parse::<u8>()
                .is_ok_and(|n| (1..=12).contains(&n)) =>
        {
            Key::F(lower[1..].parse::<u8>().unwrap_or(1))
        }
        _ => {
            return Err(format!("{tool}: unknown key `{name}`"));
        }
    };
    Ok(key)
}

/// Parse modifier names into [`Key`]s (shared by click/drag `held` and
/// keyboard chords).
pub fn parse_keys(names: &[String], tool: &str) -> Result<Vec<Key>, String> {
    names
        .iter()
        .map(|n| match n.to_ascii_lowercase().as_str() {
            "shift" => Ok(Key::Shift),
            "ctrl" | "control" => Ok(Key::Ctrl),
            "alt" | "option" => Ok(Key::Alt),
            "meta" | "cmd" | "command" | "super" | "win" => Ok(Key::Meta),
            other => Err(format!(
                "{tool}: unknown modifier `{other}` (expected shift, ctrl, alt or meta)"
            )),
        })
        .collect()
}

/// Parse names for `hold`/`release` step arrays: MODIFIER names first
/// (same aliases as `held`), then any ordinary key name - unlike `held`,
/// which takes modifiers only, a `hold` may keep any key down (an app that
/// latches `space` or `enter` while open). Errors name the accepted set:
/// "modifier or key".
pub fn parse_hold_keys(names: &[String], tool: &str) -> Result<Vec<Key>, String> {
    names
        .iter()
        .map(|n| match n.to_ascii_lowercase().as_str() {
            RELEASE_ALL => Err(format!(
                "{tool}: `{RELEASE_ALL}` is a `release`-only value (release every held key)"
            )),
            "shift" => Ok(Key::Shift),
            "ctrl" | "control" => Ok(Key::Ctrl),
            "alt" | "option" => Ok(Key::Alt),
            "meta" | "cmd" | "command" | "super" | "win" => Ok(Key::Meta),
            _ => parse_key(n, tool),
        })
        .collect()
}

/// Human-facing name of a `Key` (for the `sent` report and the held-key
/// advice). Every form is a name [`parse_key`] accepts, so `key_name` can be
/// fed back into a `release` - a Debug rendering would not round-trip.
pub fn key_name(key: &Key) -> String {
    match key {
        Key::Char(c) => c.to_string(),
        Key::Shift => "shift".to_string(),
        Key::Ctrl => "ctrl".to_string(),
        Key::Alt => "alt".to_string(),
        Key::Meta => "meta".to_string(),
        Key::Enter => "enter".to_string(),
        Key::Escape => "escape".to_string(),
        Key::Backspace => "backspace".to_string(),
        Key::Tab => "tab".to_string(),
        Key::Space => "space".to_string(),
        Key::Delete => "delete".to_string(),
        Key::Insert => "insert".to_string(),
        Key::ArrowUp => "up".to_string(),
        Key::ArrowDown => "down".to_string(),
        Key::ArrowLeft => "left".to_string(),
        Key::ArrowRight => "right".to_string(),
        Key::Home => "home".to_string(),
        Key::End => "end".to_string(),
        Key::PageUp => "pageup".to_string(),
        Key::PageDown => "pagedown".to_string(),
        Key::F(n) => format!("f{n}"),
    }
}

/// Handle for the pipeline tools: both share the process-wide input
/// simulator, so a down/up or drag never splits across devices.
pub(crate) fn shared_sim() -> Result<xa11y::InputSim, String> {
    super::shared_input_sim()
}
