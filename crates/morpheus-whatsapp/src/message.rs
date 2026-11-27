//! Engine-independent message model.
//!
//! Inbound messages are normalized here once, so agent code never matches on
//! generated protobuf types: a [`MessageContent`] tells you what arrived and
//! [`IncomingMessage`] carries the addressing, sender and timestamp.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use whatsapp_rust::Jid;
use whatsapp_rust::proto_helpers::MessageExt;
use whatsapp_rust::types::message::MessageInfo;
use whatsapp_rust::waproto::whatsapp as wa;

use crate::error::{Error, Result};

/// A WhatsApp chat address, in its canonical `user@server` form.
///
/// Direct chats use `@s.whatsapp.net`, groups `@g.us`, broadcasts
/// (including `status@broadcast`) `@broadcast` and channels `@newsletter`.
/// WhatsApp is migrating some direct chats to LID addresses (`@lid`), so a
/// chat id is treated as an opaque canonical string, not as a phone number.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChatId(String);

impl ChatId {
    /// Build a direct-chat id from a phone number in international format.
    ///
    /// `+55 (11) 91234-5678` and `5511912345678` both produce
    /// `5511912345678@s.whatsapp.net`.
    pub fn phone(number: &str) -> Result<Self> {
        let digits: String = number.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() < 7 || digits.len() > 15 {
            return Err(Error::InvalidChatId {
                input: number.to_string(),
            });
        }
        Ok(Self(format!("{digits}@s.whatsapp.net")))
    }

    /// Build a group id from its numeric part (e.g. `120363012345678901`).
    pub fn group(id: impl AsRef<str>) -> Result<Self> {
        let id = id.as_ref();
        let user = id.strip_suffix("@g.us").unwrap_or(id);
        if user.is_empty() || !user.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
            return Err(Error::InvalidChatId {
                input: id.to_string(),
            });
        }
        Ok(Self(format!("{user}@g.us")))
    }

    /// Parse a full JID (`user@server`). Plain phone numbers are rejected
    /// here; use [`ChatId::phone`] for those.
    pub fn parse(raw: &str) -> Result<Self> {
        let normalized = raw.trim().to_ascii_lowercase();
        if !normalized.contains('@') {
            return Err(Error::InvalidChatId {
                input: raw.to_string(),
            });
        }
        let jid = Jid::from_str(&normalized).map_err(|_| Error::InvalidChatId {
            input: raw.to_string(),
        })?;
        Ok(Self(jid.to_string()))
    }

    /// The canonical `user@server` string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The addressing part of the JID - the `user` in `user@server`, without
    /// the device suffix of `user:12@server`.
    ///
    /// This is what identifies an *account*, independent of which of its
    /// devices sent the message and of whether it is addressed by phone
    /// number (`@s.whatsapp.net`) or by LID (`@lid`).
    pub fn user(&self) -> &str {
        self.0.split(['@', ':']).next().unwrap_or_default()
    }

    /// What kind of conversation this chat is.
    pub fn kind(&self) -> ChatKind {
        match Jid::from_str(&self.0) {
            Ok(jid) => ChatKind::from_jid(&jid),
            Err(_) => ChatKind::Other,
        }
    }

    /// Whether this is a one-to-one chat (including LID and hosted ones).
    pub fn is_direct(&self) -> bool {
        self.kind() == ChatKind::Direct
    }

    /// Whether this is a group chat.
    pub fn is_group(&self) -> bool {
        self.kind() == ChatKind::Group
    }

    /// Whether this is the broadcast/status namespace.
    pub fn is_broadcast(&self) -> bool {
        self.kind() == ChatKind::Broadcast
    }

    /// Whether this is a channel (newsletter).
    pub fn is_newsletter(&self) -> bool {
        self.kind() == ChatKind::Newsletter
    }

    /// A stable, filesystem-safe key for this chat.
    ///
    /// Every byte outside `[A-Za-z0-9._-]` becomes `_`, so the key is safe on
    /// every platform: `5511912345678@s.whatsapp.net` becomes
    /// `5511912345678_s.whatsapp.net`, and a device-qualified JID
    /// (`5511912345678:12@...`) becomes `5511912345678_12_s.whatsapp.net`
    /// instead of carrying a `:` that Windows rejects.
    ///
    /// The key is an identifier, **not** a JID: pass it to a file or session
    /// store, never back to [`ChatId::parse`].
    pub fn storage_key(&self) -> String {
        self.0
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

    pub(crate) fn from_jid(jid: &Jid) -> Self {
        Self(jid.to_string())
    }

    pub(crate) fn to_jid(&self) -> Result<Jid> {
        Jid::from_str(&self.0).map_err(|_| Error::InvalidChatId {
            input: self.0.clone(),
        })
    }
}

impl std::fmt::Display for ChatId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What kind of conversation a chat is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ChatKind {
    /// One-to-one chat.
    Direct,
    /// Group chat.
    Group,
    /// Broadcast list or status feed.
    Broadcast,
    /// Channel (newsletter).
    Newsletter,
    /// A server namespace this crate does not model yet.
    Other,
}

impl ChatKind {
    fn from_jid(jid: &Jid) -> Self {
        // `is_pn_family`/`is_lid_family` also cover the hosted namespaces
        // (`@hosted`, `@hosted.lid`), which are still one-to-one chats.
        let server = jid.server;
        if server.is_pn_family() || server.is_lid_family() {
            Self::Direct
        } else {
            match server.as_str() {
                "g.us" => Self::Group,
                "broadcast" => Self::Broadcast,
                "newsletter" => Self::Newsletter,
                // Legacy phone-number and non-WhatsApp identities are
                // still addressed to one conversation.
                "c.us" | "msgr" | "interop" | "bot" => Self::Direct,
                _ => Self::Other,
            }
        }
    }
}

/// Who sent an inbound message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sender {
    /// The sender's JID (for groups this is the participant, not the chat).
    pub id: ChatId,
    /// The sender's WhatsApp profile name, when the server sent one.
    pub name: Option<String>,
}

/// The payload of an inbound message.
///
/// Media variants carry only metadata; this crate does not download bytes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MessageContent {
    /// Plain text (conversation or extended text).
    Text(String),
    /// Image, with its optional caption.
    Image {
        caption: Option<String>,
        mime_type: Option<String>,
    },
    /// Video, including GIF playback.
    Video {
        caption: Option<String>,
        seconds: Option<u32>,
        gif: bool,
    },
    /// Audio, distinguishing voice notes from audio files.
    Audio {
        voice_note: bool,
        seconds: Option<u32>,
    },
    /// Document, with its file name and optional caption.
    Document {
        file_name: Option<String>,
        caption: Option<String>,
    },
    /// Sticker.
    Sticker,
    /// Static or live location.
    ///
    /// Coordinates are optional because a message can lose them on the way
    /// (live-location updates, malformed sends); a missing `latitude` is not
    /// the same point as `0.0`, which sits in the Gulf of Guinea.
    Location {
        latitude: Option<f64>,
        longitude: Option<f64>,
        name: Option<String>,
    },
    /// Shared contact card.
    Contact { display_name: Option<String> },
    /// Reaction to another message (empty emoji removes a reaction).
    Reaction {
        emoji: String,
        target_message_id: Option<String>,
    },
    /// A poll the user created.
    Poll {
        question: String,
        options: Vec<String>,
    },
    /// Protocol chatter: edits, deletes, key shares, receipts.
    System,
    /// A message type this crate does not model (payments, events, …).
    Unsupported,
}

impl MessageContent {
    /// A short, stable label for logs and routing (`"text"`, `"image"`, …).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Image { .. } => "image",
            Self::Video { .. } => "video",
            Self::Audio { .. } => "audio",
            Self::Document { .. } => "document",
            Self::Sticker => "sticker",
            Self::Location { .. } => "location",
            Self::Contact { .. } => "contact",
            Self::Reaction { .. } => "reaction",
            Self::Poll { .. } => "poll",
            Self::System => "system",
            Self::Unsupported => "unsupported",
        }
    }
}

/// A decrypted message from the linked account.
///
/// `from_me` messages are what this account sent from any of its devices -
/// the linked phone included - echoed back to this companion. They include
/// the agent's own replies, which must never be answered again; use
/// [`WhatsApp::is_prompt`](crate::WhatsApp::is_prompt) to decide what is
/// addressed to the agent instead of testing this flag directly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IncomingMessage {
    /// Server message id; unique within a chat.
    pub id: String,
    /// The conversation this message belongs to.
    pub chat: ChatId,
    /// Direct chat, group, broadcast or channel.
    pub chat_kind: ChatKind,
    /// Who wrote it.
    pub sender: Sender,
    /// The sender's other address form, when the engine delivered both: a
    /// chat addressed by LID carries the sender's phone-number JID here, and a
    /// chat addressed by phone number carries the LID. `None` when the stanza
    /// supplied a single form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_alt: Option<ChatId>,
    /// Whether the message was sent by this account.
    pub from_me: bool,
    /// Message timestamp.
    pub timestamp: DateTime<Utc>,
    /// What it contains.
    pub content: MessageContent,
}

impl IncomingMessage {
    /// The message text, for text messages only.
    pub fn text(&self) -> Option<&str> {
        match &self.content {
            MessageContent::Text(text) => Some(text),
            _ => None,
        }
    }

    /// The caption of a media message, when it has one.
    pub fn caption(&self) -> Option<&str> {
        match &self.content {
            MessageContent::Image { caption, .. }
            | MessageContent::Video { caption, .. }
            | MessageContent::Document { caption, .. } => caption.as_deref(),
            _ => None,
        }
    }

    pub(crate) fn from_engine(message: &wa::Message, info: &MessageInfo) -> Self {
        let name = (!info.push_name.is_empty()).then(|| info.push_name.clone());
        Self {
            id: info.id.to_string(),
            chat: ChatId::from_jid(&info.source.chat),
            chat_kind: ChatKind::from_jid(&info.source.chat),
            sender: Sender {
                id: ChatId::from_jid(&info.source.sender),
                name,
            },
            sender_alt: info.source.sender_alt.as_ref().map(ChatId::from_jid),
            from_me: info.source.is_from_me,
            timestamp: info.timestamp,
            content: content_of(message),
        }
    }
}

fn content_of(message: &wa::Message) -> MessageContent {
    let base = message.get_base_message();

    if let Some(text) = &base.conversation {
        return MessageContent::Text(text.clone());
    }
    if let Some(text) = base
        .extended_text_message
        .as_option()
        .and_then(|m| m.text.as_ref())
    {
        return MessageContent::Text(text.clone());
    }
    if let Some(image) = base.image_message.as_option() {
        return MessageContent::Image {
            caption: image.caption.clone(),
            mime_type: image.mimetype.clone(),
        };
    }
    if let Some(video) = base.video_message.as_option() {
        return MessageContent::Video {
            caption: video.caption.clone(),
            seconds: video.seconds,
            gif: video.gif_playback.unwrap_or(false),
        };
    }
    if let Some(audio) = base.audio_message.as_option() {
        return MessageContent::Audio {
            voice_note: audio.ptt.unwrap_or(false),
            seconds: audio.seconds,
        };
    }
    if let Some(document) = base.document_message.as_option() {
        return MessageContent::Document {
            file_name: document.file_name.clone(),
            caption: document.caption.clone(),
        };
    }
    if base.sticker_message.is_set() {
        return MessageContent::Sticker;
    }
    if let Some(location) = base.location_message.as_option() {
        return MessageContent::Location {
            latitude: location.degrees_latitude,
            longitude: location.degrees_longitude,
            name: location.name.clone(),
        };
    }
    if let Some(contact) = base.contact_message.as_option() {
        return MessageContent::Contact {
            display_name: contact.display_name.clone(),
        };
    }
    if let Some(reaction) = base.reaction_message.as_option() {
        return MessageContent::Reaction {
            emoji: reaction.text.clone().unwrap_or_default(),
            target_message_id: reaction.key.as_option().and_then(|key| key.id.clone()),
        };
    }
    if let Some(poll) = base.poll_creation_message.as_option() {
        return MessageContent::Poll {
            question: poll.name.clone().unwrap_or_default(),
            options: poll
                .options
                .iter()
                .filter_map(|option| option.option_name.clone())
                .collect(),
        };
    }
    if base.protocol_message.is_set() {
        return MessageContent::System;
    }

    MessageContent::Unsupported
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(chat: &str, sender: &str, from_me: bool) -> MessageInfo {
        let mut info = MessageInfo::default();
        info.source.chat = Jid::from_str(chat).expect("chat jid");
        info.source.sender = Jid::from_str(sender).expect("sender jid");
        info.source.is_from_me = from_me;
        info.source.is_group = chat.ends_with("@g.us");
        info.id = "ABC123".into();
        info.push_name = "Ana".to_string();
        info
    }

    fn text(value: &str) -> wa::Message {
        wa::Message {
            conversation: Some(value.to_string()),
            ..Default::default()
        }
    }

    /// A set (present) nested message, the shape the generated protobuf API
    /// uses in place of a plain `Option`.
    fn field<T: Default>(value: T) -> whatsapp_rust::waproto::buffa::MessageField<T> {
        whatsapp_rust::waproto::buffa::MessageField::some(value)
    }

    #[test]
    fn phone_ids_strip_formatting() {
        let chat = ChatId::phone("+55 (11) 91234-5678").expect("valid phone");
        assert_eq!(chat.as_str(), "5511912345678@s.whatsapp.net");
        assert!(!chat.is_group());
    }

    #[test]
    fn phone_ids_reject_short_numbers() {
        assert!(matches!(
            ChatId::phone("1234"),
            Err(Error::InvalidChatId { .. })
        ));
    }

    #[test]
    fn group_ids_normalize_suffix() {
        let chat = ChatId::group("120363012345678901@g.us").expect("valid group");
        assert_eq!(chat.as_str(), "120363012345678901@g.us");
        assert!(chat.is_group());
    }

    #[test]
    fn parse_keeps_canonical_jid() {
        let chat = ChatId::parse(" 5511912345678@S.Whatsapp.Net ").expect("valid jid");
        assert_eq!(chat.as_str(), "5511912345678@s.whatsapp.net");
    }

    #[test]
    fn parse_rejects_bare_phone_numbers() {
        assert!(matches!(
            ChatId::parse("5511912345678"),
            Err(Error::InvalidChatId { .. })
        ));
    }

    #[test]
    fn user_is_the_account_part_of_a_jid() {
        // Device suffixes and the server are addressing details, not identity:
        // the same account is reached through all of these.
        for raw in [
            "5511912345678@s.whatsapp.net",
            "5511912345678:12@s.whatsapp.net",
            "5511912345678@hosted",
        ] {
            let chat = ChatId::parse(raw).expect("jid");
            assert_eq!(chat.user(), "5511912345678", "{raw}");
        }
        let lid = ChatId::parse("99887766554433@lid").expect("lid");
        assert_eq!(lid.user(), "99887766554433");
    }

    #[test]
    fn storage_key_has_no_at_sign() {
        let chat = ChatId::phone("5511912345678").expect("valid phone");
        assert_eq!(chat.storage_key(), "5511912345678_s.whatsapp.net");
    }

    #[test]
    fn storage_key_is_filesystem_safe_for_device_jids() {
        // Device-qualified JIDs (user:device@server) are produced by the
        // engine and accepted by `ChatId::parse`; the key must not carry a
        // `:`, which is invalid in Windows file names and an ADS separator.
        let chat = ChatId::parse("5511912345678:12@s.whatsapp.net").expect("device jid");
        let key = chat.storage_key();
        assert!(!key.contains(':'), "unsafe storage key: {key}");
        assert!(
            key.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
            "unsafe storage key: {key}"
        );
    }

    #[test]
    fn lid_and_hosted_jids_are_direct_chats() {
        // `@lid` and the newer hosted namespaces are one-to-one chats, not
        // "other" servers.
        for raw in [
            "123456789012345@lid",
            "5511912345678@hosted",
            "123456789012345@hosted.lid",
        ] {
            let jid = Jid::from_str(raw).expect("jid");
            assert_eq!(ChatKind::from_jid(&jid), ChatKind::Direct, "{raw}");
        }
    }

    #[test]
    fn newsletter_jids_are_channels() {
        let jid = Jid::from_str("120363012345678901@newsletter").expect("jid");
        assert_eq!(ChatKind::from_jid(&jid), ChatKind::Newsletter);
        let chat = ChatId::parse("120363012345678901@newsletter").expect("jid");
        assert!(chat.is_newsletter());
    }

    #[test]
    fn content_of_models_media_metadata() {
        let video = wa::Message {
            video_message: field(wa::message::VideoMessage {
                caption: Some("clip".to_string()),
                seconds: Some(9),
                gif_playback: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            content_of(&video),
            MessageContent::Video {
                caption: Some("clip".to_string()),
                seconds: Some(9),
                gif: true,
            }
        );

        let audio = wa::Message {
            audio_message: field(wa::message::AudioMessage {
                ptt: Some(true),
                seconds: Some(4),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            content_of(&audio),
            MessageContent::Audio {
                voice_note: true,
                seconds: Some(4),
            }
        );

        let document = wa::Message {
            document_message: field(wa::message::DocumentMessage {
                file_name: Some("report.pdf".to_string()),
                caption: Some("the report".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            content_of(&document),
            MessageContent::Document {
                file_name: Some("report.pdf".to_string()),
                caption: Some("the report".to_string()),
            }
        );

        let sticker = wa::Message {
            sticker_message: field(wa::message::StickerMessage::default()),
            ..Default::default()
        };
        assert_eq!(content_of(&sticker), MessageContent::Sticker);

        let contact = wa::Message {
            contact_message: field(wa::message::ContactMessage {
                display_name: Some("Ana".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            content_of(&contact),
            MessageContent::Contact {
                display_name: Some("Ana".to_string()),
            }
        );
    }

    #[test]
    fn content_of_keeps_location_coordinates() {
        let message = wa::Message {
            location_message: field(wa::message::LocationMessage {
                degrees_latitude: Some(-23.5505),
                degrees_longitude: Some(-46.6333),
                name: Some("Office".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            content_of(&message),
            MessageContent::Location {
                latitude: Some(-23.5505),
                longitude: Some(-46.6333),
                name: Some("Office".to_string()),
            }
        );
    }

    #[test]
    fn location_without_coordinates_is_not_a_bogus_point() {
        // A location that lost its coordinates must not look like a valid
        // point in the Gulf of Guinea.
        let message = wa::Message {
            location_message: field(wa::message::LocationMessage {
                name: Some("Somewhere".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            content_of(&message),
            MessageContent::Location {
                latitude: None,
                longitude: None,
                name: Some("Somewhere".to_string()),
            }
        );
    }

    #[test]
    fn unknown_payload_is_unsupported() {
        assert_eq!(
            content_of(&wa::Message::default()),
            MessageContent::Unsupported
        );
    }

    #[test]
    fn direct_message_normalization() {
        let message = IncomingMessage::from_engine(
            &text("hello"),
            &info(
                "5511912345678@s.whatsapp.net",
                "5511912345678@s.whatsapp.net",
                false,
            ),
        );

        assert_eq!(message.text(), Some("hello"));
        assert_eq!(message.chat_kind, ChatKind::Direct);
        assert_eq!(message.id, "ABC123");
        assert_eq!(message.sender.name.as_deref(), Some("Ana"));
        assert_eq!(message.sender_alt, None);
        assert!(!message.from_me);
    }

    /// A LID-routed direct chat keeps the phone-number form the engine read
    /// from the stanza, so policy code can recognize a contact the owner
    /// wrote as a number.
    #[test]
    fn the_alternate_address_form_survives_normalization() {
        let mut info = info("42121264402678@lid", "42121264402678@lid", false);
        info.source.sender_alt =
            Some(Jid::from_str("554288895786@s.whatsapp.net").expect("pn jid"));

        let message = IncomingMessage::from_engine(&text("hello"), &info);

        assert_eq!(message.chat.as_str(), "42121264402678@lid");
        assert_eq!(
            message.sender_alt.as_ref().map(ChatId::as_str),
            Some("554288895786@s.whatsapp.net")
        );
    }

    #[test]
    fn group_message_uses_participant_as_sender() {
        let message = IncomingMessage::from_engine(
            &text("hi group"),
            &info(
                "120363012345678901@g.us",
                "5511912345678@s.whatsapp.net",
                false,
            ),
        );

        assert_eq!(message.chat_kind, ChatKind::Group);
        assert_eq!(message.sender.id.as_str(), "5511912345678@s.whatsapp.net");
        assert_eq!(message.chat.as_str(), "120363012345678901@g.us");
    }

    #[test]
    fn extended_text_is_text() {
        let message = wa::Message {
            extended_text_message: field(wa::message::ExtendedTextMessage {
                text: Some("link preview".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let normalized = IncomingMessage::from_engine(
            &message,
            &info(
                "5511912345678@s.whatsapp.net",
                "5511912345678@s.whatsapp.net",
                false,
            ),
        );
        assert_eq!(normalized.text(), Some("link preview"));
    }

    #[test]
    fn ephemeral_wrapper_is_unwrapped() {
        let message = wa::Message {
            ephemeral_message: field(wa::message::FutureProofMessage {
                message: field(text("vanishing")),
            }),
            ..Default::default()
        };
        let normalized = IncomingMessage::from_engine(
            &message,
            &info(
                "5511912345678@s.whatsapp.net",
                "5511912345678@s.whatsapp.net",
                false,
            ),
        );
        assert_eq!(normalized.text(), Some("vanishing"));
    }

    #[test]
    fn media_caption_is_exposed() {
        let message = wa::Message {
            image_message: field(wa::message::ImageMessage {
                caption: Some("see this".to_string()),
                mimetype: Some("image/jpeg".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let normalized = IncomingMessage::from_engine(
            &message,
            &info(
                "5511912345678@s.whatsapp.net",
                "5511912345678@s.whatsapp.net",
                false,
            ),
        );

        assert_eq!(normalized.caption(), Some("see this"));
        assert_eq!(normalized.text(), None);
        assert_eq!(normalized.content.kind(), "image");
    }

    #[test]
    fn reaction_keeps_target_and_emoji() {
        let message = wa::Message {
            reaction_message: field(wa::message::ReactionMessage {
                text: Some("👍".to_string()),
                key: field(wa::MessageKey {
                    id: Some("TARGET".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let normalized = IncomingMessage::from_engine(
            &message,
            &info(
                "5511912345678@s.whatsapp.net",
                "5511912345678@s.whatsapp.net",
                true,
            ),
        );

        assert_eq!(
            normalized.content,
            MessageContent::Reaction {
                emoji: "👍".to_string(),
                target_message_id: Some("TARGET".to_string()),
            }
        );
        assert!(normalized.from_me);
    }

    #[test]
    fn protocol_messages_are_system() {
        let message = wa::Message {
            protocol_message: field(wa::message::ProtocolMessage::default()),
            ..Default::default()
        };
        let normalized = IncomingMessage::from_engine(
            &message,
            &info(
                "5511912345678@s.whatsapp.net",
                "5511912345678@s.whatsapp.net",
                false,
            ),
        );
        assert_eq!(normalized.content, MessageContent::System);
    }

    #[test]
    fn poll_keeps_question_and_options() {
        let message = wa::Message {
            poll_creation_message: field(wa::message::PollCreationMessage {
                name: Some("Lunch?".to_string()),
                options: vec![
                    wa::message::poll_creation_message::Option {
                        option_name: Some("Pizza".to_string()),
                        ..Default::default()
                    },
                    wa::message::poll_creation_message::Option {
                        option_name: Some("Sushi".to_string()),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        let normalized = IncomingMessage::from_engine(
            &message,
            &info(
                "120363012345678901@g.us",
                "5511912345678@s.whatsapp.net",
                false,
            ),
        );

        assert_eq!(
            normalized.content,
            MessageContent::Poll {
                question: "Lunch?".to_string(),
                options: vec!["Pizza".to_string(), "Sushi".to_string()],
            }
        );
    }
}
