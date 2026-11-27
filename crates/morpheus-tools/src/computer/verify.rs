//! Post-action feedback component - what the UI looks like AFTER a step ran.
//!
//! A step report says what was sent; this component says whether anything
//! happened: the target's fresh state (or that it is gone), the application
//! now in the foreground and the element holding keyboard focus. That is the
//! information a human gets for free by looking at the screen, and without it
//! every chain costs an extra `computer_snapshot` and a click swallowed by an
//! overlay reads as success.
//!
//! Feedback is best-effort by construction: every provider call here is
//! allowed to fail and simply contributes nothing, because a successful
//! action must never be reported as an error by its own observation.
use std::time::Duration;

use xa11y::{App, AppExt};

use super::types::SurfaceKind;

/// Observation window for the post-action re-reads. Short on purpose: the
/// step already happened, so this only asks "what is there now".
const LOOKUP_TIMEOUT: Duration = Duration::from_millis(500);

/// The foreground query is the one re-read that runs on EVERY step, so it
/// gets a single attempt instead of a poll - "where is focus now" has no
/// meaningful wait.
const FOREGROUND_TIMEOUT: Duration = Duration::ZERO;

/// The root a step acted on, re-used to re-read its target afterwards.
pub(crate) struct Target<'a> {
    /// Application name, when the step targeted an app.
    pub name: Option<&'a str>,
    /// Application pid, when the step targeted an app.
    pub pid: Option<u32>,
    /// Shell surface, when the step targeted one.
    pub surface: Option<SurfaceKind>,
    /// Element selector the step acted on, if any.
    pub selector: Option<&'a str>,
    /// Match index the step used.
    pub nth: Option<usize>,
}

/// Render the post-action block for one step: `""` when nothing could be
/// observed, otherwise a newline-prefixed `after: …` fragment.
pub(crate) fn after(target: &Target<'_>) -> String {
    let mut parts = Vec::new();
    if let Some(selector) = target.selector {
        parts.push(target_state(target, selector));
    }
    if let Some(foreground) = foreground() {
        parts.push(foreground);
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("\n  after: {}", parts.join("; "))
    }
}

/// Fresh state of the element the step acted on.
fn target_state(target: &Target<'_>, selector: &str) -> String {
    let Some(locator) = locator(target, selector) else {
        return format!("`{selector}` state unknown");
    };
    // `element()` fails fast, so a widget the action just re-created would
    // read as "gone". The short window is what makes the answer describe the
    // SETTLED UI instead of one frame of it.
    render_target_state(
        selector,
        locator.wait_until(|element| element.is_some(), LOOKUP_TIMEOUT),
    )
}

/// Render the target's fresh state from its wait result.
pub(super) fn render_target_state(
    selector: &str,
    waited: xa11y::Result<Option<xa11y::Element>>,
) -> String {
    match waited {
        Ok(Some(element)) => state_line(selector, element.data()),
        // A miss is meaningful: the control stopped existing (a dialog closed,
        // a row was replaced) - exactly what a trained eye would notice. The
        // wait only returns this once the target stayed missing for the whole
        // observation window.
        Ok(None) | Err(xa11y::Error::Timeout { .. }) => format!("`{selector}` is gone"),
        Err(_) => format!("`{selector}` state unknown"),
    }
}

/// One-line state summary of an element, in the same token vocabulary as the
/// snapshot outline.
pub(super) fn state_line(selector: &str, data: &xa11y::ElementData) -> String {
    let mut flags = vec![if data.states.visible {
        "visible"
    } else {
        "hidden"
    }];
    if !data.states.enabled {
        flags.push("disabled");
    }
    if data.states.focused {
        flags.push("focused");
    }
    let mut state = flags.join(" ");
    if let Some(value) = data.value.as_deref().filter(|v| !v.is_empty()) {
        state.push_str(&format!(" value={value:?}"));
    }
    format!("`{selector}` now {state}")
}

/// Foreground application and the element inside it holding keyboard focus -
/// where the next keystroke will land.
fn foreground() -> Option<String> {
    let app = App::foreground(FOREGROUND_TIMEOUT).ok()?;
    let app_name = app.name.clone();
    Some(match super::apps::find_focused_below(&app.as_element()) {
        Some((_, leaf)) => {
            let mut focus = leaf.role.to_snake_case().to_string();
            if let Some(name) = leaf.name.as_deref().filter(|n| !n.is_empty()) {
                focus.push_str(&format!(" {name:?}"));
            }
            format!("foreground {app_name:?}, focus {focus}")
        }
        None => format!("foreground {app_name:?}"),
    })
}

/// Rebuild the step's locator from the same root and selector it acted on.
fn locator(target: &Target<'_>, selector: &str) -> Option<xa11y::Locator> {
    let nth = target.nth.unwrap_or(1);
    if let Some(kind) = target.surface {
        return super::surface::resolve(kind, LOOKUP_TIMEOUT)
            .ok()
            .map(|surface| surface.locator(selector).nth(nth));
    }
    let app = super::resolve_app(target.name, target.pid, LOOKUP_TIMEOUT, "computer").ok()?;
    Some(app.locator(selector).nth(nth))
}
