//! The one outbound path: presence, pacing and sending.
//!
//! Every reply this interface produces leaves through [`send`], so filtering
//! empty bodies, the optional typing window and send-error logging have a
//! single owner. The caller (`run`) keeps lifecycle and task supervision.
//!
//! Humanization is a pacing effect only. It never changes the text, the
//! routing or the turn result, and it is best effort: a failed presence call
//! skips the wait and the replies are still sent.

use std::time::Duration;

use morpheus_whatsapp::{ChatId, WhatsApp};

use super::typing;

/// What one inbound message produced, and whether to simulate typing for it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplyBatch {
    /// Replies to send, in order. Empty bodies are dropped at delivery time.
    pub replies: Vec<String>,
    /// Show the typing state before sending. False for commands and hints.
    pub humanize: bool,
}

impl ReplyBatch {
    /// A batch produced by a turn, paced according to the chat's setting.
    #[must_use]
    pub fn new(replies: Vec<String>, humanize: bool) -> Self {
        Self { replies, humanize }
    }

    /// A batch that is never paced (commands, routing hints, errors).
    #[must_use]
    pub fn plain(replies: Vec<String>) -> Self {
        Self {
            replies,
            humanize: false,
        }
    }
}

/// How long to show the typing state before `replies`, if at all.
///
/// Pure: no replies or humanization off means no presence and no wait.
#[must_use]
pub fn plan(replies: &[String], humanize: bool) -> Option<Duration> {
    if !humanize {
        return None;
    }
    let has_text = replies.iter().any(|reply| !reply.trim().is_empty());
    has_text.then(|| typing::delay(typing::total_chars(replies)))
}

/// Show presence when asked and send every non-empty reply, in order.
///
/// A failed `composing` call is logged at debug and treated as “no typing”:
/// the replies are sent immediately instead of being delayed or dropped.
/// `paused` is cleanup only and its failure changes nothing.
pub async fn send(link: &WhatsApp, chat: &ChatId, batch: ReplyBatch) {
    let delay = plan(&batch.replies, batch.humanize);
    let mut typing = false;
    if let Some(delay) = delay {
        match link.set_typing(chat, true).await {
            Ok(()) => {
                typing = true;
                tokio::time::sleep(delay).await;
            }
            Err(error) => log::debug!(
                "morpheus whatsapp: could not show the typing state in {chat}: {error}; \
                 sending without the delay"
            ),
        }
    }

    for reply in batch.replies {
        if reply.trim().is_empty() {
            continue;
        }
        log::info!(
            "morpheus whatsapp: sending reply to {} ({} chars)",
            chat,
            reply.len()
        );
        match link.send_text(chat, reply).await {
            Ok(sent) => log::info!(
                "morpheus whatsapp: send_text ok id={} to={}",
                sent.id,
                sent.to
            ),
            Err(error) => log::warn!("morpheus whatsapp: reply to {chat} failed: {error}"),
        }
    }

    if typing && let Err(error) = link.set_typing(chat, false).await {
        log::debug!("morpheus whatsapp: could not clear the typing state in {chat}: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replies(bodies: &[&str]) -> Vec<String> {
        bodies.iter().map(|body| (*body).to_string()).collect()
    }

    #[test]
    fn humanization_off_means_no_presence_and_no_wait() {
        assert_eq!(plan(&replies(&["hello"]), false), None);
        assert_eq!(plan(&[], false), None);
    }

    #[test]
    fn an_empty_batch_is_never_humanized() {
        assert_eq!(plan(&[], true), None);
        assert_eq!(plan(&replies(&["", "   "]), true), None);
    }

    #[test]
    fn one_window_covers_the_whole_batch() {
        let short = plan(&replies(&["hello"]), true).expect("a delay");
        let long = plan(&replies(&["hello", &"x".repeat(200)]), true).expect("a delay");
        assert!(long >= short, "the batch delay counts every reply");
        assert!(
            long <= Duration::from_millis(typing::MAX_MS),
            "the cap applies to the batch"
        );
    }

    #[test]
    fn the_delay_is_bounded_and_deterministic() {
        let batch = vec!["a".repeat(60)];
        let first = plan(&batch, true).expect("a delay");
        assert_eq!(plan(&batch, true), Some(first));
        assert!(first >= Duration::from_millis(typing::MIN_MS));
        assert!(first <= Duration::from_millis(typing::MAX_MS));
    }

    #[test]
    fn plain_batches_are_never_paced() {
        let batch = ReplyBatch::plain(replies(&["/help output"]));
        assert!(!batch.humanize);
        assert_eq!(batch.replies, replies(&["/help output"]));
    }
}
