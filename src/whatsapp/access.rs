//! Who may use the agent over WhatsApp.
//!
//! `WhatsApp::is_prompt` (in the `morpheus-whatsapp` crate) decides what is a
//! conversation at all — it drops echoes, broadcasts, channels and protocol
//! chatter. This module decides the separate question: whether the account
//! owner authorized that conversation to reach the agent. The policy is
//! read from `setup.json` → `whatsapp` and fails closed: until a contact or
//! group is listed there, only the owner's own self-chat is answered.

use morpheus_whatsapp::{ChatKind, IncomingMessage};

use crate::setup::WhatsAppConfig;

/// The resolved access policy, copied once from `setup.json` at startup.
#[derive(Clone, Debug, Default)]
pub struct Access {
    policy: WhatsAppConfig,
}

impl Access {
    /// Build the policy from the persisted `whatsapp` section.
    #[must_use]
    pub fn new(policy: WhatsAppConfig) -> Self {
        Self { policy }
    }

    /// Whether `message` may reach the agent.
    ///
    /// `from_me` messages are the account's own; `is_prompt` only lets the
    /// self-chat through, so the toggle alone applies to them. Every other
    /// message must match its chat's allowlist by kind — direct chats
    /// against `contacts`, groups against `groups` — and the matching entry
    /// must be ENABLED (a disabled entry stays saved but denies access).
    /// Chat kinds the policy does not model are never allowed.
    ///
    /// A direct chat is matched on both of its address forms: WhatsApp now
    /// routes some of them by LID, so a contact the owner wrote as a phone
    /// number arrives as `…@lid` and only [`IncomingMessage::sender_alt`]
    /// carries the number. Groups match on the group id alone — their
    /// `sender_alt` names a participant, not the group.
    #[must_use]
    pub fn allows(&self, message: &IncomingMessage) -> bool {
        if message.from_me {
            return self.policy.self_chat;
        }
        let allowed = match message.chat_kind {
            ChatKind::Direct => &self.policy.contacts,
            ChatKind::Group => &self.policy.groups,
            _ => return false,
        };
        let mut ids = vec![identity(message.chat.as_str())];
        if message.chat_kind == ChatKind::Direct
            && let Some(alt) = &message.sender_alt
        {
            ids.push(identity(alt.as_str()));
        }
        allowed
            .iter()
            .filter(|entry| entry.enabled && !entry.id.trim().is_empty())
            .map(|entry| identity(&entry.id))
            .any(|entry| ids.contains(&entry))
    }
}

/// The identity part of a JID or a user-written address: ASCII letters and
/// digits only, lowercased. The `@server` suffix, device (`:12`) suffix,
/// spaces, `+` and punctuation are addressing details, so
/// `+55 (11) 91234-5678`, `5511912345678` and
/// `5511912345678@s.whatsapp.net` all compare equal.
fn identity(id: &str) -> String {
    id.split(['@', ':'])
        .next()
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::setup::WhatsAppEntry;
    use morpheus_whatsapp::{ChatId, MessageContent, Sender};

    const OWNER: &str = "5511999999999";

    fn message(chat: &str, from_me: bool) -> IncomingMessage {
        let chat = ChatId::parse(chat).expect("chat id");
        IncomingMessage {
            id: "MSG1".to_string(),
            chat_kind: chat.kind(),
            sender: Sender {
                id: chat.clone(),
                name: None,
            },
            sender_alt: None,
            chat,
            from_me,
            timestamp: Utc::now(),
            content: MessageContent::Text("hello".into()),
        }
    }

    fn policy(contacts: &[&str], groups: &[&str]) -> Access {
        Access::new(WhatsAppConfig {
            self_chat: true,
            contacts: contacts.iter().map(|s| WhatsAppEntry::new(*s)).collect(),
            groups: groups.iter().map(|s| WhatsAppEntry::new(*s)).collect(),
        })
    }

    #[test]
    fn the_default_policy_is_the_self_chat_only() {
        let access = Access::default();
        assert!(access.allows(&message(
            &format!("{OWNER}@s.whatsapp.net"),
            true
        )));
        assert!(!access.allows(&message(
            "5511912345678@s.whatsapp.net",
            false
        )));
        assert!(!access.allows(&message("120363012345678901@g.us", false)));
    }

    #[test]
    fn contacts_match_with_or_without_formatting() {
        let access = policy(&["+55 (19) 91234-5678"], &[]);
        assert!(access.allows(&message("5519912345678@s.whatsapp.net", false)));
        // A different number stays denied.
        assert!(!access.allows(&message("5511912345678@s.whatsapp.net", false)));
        // A group message from that same contact is a group rule, not a
        // contact rule.
        assert!(!access.allows(&message("120363012345678901@g.us", false)));
    }

    /// A direct chat WhatsApp routes by LID: the phone number the owner wrote
    /// is not in the chat id, only in the alternate form the engine attaches.
    #[test]
    fn a_contact_allows_the_lid_routed_chat_of_that_number() {
        let access = policy(&["55 42 8889-5786"], &[]);
        let mut lid = message("42121264402678@lid", false);
        lid.sender_alt = Some(ChatId::parse("554288895786@s.whatsapp.net").expect("alt id"));
        assert!(access.allows(&lid));

        // The LID of another account is still denied.
        let mut other = message("42121264409999@lid", false);
        other.sender_alt = Some(ChatId::parse("5511912345678@s.whatsapp.net").expect("alt id"));
        assert!(!access.allows(&other));

        // And the same chat without the alternate form has nothing to match.
        assert!(!access.allows(&message("42121264402678@lid", false)));
    }

    /// A group's alternate form names a participant, so it must never let a
    /// contact rule (or a mismatched group id) admit the group.
    #[test]
    fn a_groups_alternate_form_does_not_admit_it() {
        let access = policy(&["554288895786"], &["120363012345678901"]);
        let mut group = message("120363012345678901@g.us", false);
        group.sender_alt = Some(ChatId::parse("554288895786@s.whatsapp.net").expect("alt id"));
        // Allowed by the group id, never by the participant's number.
        assert!(access.allows(&group));

        let mut stranger = message("120363099999999999@g.us", false);
        stranger.sender_alt = Some(ChatId::parse("554288895786@s.whatsapp.net").expect("alt id"));
        assert!(!access.allows(&stranger));
    }

    #[test]
    fn groups_match_by_id_or_digits() {
        let access = policy(&[], &["120363012345678901"]);
        assert!(access.allows(&message("120363012345678901@g.us", false)));
        assert!(!access.allows(&message("120363099999999999@g.us", false)));
        assert!(!access.allows(&message("5511912345678@s.whatsapp.net", false)));
    }

    /// Disabling keeps the entry in `setup.json` but denies that origin.
    #[test]
    fn a_disabled_entry_stays_saved_but_denies_access() {
        let mut access = policy(&["5511912345678"], &["120363012345678901"]);
        assert!(access.allows(&message("5511912345678@s.whatsapp.net", false)));
        assert!(access.allows(&message("120363012345678901@g.us", false)));

        access.policy.contacts[0].enabled = false;
        access.policy.groups[0].enabled = false;
        assert!(!access.allows(&message("5511912345678@s.whatsapp.net", false)));
        assert!(!access.allows(&message("120363012345678901@g.us", false)));
        assert_eq!(access.policy.contacts.len(), 1);
        assert_eq!(access.policy.groups.len(), 1);
    }

    #[test]
    fn contacts_and_groups_combine() {
        let access = policy(&["5511912345678"], &["120363012345678901@G.US"]);
        assert!(access.allows(&message("5511912345678@s.whatsapp.net", false)));
        assert!(access.allows(&message("120363012345678901@g.us", false)));
        assert!(!access.allows(&message("5511888888888@s.whatsapp.net", false)));
    }

    #[test]
    fn the_self_chat_toggle_and_blank_entries() {
        let mut off = policy(&["5511912345678"], &[]);
        off.policy.self_chat = false;
        assert!(!off.allows(&message(
            &format!("{OWNER}@s.whatsapp.net"),
            true
        )));
        // A blank entry never matches a chat with no identity left.
        let blank = policy(&["", "   "], &[]);
        assert!(!blank.allows(&message("5511912345678@s.whatsapp.net", false)));
    }

    #[test]
    fn unmodelled_chat_kinds_are_never_allowed() {
        let access = policy(&["5511912345678"], &["120363012345678901"]);
        // `ChatKind::Other` is not a direct chat or a group: the policy
        // fails closed instead of inheriting `is_prompt`'s tolerance.
        let mut unknown = message("5511912345678@s.whatsapp.net", false);
        unknown.chat_kind = ChatKind::Other;
        assert!(!access.allows(&unknown));

        let mut broadcast = message("status@broadcast", false);
        broadcast.chat_kind = ChatKind::Broadcast;
        assert!(!access.allows(&broadcast));
    }
}
