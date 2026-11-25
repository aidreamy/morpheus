//! Error and result types.
//!
//! Engine failures keep their `source` chain: the protocol layer reports
//! actionable detail (handshake stage, HTTP status, store error) and
//! flattening it to a string would throw that away.

use std::error::Error as StdError;

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// A boxed, thread-safe error from the protocol engine or the OS.
pub type BoxError = Box<dyn StdError + Send + Sync + 'static>;

/// Everything that can go wrong while linking, sending or receiving.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A chat identifier was not a valid WhatsApp JID.
    #[error("`{input}` is not a valid WhatsApp chat id")]
    InvalidChatId { input: String },

    /// The phone number given to
    /// [`Config::with_pairing_code`](crate::Config::with_pairing_code) is not
    /// a plausible international number (7–15 digits).
    #[error("`{phone}` is not a valid pairing phone number (expected 7 to 15 digits)")]
    InvalidPhone { phone: String },

    /// [`WhatsApp::send_text`](crate::WhatsApp::send_text) was given empty
    /// (or whitespace-only) text.
    #[error("the message text is empty")]
    EmptyMessage,

    /// The SQLite device store could not be opened or migrated.
    #[error("the WhatsApp device store could not be opened")]
    Storage(#[source] BoxError),

    /// The link could not be established or resumed.
    #[error("the WhatsApp session failed")]
    Session(#[source] BoxError),

    /// The message could not be encrypted or handed to the server.
    #[error("the WhatsApp message could not be sent")]
    Send(#[source] BoxError),

    /// The chat state (typing indicator) could not be sent.
    #[error("the WhatsApp chat state could not be sent")]
    ChatState(#[source] BoxError),

    /// The pairing payload could not be encoded as a QR code.
    ///
    /// The underlying `qrcode` error is kept as the source, not as a public
    /// type: a major bump of that crate must not be breaking for callers.
    #[error("the pairing payload could not be rendered as a QR code")]
    Qr(#[source] BoxError),

    /// The link stopped without the account being logged out (the engine's
    /// run loop ended, e.g. after a fatal protocol failure).
    #[error("the WhatsApp link stopped before the account was logged out")]
    Stopped,

    /// The engine reported an unrecoverable failure; the link cannot
    /// recover without caller intervention.
    #[error("the WhatsApp link failed ({kind:?}): {reason}")]
    LinkFailed {
        /// What kind of failure ended the link, so a supervisor can react
        /// without parsing `reason`.
        kind: crate::FailureKind,
        /// Human-readable detail from the engine.
        reason: String,
    },
}

impl Error {
    pub(crate) fn storage(err: impl Into<BoxError>) -> Self {
        Self::Storage(err.into())
    }

    pub(crate) fn session(err: impl Into<BoxError>) -> Self {
        Self::Session(err.into())
    }

    pub(crate) fn send(err: impl Into<BoxError>) -> Self {
        Self::Send(err.into())
    }

    pub(crate) fn chat_state(err: impl Into<BoxError>) -> Self {
        Self::ChatState(err.into())
    }

    pub(crate) fn qr(err: impl Into<BoxError>) -> Self {
        Self::Qr(err.into())
    }

    /// Validate a pairing phone number (7–15 digits, like
    /// [`ChatId::phone`](crate::ChatId::phone)).
    pub(crate) fn check_phone(phone: &str) -> Result<()> {
        if (7usize..=15).contains(&phone.len()) && phone.bytes().all(|b| b.is_ascii_digit()) {
            Ok(())
        } else {
            Err(Self::InvalidPhone {
                phone: phone.to_string(),
            })
        }
    }
}
