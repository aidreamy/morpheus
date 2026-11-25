//! The simulated typing window.
//!
//! Humanized replies show WhatsApp's “typing…” state for a short interval
//! before sending. The interval is derived only from the reply text, so it is
//! deterministic and testable: no randomness, no floating point, no clock.
//!
//! The window is clamped to at most [`MAX_MS`], which stays below the
//! server-side lifetime of a chat state (~3 seconds). That is why one
//! `composing` stanza is enough: it expires on its own, and the following
//! `paused` is cleanup rather than a requirement.

use std::time::Duration;

/// Fixed cost, in milliseconds, of any non-empty reply.
pub const BASE_MS: u64 = 500;

/// Cost added per reply character, in milliseconds.
pub const PER_CHAR_MS: u64 = 20;

/// Lower bound of the simulated window.
pub const MIN_MS: u64 = 1_000;

/// Upper bound of the simulated window, below the chat-state lifetime.
pub const MAX_MS: u64 = 2_500;

/// Characters across the non-empty replies of one batch.
#[must_use]
pub fn total_chars(replies: &[String]) -> usize {
    replies
        .iter()
        .filter(|reply| !reply.trim().is_empty())
        .map(|reply| reply.chars().count())
        .sum()
}

/// How long to show the typing state for `chars` characters.
///
/// [`MIN_MS`]–[`MAX_MS`], computed in integer milliseconds, so the same input
/// always produces the same duration.
#[must_use]
pub fn delay(chars: usize) -> Duration {
    let chars = u64::try_from(chars).unwrap_or(u64::MAX);
    let millis = BASE_MS
        .saturating_add(PER_CHAR_MS.saturating_mul(chars))
        .clamp(MIN_MS, MAX_MS);
    Duration::from_millis(millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_replies_get_the_floor() {
        assert_eq!(delay(0), Duration::from_millis(MIN_MS));
        assert_eq!(delay(1), Duration::from_millis(MIN_MS));
        // 500 + 20 * 25 = 1000: exactly the floor.
        assert_eq!(delay(25), Duration::from_millis(MIN_MS));
    }

    #[test]
    fn the_delay_grows_with_the_text() {
        assert_eq!(delay(26), Duration::from_millis(1_020));
        assert_eq!(delay(50), Duration::from_millis(1_500));
        assert_eq!(delay(100), Duration::from_millis(MAX_MS));
    }

    #[test]
    fn the_window_is_capped() {
        assert_eq!(delay(10_000), Duration::from_millis(MAX_MS));
        assert_eq!(delay(usize::MAX), Duration::from_millis(MAX_MS));
    }

    #[test]
    fn every_duration_stays_inside_the_bounds() {
        for chars in [0, 1, 2, 10, 25, 99, 100, 1_000, 100_000] {
            let millis = delay(chars).as_millis() as u64;
            assert!(
                (MIN_MS..=MAX_MS).contains(&millis),
                "{chars} chars produced {millis} ms"
            );
        }
    }

    #[test]
    fn only_non_empty_replies_count() {
        assert_eq!(
            total_chars(&["abc".into(), "  ".into(), "".into(), "de".into()]),
            5
        );
        assert_eq!(total_chars(&[]), 0);
    }

    #[test]
    fn characters_not_bytes_are_counted() {
        assert_eq!(total_chars(&["éé".into()]), 2);
        assert_eq!(total_chars(&["👍".into()]), 1);
    }
}
