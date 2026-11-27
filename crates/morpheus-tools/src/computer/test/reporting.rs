//! Tests for the post-action report: the ambiguous-selector note, the
//! `after:` target state and the tolerant application-name matching. These are
//! the pieces the report leans on, so they are pinned against the mock tree
//! instead of being asserted by inspection.
#[cfg(test)]
mod ambiguity_tests {
    use std::sync::Arc;

    use xa11y::mock::build_provider;
    use xa11y::{Locator, Provider};

    use crate::computer::ambiguity_note;

    /// The fixture's app root, as a locator scoped to the whole application.
    fn app_locator(selector: &str) -> Locator {
        let provider: Arc<dyn Provider> = build_provider();
        let root = provider.list_apps().expect("mock exposes an app root")[0].clone();
        Locator::new(provider, Some(root), selector)
    }

    /// `button` matches two fixture buttons: acting on the first without
    /// `nth` must be stated, with the count, so the caller can retarget.
    #[test]
    fn multiple_matches_without_nth_are_reported() {
        let locator = app_locator("button");
        let note = ambiguity_note(&locator, None);
        assert!(note.contains("matched 2 elements"), "note: {note}");
        assert!(note.contains("`nth`"), "the note must name the fix: {note}");
    }

    /// An explicit `nth` is the caller's decision - no note.
    #[test]
    fn explicit_nth_stays_silent() {
        let locator = app_locator("button");
        assert_eq!(ambiguity_note(&locator, Some(2)), "");
    }

    /// A selector with a single match has nothing to warn about.
    #[test]
    fn unique_match_stays_silent() {
        let locator = app_locator("button[name='Back']");
        assert_eq!(ambiguity_note(&locator, None), "");
    }
}

#[cfg(test)]
mod target_state_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use xa11y::mock::build_provider;
    use xa11y::{Error, Locator, Provider};

    use crate::computer::verify::{render_target_state, state_line};

    const SELECTOR: &str = "button[name='Back']";

    fn element(selector: &str) -> xa11y::Element {
        let provider: Arc<dyn Provider> = build_provider();
        let root = provider.list_apps().expect("mock exposes an app root")[0].clone();
        Locator::new(provider, Some(root), selector)
            .element()
            .expect("fixture element resolves")
    }

    /// The `after:` line reads in the snapshot's own token vocabulary, so the
    /// model can compare it with what it saw before acting.
    #[test]
    fn fresh_state_uses_snapshot_tokens() {
        let line = render_target_state(SELECTOR, Ok(Some(element(SELECTOR))));
        assert_eq!(line, format!("`{SELECTOR}` now visible"));
    }

    /// Disabled is the state that explains a swallowed click - it must survive
    /// into the report.
    #[test]
    fn disabled_target_is_named() {
        let selector = "button[name='Forward']";
        let line = render_target_state(selector, Ok(Some(element(selector))));
        assert_eq!(line, format!("`{selector}` now visible disabled"));
    }

    /// A control that really disappeared (a closed dialog, a replaced row) is
    /// reported as gone - the wait only says this after the whole window.
    #[test]
    fn missing_target_is_reported_as_gone() {
        let provider: Arc<dyn Provider> = build_provider();
        let root = provider.list_apps().expect("mock exposes an app root")[0].clone();
        let waited = Locator::new(provider, Some(root), "button[name='Nope']")
            .wait_until(|element| element.is_some(), Duration::from_millis(10));
        assert!(waited.is_err(), "a miss must not resolve");

        let line = render_target_state("button[name='Nope']", waited);
        assert_eq!(line, "`button[name='Nope']` is gone");
    }

    /// A provider failure is NOT evidence that the target is gone: the report
    /// says it could not observe, never that the element was removed.
    #[test]
    fn provider_failure_is_not_reported_as_gone() {
        let failed = Err(Error::Platform {
            code: -1,
            message: "provider unavailable".into(),
        });
        let line = render_target_state(SELECTOR, failed);
        assert_eq!(line, format!("`{SELECTOR}` state unknown"));
    }

    /// The state line never invents a value: an empty value is omitted.
    #[test]
    fn empty_value_is_omitted() {
        let element = element(SELECTOR);
        let mut data = element.data().clone();
        data.value = Some(String::new());
        assert!(!state_line(SELECTOR, &data).contains("value="));
    }
}

#[cfg(test)]
mod fuzzy_name_tests {
    use crate::computer::unique_fuzzy_name;

    /// The everyday near-miss resolves: a unique case-insensitive substring
    /// (`chrome` -> `Google Chrome`) is substituted.
    #[test]
    fn unique_substring_resolves_and_keeps_the_real_name() {
        let apps = ["Firefox", "Google Chrome"];
        assert_eq!(
            unique_fuzzy_name("chrome", apps),
            Some("Google Chrome"),
            "the resolved name must be the real one, not the caller's spelling"
        );
        assert_eq!(
            unique_fuzzy_name("  GOOGLE CHROME  ", apps),
            Some("Google Chrome")
        );
    }

    /// Two candidates: acting on either would be a guess, so nothing resolves.
    #[test]
    fn multiple_candidates_resolve_to_nothing() {
        let apps = ["Google Chrome", "Chrome Beta"];
        assert_eq!(unique_fuzzy_name("chrome", apps), None);
    }

    /// An unnamed application must never become the unique match for every
    /// miss (a blank string matches any needle as a substring).
    #[test]
    fn blank_candidates_never_match() {
        // A single blank candidate is the dangerous shape: without the
        // non-empty guard it is the UNIQUE match for any miss.
        assert_eq!(unique_fuzzy_name("chrome", ["", "Firefox"]), None);
        assert_eq!(unique_fuzzy_name("chrome", ["   ", "Firefox"]), None);
    }

    /// An empty needle is not a name - the exact lookup is the only path.
    #[test]
    fn blank_needle_resolves_to_nothing() {
        assert_eq!(unique_fuzzy_name("  ", ["Firefox"]), None);
    }
}
