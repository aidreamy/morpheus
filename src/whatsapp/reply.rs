//! Outbound reply formatting.
//!
//! Every reply that leaves this interface passes through one budget so a
//! runaway model answer or shell output cannot flood the phone. External
//! agents also get a bold first line naming them (and the instance, when there
//! are several); the label sits outside the budget, so truncation never hides
//! who answered. The main agent's replies stay unlabeled.

/// Upper bound on one outbound body, in characters (not bytes: an emoji must
/// count once). A longer body is cut and suffixed with its original size.
pub const MAX_REPLY_CHARS: usize = 16_000;

/// Keep a reply under [`MAX_REPLY_CHARS`], saying what was cut.
#[must_use]
pub fn truncate(text: String) -> String {
    let total = text.chars().count();
    if total <= MAX_REPLY_CHARS {
        return text;
    }
    let kept: String = text.chars().take(MAX_REPLY_CHARS).collect();
    format!("{kept}\n\n… truncated: {total} characters total.")
}

/// A successful external reply: `*claude*` or `*claude #2*`, then a blank
/// line, then the (bounded) body.
#[must_use]
pub fn format_agent_reply(
    agent: &str,
    index: usize,
    total: usize,
    body: impl Into<String>,
) -> String {
    format!(
        "*{}*\n\n{}",
        super::selection::instance_label(agent, index, total),
        truncate(body.into())
    )
}

/// A failed external turn keeps the identity: `*claude*` then `⚠️ reason`.
#[must_use]
pub fn format_agent_error(agent: &str, index: usize, total: usize, reason: &str) -> String {
    format!(
        "*{}*\n\n⚠️ {}",
        super::selection::instance_label(agent, index, total),
        truncate(reason.to_string())
    )
}

/// The note appended when permission requests were denied: it names how many
/// tools were denied and which ones, so the user knows what `/mode yolo`
/// would allow.
#[must_use]
pub fn denial_note(tools: &[String]) -> Option<String> {
    let count = tools.len();
    let (noun, verb, pronoun) = match count {
        0 => return None,
        1 => ("tool", "was", "it"),
        _ => ("tools", "were", "them"),
    };
    Some(format!(
        "\n\n_{count} {noun} requiring approval {verb} denied (no approval dialog on WhatsApp): \
         {}. Use `/mode yolo` if you want {pronoun} to run._",
        tools.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_replies_are_unchanged() {
        assert_eq!(truncate("short".to_string()), "short");
        assert_eq!(
            truncate("x".repeat(MAX_REPLY_CHARS)),
            "x".repeat(MAX_REPLY_CHARS)
        );
    }

    #[test]
    fn truncation_is_bounded_and_explains_itself() {
        let long = "x".repeat(MAX_REPLY_CHARS + 10);
        let cut = truncate(long);
        assert!(cut.chars().count() <= MAX_REPLY_CHARS + 60);
        assert!(
            cut.contains("… truncated: 16010 characters total."),
            "{cut}"
        );
    }

    #[test]
    fn external_replies_are_labeled_and_unlabeled_means_main() {
        assert_eq!(format_agent_reply("claude", 1, 1, "hi"), "*claude*\n\nhi");
        assert_eq!(
            format_agent_reply("opencode", 2, 2, "hi"),
            "*OpenCode #2*\n\nhi"
        );
    }

    #[test]
    fn errors_keep_the_identity() {
        assert_eq!(
            format_agent_error("claude", 1, 1, "not installed"),
            "*claude*\n\n⚠️ not installed"
        );
    }

    #[test]
    fn the_label_is_outside_the_budget() {
        let reply = format_agent_reply("claude", 1, 1, "x".repeat(MAX_REPLY_CHARS + 5));
        assert!(reply.starts_with("*claude*\n\n"), "label must survive");
        assert!(reply.contains("truncated"));
    }

    #[test]
    fn the_denial_note_names_the_count_and_the_tools() {
        assert_eq!(denial_note(&[]), None);

        let one = denial_note(&["bash_run".to_string()]).expect("a note");
        assert!(one.contains("1 tool"), "{one}");
        assert!(one.contains("was denied"), "{one}");
        assert!(one.contains("bash_run"), "{one}");
        assert!(one.contains("yolo"), "{one}");

        let two = denial_note(&["bash_run".to_string(), "edit_file".to_string()]).expect("a note");
        assert!(two.contains("2 tools"), "{two}");
        assert!(two.contains("were denied"), "{two}");
        assert!(two.contains("bash_run, edit_file"), "{two}");
    }

    #[test]
    fn instance_labels_are_only_added_when_there_is_more_than_one() {
        assert_eq!(
            format_agent_reply("claude", 1, 2, "hi"),
            "*claude #1*\n\nhi"
        );
        assert_eq!(
            format_agent_reply("claude", 2, 2, "hi"),
            "*claude #2*\n\nhi"
        );
        assert_eq!(format_agent_reply("maria", 1, 1, "hi"), "*maria*\n\nhi");
    }

    #[test]
    fn the_budget_counts_characters_not_bytes() {
        let long = "é".repeat(MAX_REPLY_CHARS + 1);
        let cut = truncate(long);
        assert!(cut.contains("16001 characters total"), "{cut}");
        // The kept prefix is exactly the budget, counted in chars.
        assert_eq!(cut.chars().take(MAX_REPLY_CHARS).count(), MAX_REPLY_CHARS);
    }
}
