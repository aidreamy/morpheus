//! Wire types of the OpenCode HTTP server.
//!
//! Only the parts Morpheus actually consumes are modeled, and every field is
//! optional/defaulted: the server is a separate, independently versioned
//! project, so an unknown or new field must never break a turn. Events are
//! handled as [`serde_json::Value`] (see [`super::event`]) for the same
//! reason — the event union grows with every server release.
//!
//! Naming follows the server's own camelCase wire format (`sessionID`,
//! `messageID`, `providerID`, …), so the `ID` suffixes are spelled out.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// `GET /global/health` — liveness plus the server version.
#[derive(Debug, Clone, Deserialize)]
pub struct Health {
    #[serde(default)]
    pub healthy: bool,
    #[serde(default)]
    pub version: Option<String>,
}

/// One server session (`POST /session`, `GET /session/:id`).
#[derive(Debug, Clone, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub directory: String,
    /// The agent (mode) the session runs with, when the server records one.
    #[serde(default)]
    pub agent: Option<String>,
    /// The session's model, when the server records one.
    #[serde(default)]
    pub model: Option<SessionModel>,
}

/// The model a session runs with.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionModel {
    #[serde(default)]
    pub id: String,
    #[serde(default, rename = "providerID")]
    pub provider_id: String,
}

/// A message with its parts, as returned by the message endpoints.
#[derive(Debug, Clone, Deserialize)]
pub struct MessageWithParts {
    #[serde(default)]
    pub info: MessageInfo,
    #[serde(default)]
    pub parts: Vec<Part>,
}

/// Message metadata: just enough to tell the roles apart and to surface a
/// provider/model error the server reports on the assistant message.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MessageInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default, rename = "sessionID")]
    pub session_id: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub error: Option<MessageError>,
    #[serde(default)]
    pub tokens: Option<Tokens>,
    #[serde(default, rename = "modelID")]
    pub model_id: String,
    #[serde(default, rename = "providerID")]
    pub provider_id: String,
}

/// Token accounting the server reports on an assistant message.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Tokens {
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub reasoning: u64,
    #[serde(default)]
    pub cache: CacheTokens,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CacheTokens {
    #[serde(default)]
    pub read: u64,
    #[serde(default)]
    pub write: u64,
}

impl Tokens {
    /// Tokens occupying the context window: the prompt side (uncached plus
    /// cache reads/writes) plus the completion.
    #[must_use]
    pub fn in_context(&self) -> u64 {
        self.input
            .saturating_add(self.cache.read)
            .saturating_add(self.cache.write)
            .saturating_add(self.output)
    }

    /// Whether the server actually reported numbers (an all-zero block is
    /// the pre-turn placeholder, not a measurement).
    #[must_use]
    pub fn reported(&self) -> bool {
        self.total > 0 || self.in_context() > 0
    }
}

impl MessageInfo {
    #[must_use]
    pub fn is_assistant(&self) -> bool {
        self.role == "assistant"
    }
}

/// The error object some server versions attach to a failed assistant
/// message (`{ name, data: { message } }`).
#[derive(Debug, Clone, Deserialize)]
pub struct MessageError {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub data: serde_json::Value,
}

impl MessageError {
    /// A human-readable rendering of the error, falling back to the raw
    /// JSON when the server's shape does not match.
    #[must_use]
    pub fn message(&self) -> String {
        if let Some(text) = self.data.get("message").and_then(serde_json::Value::as_str) {
            if self.name.is_empty() {
                return text.to_string();
            }
            return format!("{}: {text}", self.name);
        }
        if self.name.is_empty() {
            self.data.to_string()
        } else {
            format!("{}: {}", self.name, self.data)
        }
    }
}

/// One part of a message. Tagged by the server's `type` string; unknown
/// types (snapshots, patches, retries, …) degrade to [`Part::Other`].
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Part {
    /// Assistant or user prose. `text` is the FULL text so far; the stream
    /// event may carry a `delta` next to it.
    Text(TextPart),
    /// The model's internal reasoning.
    Reasoning(ReasoningPart),
    /// A tool call, with its lifecycle state.
    Tool(Box<ToolPart>),
    /// Anything this client does not model yet.
    #[serde(other)]
    Other,
}

impl Part {
    /// The session the part belongs to (empty when the server omitted it).
    #[must_use]
    pub fn session_id(&self) -> &str {
        match self {
            Self::Text(part) => &part.session_id,
            Self::Reasoning(part) => &part.session_id,
            Self::Tool(part) => &part.session_id,
            Self::Other => "",
        }
    }

    /// The message the part belongs to (empty when omitted).
    #[must_use]
    pub fn message_id(&self) -> &str {
        match self {
            Self::Text(part) => &part.message_id,
            Self::Reasoning(part) => &part.message_id,
            Self::Tool(part) => &part.message_id,
            Self::Other => "",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TextPart {
    #[serde(default)]
    pub id: String,
    #[serde(default, rename = "sessionID")]
    pub session_id: String,
    #[serde(default, rename = "messageID")]
    pub message_id: String,
    #[serde(default)]
    pub text: String,
    /// Server-injected text (file contents, snapshots) that the model sees
    /// but the user did not type.
    #[serde(default)]
    pub synthetic: bool,
    #[serde(default)]
    pub ignored: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReasoningPart {
    #[serde(default)]
    pub id: String,
    #[serde(default, rename = "sessionID")]
    pub session_id: String,
    #[serde(default, rename = "messageID")]
    pub message_id: String,
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolPart {
    #[serde(default)]
    pub id: String,
    #[serde(default, rename = "sessionID")]
    pub session_id: String,
    #[serde(default, rename = "messageID")]
    pub message_id: String,
    #[serde(default, rename = "callID")]
    pub call_id: String,
    /// The tool's real name (`bash`, `read`, `edit`, …).
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub state: ToolState,
}

/// Lifecycle of a tool call. Tagged by the server's `status`; unknown
/// statuses degrade to [`ToolState::Unknown`].
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ToolState {
    Pending {
        #[serde(default)]
        input: serde_json::Value,
    },
    Running {
        #[serde(default)]
        input: serde_json::Value,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        metadata: Option<serde_json::Value>,
    },
    Completed {
        #[serde(default)]
        input: serde_json::Value,
        #[serde(default)]
        output: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        metadata: Option<serde_json::Value>,
    },
    Error {
        #[serde(default)]
        input: serde_json::Value,
        #[serde(default)]
        error: String,
        #[serde(default)]
        metadata: Option<serde_json::Value>,
    },
    #[default]
    #[serde(other)]
    Unknown,
}

impl ToolState {
    /// The tool's input arguments, when the state carries them.
    #[must_use]
    pub fn input(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Pending { input } | Self::Running { input, .. } => Some(input),
            Self::Completed { input, .. } | Self::Error { input, .. } => Some(input),
            Self::Unknown => None,
        }
    }

    /// The title the tool reports (its human summary), when it has one.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        match self {
            Self::Running { title, .. } => title.as_deref(),
            Self::Completed { title, .. } => Some(title.as_str()),
            _ => None,
        }
    }

    /// The tool's structured metadata (the edit tool carries its diff here).
    #[must_use]
    pub fn metadata(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Running { metadata, .. }
            | Self::Completed { metadata, .. }
            | Self::Error { metadata, .. } => metadata.as_ref(),
            Self::Pending { .. } | Self::Unknown => None,
        }
    }

    /// The tool's output text (completed) or error text (error).
    #[must_use]
    pub fn output(&self) -> Option<&str> {
        match self {
            Self::Completed { output, .. } if !output.is_empty() => Some(output),
            Self::Error { error, .. } if !error.is_empty() => Some(error),
            _ => None,
        }
    }

    #[must_use]
    pub fn status(&self) -> &'static str {
        match self {
            Self::Pending { .. } => "pending",
            Self::Running { .. } => "running",
            Self::Completed { .. } => "completed",
            Self::Error { .. } => "error",
            Self::Unknown => "unknown",
        }
    }
}

/// One item of a session's todo list (`todo.updated`).
#[derive(Debug, Clone, Deserialize)]
pub struct Todo {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub priority: String,
}

/// A permission request awaiting an answer (`permission.updated`).
#[derive(Debug, Clone, Deserialize)]
pub struct Permission {
    #[serde(default)]
    pub id: String,
    /// The tool category (`bash`, `edit`, `webfetch`, …).
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default, rename = "sessionID")]
    pub session_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// One agent the server exposes (`GET /agent`). OpenCode calls these
/// "agents"; from Morpheus's point of view they are the session modes.
#[derive(Debug, Clone, Deserialize)]
pub struct Agent {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub mode: String,
}

/// One slash command the server exposes (`GET /command`).
#[derive(Debug, Clone, Deserialize)]
pub struct Command {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// `GET /config/providers` — the providers and the models each one serves.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Providers {
    #[serde(default)]
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub default: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Provider {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub models: HashMap<String, Model>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Model {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// The model's declared limits (context window), when published.
    #[serde(default)]
    pub limit: Option<ModelLimit>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelLimit {
    #[serde(default)]
    pub context: Option<u64>,
    #[serde(default)]
    pub output: Option<u64>,
}

/// The slice of `GET /config` this client reads: the configured model, in
/// the `provider/model` spelling.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub model: Option<String>,
}

/// Body of `POST /session/:id/message` and `/prompt_async`.
#[derive(Debug, Clone, Serialize)]
pub struct PromptBody<'a> {
    pub parts: Vec<PromptPart<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PromptPart<'a> {
    Text { text: &'a str },
}

/// A model addressed the way the server wants it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelRef {
    #[serde(rename = "providerID")]
    pub provider: String,
    #[serde(rename = "modelID")]
    pub model: String,
}

impl ModelRef {
    /// Parse the `provider/model` spelling the server uses everywhere else
    /// (config values, model lists). `None` for a value without a provider
    /// prefix — the server cannot address such a model.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let (provider, model) = value.split_once('/')?;
        if provider.is_empty() || model.is_empty() {
            return None;
        }
        Some(Self {
            provider: provider.to_string(),
            model: model.to_string(),
        })
    }
}

/// The permission answer values the server accepts
/// (`POST /session/:id/permissions/:permissionID`).
pub mod answer {
    pub const ONCE: &str = "once";
    pub const ALWAYS: &str = "always";
    pub const REJECT: &str = "reject";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_refs_split_on_the_first_slash() {
        let model = ModelRef::parse("anthropic/claude-sonnet-5").expect("a model");
        assert_eq!(model.provider, "anthropic");
        assert_eq!(model.model, "claude-sonnet-5");
        assert_eq!(
            ModelRef::parse("openrouter/meta-llama/llama-3").expect("nested id"),
            ModelRef {
                provider: "openrouter".to_string(),
                model: "meta-llama/llama-3".to_string(),
            }
        );
        assert!(ModelRef::parse("claude-sonnet-5").is_none());
        assert!(ModelRef::parse("/model").is_none());
        assert!(ModelRef::parse("provider/").is_none());
    }

    #[test]
    fn tool_states_deserialize_from_the_wire_shape() {
        let part: Part = serde_json::from_value(serde_json::json!({
            "type": "tool",
            "id": "prt_1",
            "messageID": "msg_1",
            "callID": "call_1",
            "tool": "bash",
            "state": { "status": "completed", "input": { "command": "ls" }, "output": "ok", "title": "ls" }
        }))
        .expect("a tool part");
        let Part::Tool(tool) = part else {
            panic!("expected a tool part");
        };
        assert_eq!(tool.call_id, "call_1");
        assert_eq!(tool.tool, "bash");
        assert_eq!(tool.state.status(), "completed");
        assert_eq!(tool.state.title(), Some("ls"));
        assert_eq!(tool.state.output(), Some("ok"));
    }

    #[test]
    fn unknown_part_and_state_types_degrade_instead_of_failing() {
        let part: Part = serde_json::from_value(serde_json::json!({
            "type": "snapshot",
            "id": "prt_2",
            "sessionID": "ses_1",
            "messageID": "msg_1",
            "snapshot": "abc"
        }))
        .expect("an unknown part still parses");
        assert!(matches!(part, Part::Other));

        let state: ToolState = serde_json::from_value(serde_json::json!({
            "status": "some-future-status"
        }))
        .expect("an unknown state still parses");
        assert!(matches!(state, ToolState::Unknown));
    }
}
