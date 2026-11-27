//! Mapping from OpenCode server events to the crate's typed stream.
//!
//! The server streams everything from every session on one endpoint, and its
//! event union grows with every release - so the raw frame arrives here as
//! [`serde_json::Value`] and every field is read defensively. The mapper is
//! *stateful* on purpose:
//!
//! - `message.updated` establishes each message's ROLE, which the part
//!   events do not carry; the user's own prompt echo is therefore filtered
//!   out of a live turn instead of appearing as an assistant message.
//! - text fragments arrive as full snapshots (with an optional `delta`
//!   hint), so the mapper remembers how much of each part it already
//!   emitted and forwards only the growth.
//! - a tool call is announced once (`ToolCall`) and patched afterwards
//!   (`ToolCallUpdate`), matching what the ACP path emits. File edits carry
//!   their unified diff in the tool's `metadata`, which is turned into the
//!   same [`ToolDiffSummary`] the ACP path produces.
//!
//! Signals travel separately from display content: a frame can both add
//! timeline content and end the turn (an error), refresh the title, or
//! report a retry.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::api::{self, Part, ToolState};
use crate::events::{
    AgentPermissionOption, PlanEntry, PlanEntryPriority, PlanEntryStatus, SubagentEvent,
    ToolCallStatus, ToolDiffSummary, ToolKind, ToolOutputBlock,
};

/// A permission request the turn must answer before the agent continues.
#[derive(Debug, Clone)]
pub struct PermissionNotice {
    pub id: String,
    /// Human-readable summary of what is being asked.
    pub title: String,
    /// The tool category behind the request.
    pub kind: ToolKind,
    /// Raw request metadata for the TUI's detail rows.
    pub raw_input: Option<Value>,
    legacy: bool,
}

fn is_idle_event(event: &Value, session: &str) -> bool {
    let properties = &event["properties"];
    properties["sessionID"].as_str() == Some(session)
        && match event["type"].as_str() {
            Some("session.idle") => true,
            Some("session.status") => properties["status"]["type"] == "idle",
            _ => false,
        }
}

impl PermissionNotice {
    pub(super) fn reply(&self, session: &str, answer: &str) -> (String, Value) {
        if self.legacy {
            (
                format!("/session/{session}/permissions/{}", self.id),
                serde_json::json!({ "response": answer }),
            )
        } else {
            (
                format!("/permission/{}/reply", self.id),
                serde_json::json!({ "reply": answer }),
            )
        }
    }
}

/// Token accounting of one assistant message, as reported by the server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageInfo {
    pub provider: String,
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total: u64,
}

impl UsageInfo {
    /// Tokens occupying the context window: the prompt side (uncached plus
    /// cache reads/writes) plus the completion.
    #[must_use]
    pub fn in_context(&self) -> u64 {
        self.input
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
            .saturating_add(self.output)
    }
}

/// What a mapped event means for the turn loop, beyond display content.
#[derive(Debug)]
pub enum Signal {
    /// The session went idle: the turn is over.
    Idle,
    /// The server reported an error for this session.
    Error(String),
    /// A tool call awaits a permission decision.
    Permission(PermissionNotice),
    /// The assistant message reported its token accounting.
    Usage(UsageInfo),
    /// The provider is being retried after a transient failure.
    Retry { attempt: u64, message: String },
    /// The session's title was set or changed.
    Title(String),
}

/// Scroll one server event into display events plus turn signals.
pub struct Mapper {
    /// Whether user message text is emitted (the history replay wants it; a
    /// live turn already shows the caller's own message).
    emit_user: bool,
    /// `messageID` → role (`user` / `assistant`).
    roles: HashMap<String, String>,
    /// `part id` → the text already emitted for it (snapshots grow).
    emitted: HashMap<String, String>,
    /// Tool call ids already announced as a `ToolCall`.
    announced_tools: HashSet<String>,
}

impl Mapper {
    #[must_use]
    pub fn new(emit_user: bool) -> Self {
        Self {
            emit_user,
            roles: HashMap::new(),
            emitted: HashMap::new(),
            announced_tools: HashSet::new(),
        }
    }

    /// Map one event belonging to `session`.
    ///
    /// Events from every other session (the stream is server-wide) are
    /// dropped without touching the mapper's state.
    pub fn map(&mut self, event: &Value, session: &str) -> (Vec<SubagentEvent>, Vec<Signal>) {
        if is_idle_event(event, session) {
            return (Vec::new(), vec![Signal::Idle]);
        }
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let properties = event.get("properties").cloned().unwrap_or(Value::Null);
        match kind {
            "message.updated" => {
                let Some(info) = decode::<api::MessageInfo>(&properties, "info") else {
                    return (Vec::new(), Vec::new());
                };
                if !session.is_empty() && !info.session_id.is_empty() && info.session_id != session
                {
                    return (Vec::new(), Vec::new());
                }
                (Vec::new(), self.observe_message(&info))
            }
            "message.part.updated" => {
                let Some(part) = properties.get("part") else {
                    return (Vec::new(), Vec::new());
                };
                let Ok(part) = serde_json::from_value::<Part>(part.clone()) else {
                    return (Vec::new(), Vec::new());
                };
                if !session.is_empty()
                    && !part.session_id().is_empty()
                    && part.session_id() != session
                {
                    return (Vec::new(), Vec::new());
                }
                let delta = properties
                    .get("delta")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                (self.observe_part(&part, delta.as_deref()), Vec::new())
            }
            "todo.updated" => {
                if !self.belongs(&properties, session) {
                    return (Vec::new(), Vec::new());
                }
                let entries = plan_entries(&properties);
                let events = if entries.is_empty() {
                    Vec::new()
                } else {
                    vec![SubagentEvent::Plan { entries }]
                };
                (events, Vec::new())
            }
            "permission.asked" | "permission.updated" => {
                let Some(permission) = decode::<api::Permission>(&properties, "") else {
                    return (Vec::new(), Vec::new());
                };
                if permission.id.is_empty() || permission.session_id != session {
                    return (Vec::new(), Vec::new());
                }
                let title = if !permission.title.is_empty() {
                    permission.title
                } else if permission.patterns.is_empty() {
                    permission.kind.clone()
                } else {
                    format!("{}: {}", permission.kind, permission.patterns.join(", "))
                };
                let notice = PermissionNotice {
                    id: permission.id,
                    title,
                    kind: kind_from_tool(&permission.kind),
                    raw_input: Some(permission.metadata).filter(|value| !value.is_null()),
                    legacy: kind == "permission.updated",
                };
                (Vec::new(), vec![Signal::Permission(notice)])
            }
            "session.updated" => {
                let Some(info) = decode::<api::Session>(&properties, "info") else {
                    return (Vec::new(), Vec::new());
                };
                if !self.belongs_session(&info.id, session) || info.title.is_empty() {
                    return (Vec::new(), Vec::new());
                }
                (Vec::new(), vec![Signal::Title(info.title)])
            }
            "session.status" => {
                if !self.belongs(&properties, session) {
                    return (Vec::new(), Vec::new());
                }
                let status = properties.get("status").cloned().unwrap_or(Value::Null);
                match status.get("type").and_then(Value::as_str) {
                    Some("retry") => {
                        let attempt = status.get("attempt").and_then(Value::as_u64).unwrap_or(0);
                        let message = status
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("the provider failed")
                            .to_string();
                        (Vec::new(), vec![Signal::Retry { attempt, message }])
                    }
                    _ => (Vec::new(), Vec::new()),
                }
            }
            "session.error" => {
                if !self.belongs(&properties, session) {
                    return (Vec::new(), Vec::new());
                }
                let message = properties
                    .get("error")
                    .and_then(|error| {
                        serde_json::from_value::<api::MessageError>(error.clone()).ok()
                    })
                    .map_or_else(
                        || "the OpenCode session failed".to_string(),
                        |error| error.message(),
                    );
                (Vec::new(), vec![Signal::Error(message)])
            }
            _ => (Vec::new(), Vec::new()),
        }
    }

    /// Register a message's role and surface what it reports: a provider
    /// error (assistant messages) and the token accounting of a REAL
    /// assistant turn (the pre-turn placeholder reports all zeros).
    ///
    /// Used by the live event path and by the history replay, which reads
    /// messages from the REST endpoint instead of the stream.
    pub fn observe_message(&mut self, info: &api::MessageInfo) -> Vec<Signal> {
        if !info.id.is_empty() {
            self.roles.insert(info.id.clone(), info.role.clone());
        }
        let mut signals = Vec::new();
        if info.is_assistant() {
            if let Some(error) = &info.error {
                signals.push(Signal::Error(error.message()));
            }
            if let Some(tokens) = &info.tokens
                && tokens.reported()
            {
                signals.push(Signal::Usage(UsageInfo {
                    provider: info.provider_id.clone(),
                    model: info.model_id.clone(),
                    input: tokens.input,
                    output: tokens.output,
                    reasoning: tokens.reasoning,
                    cache_read: tokens.cache.read,
                    cache_write: tokens.cache.write,
                    total: tokens.total,
                }));
            }
        }
        signals
    }

    /// Scroll one message part into display events.
    ///
    /// `delta` is the stream's own growth hint, when the frame carries one;
    /// the REST history has no such hint and relies on snapshot diffing.
    pub fn observe_part(&mut self, part: &Part, delta: Option<&str>) -> Vec<SubagentEvent> {
        match part {
            Part::Text(text) => {
                let role = self
                    .roles
                    .get(&text.message_id)
                    .map(String::as_str)
                    .unwrap_or("assistant");
                if role == "user" {
                    if !self.emit_user || text.synthetic || text.ignored {
                        return Vec::new();
                    }
                    return vec![SubagentEvent::UserMessage {
                        text: text.text.clone(),
                    }];
                }
                self.growth(&text.id, &text.text, delta)
                    .map(|text| vec![SubagentEvent::Message { text }])
                    .unwrap_or_default()
            }
            Part::Reasoning(reasoning) => self
                .growth(&reasoning.id, &reasoning.text, delta)
                .map(|text| vec![SubagentEvent::Thought { text }])
                .unwrap_or_default(),
            Part::Tool(tool) => {
                let state = &tool.state;
                let id = if tool.call_id.is_empty() {
                    tool.id.clone()
                } else {
                    tool.call_id.clone()
                };
                let status = tool_status(state);
                if self.announced_tools.insert(id.clone()) {
                    let title = state
                        .title()
                        .filter(|title| !title.is_empty())
                        .map_or_else(|| tool.tool.clone(), str::to_string);
                    return vec![SubagentEvent::ToolCall {
                        id,
                        title,
                        kind: kind_from_tool(&tool.tool),
                        status,
                        raw_input: state.input().filter(|input| !input.is_null()).cloned(),
                        tool: Some(tool.tool.clone()).filter(|tool| !tool.is_empty()),
                    }];
                }
                vec![SubagentEvent::ToolCallUpdate {
                    id,
                    status: Some(status),
                    title: state.title().map(str::to_string),
                    raw_output: state
                        .output()
                        .map(|output| Value::String(output.to_string())),
                    content: ToolOutputBlock {
                        text: state.output().unwrap_or_default().to_string(),
                        skipped: 0,
                        diff: tool_diff(tool, state),
                    },
                }]
            }
            Part::Other => Vec::new(),
        }
    }

    /// The not-yet-emitted growth of a snapshot-style part.
    ///
    /// Prefers the server's own `delta`; otherwise diffs against the last
    /// text seen for that part. A part whose text no longer extends the
    /// previous snapshot (a replacement) is emitted whole.
    fn growth(&mut self, part_id: &str, full: &str, delta: Option<&str>) -> Option<String> {
        let previous = self.emitted.insert(part_id.to_string(), full.to_string());
        let text = match delta {
            Some(delta) if !delta.is_empty() => delta.to_string(),
            _ => match previous {
                Some(previous) if full.starts_with(&previous) => full[previous.len()..].to_string(),
                Some(_) => full.to_string(),
                None => full.to_string(),
            },
        };
        if text.is_empty() { None } else { Some(text) }
    }

    fn belongs(&self, properties: &Value, session: &str) -> bool {
        if session.is_empty() {
            return true;
        }
        match properties.get("sessionID").and_then(Value::as_str) {
            Some(id) => id == session,
            None => true,
        }
    }

    fn belongs_session(&self, id: &str, session: &str) -> bool {
        session.is_empty() || id.is_empty() || id == session
    }
}

/// One todo item as a plan entry (shared by the live `todo.updated` frame
/// and the REST todo endpoint that seeds a resumed session).
#[must_use]
pub fn plan_entry(todo: &api::Todo) -> PlanEntry {
    PlanEntry {
        content: todo.content.clone(),
        priority: plan_priority(&todo.priority),
        status: plan_status(&todo.status),
    }
}

/// The plan a `todo.updated` frame carries.
#[must_use]
pub fn plan_entries(properties: &Value) -> Vec<PlanEntry> {
    properties
        .get("todos")
        .and_then(Value::as_array)
        .map(|todos| {
            todos
                .iter()
                .filter_map(|todo| serde_json::from_value::<api::Todo>(todo.clone()).ok())
                .map(|todo| plan_entry(&todo))
                .collect()
        })
        .unwrap_or_default()
}

/// The unified diff a file-editing tool reported in its metadata.
///
/// OpenCode's edit/write tools publish `metadata.filediff` (path, patch,
/// additions, deletions) plus `metadata.diff` with the same patch wrapped in
/// a `git diff`-style header. The panel only renders a diff that starts at
/// `--- ` (or `diff --git `), so the header is normalized here.
fn tool_diff(tool: &api::ToolPart, state: &ToolState) -> Option<ToolDiffSummary> {
    let metadata = state.metadata()?;
    let filediff = metadata
        .get("filediff")
        .or_else(|| metadata.get("fileDiff"));
    let path = filediff
        .and_then(|filediff| filediff.get("file"))
        .and_then(Value::as_str)
        .or_else(|| {
            state
                .input()
                .and_then(|input| input.get("filePath"))
                .and_then(Value::as_str)
        })?;
    let raw = metadata
        .get("diff")
        .and_then(Value::as_str)
        .or_else(|| {
            filediff
                .and_then(|filediff| filediff.get("patch"))
                .and_then(Value::as_str)
        })
        .map(normalize_unified)
        .filter(|diff| !diff.is_empty());
    let (counted_added, counted_removed) = raw.as_deref().map_or((0, 0), diff_counts);
    let added = filediff
        .and_then(|filediff| filediff.get("additions"))
        .and_then(Value::as_u64)
        .map_or(counted_added, |value| {
            u32::try_from(value).unwrap_or(u32::MAX)
        });
    let removed = filediff
        .and_then(|filediff| filediff.get("deletions"))
        .and_then(Value::as_u64)
        .map_or(counted_removed, |value| {
            u32::try_from(value).unwrap_or(u32::MAX)
        });
    if raw.is_none() && added == 0 && removed == 0 {
        return None;
    }
    log::trace!(
        "opencode: diff for tool '{}' -> {} (+{added} -{removed})",
        tool.tool,
        path
    );
    Some(ToolDiffSummary {
        path: path.to_string(),
        added,
        removed,
        unified: raw,
    })
}

/// Drop the `Index:`/`====` preamble a `git diff`-style patch carries, so
/// the panel sees the `--- ` header it requires.
fn normalize_unified(raw: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    let mut started = false;
    for line in raw.lines() {
        if !started {
            if line.starts_with("--- ") || line.starts_with("diff --git ") {
                started = true;
            } else {
                continue;
            }
        }
        lines.push(line);
    }
    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Added/removed line counts from a unified diff (headers excluded).
fn diff_counts(diff: &str) -> (u32, u32) {
    let mut added = 0_u32;
    let mut removed = 0_u32;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            added = added.saturating_add(1);
        } else if line.starts_with('-') {
            removed = removed.saturating_add(1);
        }
    }
    (added, removed)
}

fn decode<T: serde::de::DeserializeOwned>(properties: &Value, key: &str) -> Option<T> {
    let value = if key.is_empty() {
        properties.clone()
    } else {
        properties.get(key)?.clone()
    };
    serde_json::from_value(value).ok()
}

fn tool_status(state: &ToolState) -> ToolCallStatus {
    match state.status() {
        "pending" => ToolCallStatus::Pending,
        "running" => ToolCallStatus::InProgress,
        "completed" => ToolCallStatus::Completed,
        "error" => ToolCallStatus::Failed,
        _ => ToolCallStatus::Unknown,
    }
}

/// OpenCode tool names are its own (`bash`, `edit`, `grep`, `webfetch`, …);
/// map the families onto the protocol's tool kinds the TUI already paints.
fn kind_from_tool(tool: &str) -> ToolKind {
    let tool = tool.to_ascii_lowercase();
    if tool.contains("edit") || tool.contains("write") || tool.contains("patch") {
        ToolKind::Edit
    } else if tool.contains("read") || tool == "cat" {
        ToolKind::Read
    } else if tool.contains("grep")
        || tool.contains("glob")
        || tool.contains("list")
        || tool.contains("search")
        || tool.contains("find")
    {
        ToolKind::Search
    } else if tool.contains("bash") || tool.contains("shell") || tool.contains("exec") {
        ToolKind::Execute
    } else if tool.contains("fetch") || tool.contains("http") {
        ToolKind::Fetch
    } else if tool.contains("todo") || tool.contains("think") || tool.contains("task") {
        ToolKind::Think
    } else {
        ToolKind::Unknown
    }
}

fn plan_status(status: &str) -> PlanEntryStatus {
    match status {
        "pending" => PlanEntryStatus::Pending,
        "in_progress" | "in-progress" => PlanEntryStatus::InProgress,
        "completed" => PlanEntryStatus::Completed,
        _ => PlanEntryStatus::Unknown,
    }
}

fn plan_priority(priority: &str) -> PlanEntryPriority {
    match priority {
        "high" => PlanEntryPriority::High,
        "medium" => PlanEntryPriority::Medium,
        "low" => PlanEntryPriority::Low,
        _ => PlanEntryPriority::Unknown,
    }
}

/// The options a permission request offers, in the server's own answer
/// vocabulary: allow once, allow always, reject.
#[must_use]
pub fn permission_options() -> Vec<AgentPermissionOption> {
    vec![
        AgentPermissionOption {
            id: api::answer::ONCE.to_string(),
            name: "Allow once".to_string(),
            kind: "allow_once".to_string(),
        },
        AgentPermissionOption {
            id: api::answer::ALWAYS.to_string(),
            name: "Allow always".to_string(),
            kind: "allow_always".to_string(),
        },
        AgentPermissionOption {
            id: api::answer::REJECT.to_string(),
            name: "Reject".to_string(),
            kind: "reject_once".to_string(),
        },
    ]
}

/// Normalize a pane answer into one of the server's answer values.
///
/// The pane sends back the option id this module published; the ACP-style
/// spellings (`allow_once`, …) are accepted too, and a dismissed box becomes
/// a rejection.
#[must_use]
pub fn permission_answer(answer: Option<&crate::events::PanePermissionAnswer>) -> &'static str {
    match answer {
        Some(crate::events::PanePermissionAnswer::Selected(id)) => match id.as_str() {
            api::answer::ONCE | "allow_once" => api::answer::ONCE,
            api::answer::ALWAYS | "allow_always" => api::answer::ALWAYS,
            _ => api::answer::REJECT,
        },
        Some(crate::events::PanePermissionAnswer::Cancel) | None => api::answer::REJECT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn update(part: Value) -> Value {
        json!({ "type": "message.part.updated", "properties": { "part": part } })
    }

    fn text_part(id: &str, text: &str) -> Value {
        json!({
            "type": "text",
            "id": id,
            "sessionID": "ses_1",
            "messageID": "msg_a",
            "text": text
        })
    }

    fn assistant_message(id: &str) -> Value {
        json!({
            "type": "message.updated",
            "properties": { "info": { "id": id, "role": "assistant", "sessionID": "ses_1" } }
        })
    }

    fn register_assistant(mapper: &mut Mapper) {
        mapper.map(&assistant_message("msg_a"), "ses_1");
    }

    #[test]
    fn only_the_growth_of_a_text_snapshot_is_emitted() {
        let mut mapper = Mapper::new(false);
        register_assistant(&mut mapper);
        let (events, _) = mapper.map(&update(text_part("prt_1", "Hello")), "ses_1");
        assert_eq!(
            events,
            vec![SubagentEvent::Message {
                text: "Hello".to_string()
            }]
        );
        let (events, _) = mapper.map(&update(text_part("prt_1", "Hello world")), "ses_1");
        assert_eq!(
            events,
            vec![SubagentEvent::Message {
                text: " world".to_string()
            }]
        );
        // A repeated full snapshot adds nothing.
        let (events, _) = mapper.map(&update(text_part("prt_1", "Hello world")), "ses_1");
        assert!(events.is_empty());
    }

    #[test]
    fn the_server_delta_wins_when_present() {
        let mut mapper = Mapper::new(false);
        register_assistant(&mut mapper);
        let mut event = update(text_part("prt_1", "Hello world"));
        event["properties"]["delta"] = json!(" world");
        let (events, _) = mapper.map(&event, "ses_1");
        assert_eq!(
            events,
            vec![SubagentEvent::Message {
                text: " world".to_string()
            }]
        );
    }

    #[test]
    fn a_live_turn_filters_the_users_own_prompt_echo() {
        let mut mapper = Mapper::new(false);
        let event = json!({
            "type": "message.updated",
            "properties": { "info": { "id": "msg_u", "role": "user" } }
        });
        mapper.map(&event, "ses_1");
        let part = json!({
            "type": "text", "id": "prt_u", "sessionID": "ses_1",
            "messageID": "msg_u", "text": "do the thing"
        });
        let (events, _) = mapper.map(&update(part), "ses_1");
        assert!(events.is_empty());
    }

    #[test]
    fn the_replay_pass_keeps_the_user_side() {
        let mut mapper = Mapper::new(true);
        let event = json!({
            "type": "message.updated",
            "properties": { "info": { "id": "msg_u", "role": "user" } }
        });
        mapper.map(&event, "ses_1");
        let part = json!({
            "type": "text", "id": "prt_u", "sessionID": "ses_1",
            "messageID": "msg_u", "text": "do the thing"
        });
        let (events, _) = mapper.map(&update(part), "ses_1");
        assert_eq!(
            events,
            vec![SubagentEvent::UserMessage {
                text: "do the thing".to_string()
            }]
        );
    }

    #[test]
    fn a_tool_call_is_announced_once_and_then_patched() {
        let mut mapper = Mapper::new(false);
        register_assistant(&mut mapper);
        let running = json!({
            "type": "tool", "id": "prt_t", "sessionID": "ses_1", "messageID": "msg_a",
            "callID": "call_1", "tool": "bash",
            "state": { "status": "running", "input": { "command": "ls" }, "title": "ls" }
        });
        let (events, _) = mapper.map(&update(running), "ses_1");
        assert!(matches!(
            &events[0],
            SubagentEvent::ToolCall { id, kind: ToolKind::Execute, status: ToolCallStatus::InProgress, tool: Some(tool), .. }
                if id == "call_1" && tool == "bash"
        ));

        let completed = json!({
            "type": "tool", "id": "prt_t", "sessionID": "ses_1", "messageID": "msg_a",
            "callID": "call_1", "tool": "bash",
            "state": { "status": "completed", "input": { "command": "ls" }, "output": "a.txt", "title": "ls" }
        });
        let (events, _) = mapper.map(&update(completed), "ses_1");
        let SubagentEvent::ToolCallUpdate {
            status, content, ..
        } = &events[0]
        else {
            panic!("expected a tool update");
        };
        assert_eq!(*status, Some(ToolCallStatus::Completed));
        assert_eq!(content.text, "a.txt");
        assert!(content.diff.is_none());
    }

    /// The real shape measured on OpenCode 1.19.6: `metadata.diff` wrapped in
    /// a `git diff`-style header, `metadata.filediff` with the counts.
    #[test]
    fn an_edit_tool_carries_its_diff() {
        let mut mapper = Mapper::new(false);
        register_assistant(&mut mapper);
        let part = json!({
            "type": "tool", "id": "prt_e", "sessionID": "ses_1", "messageID": "msg_a",
            "callID": "call_e", "tool": "edit",
            "state": {
                "status": "completed",
                "input": { "filePath": "/w/hello.txt" },
                "output": "Edit applied successfully.",
                "title": "hello.txt",
                "metadata": {
                    "diff": "Index: /w/hello.txt\n===================================================================\n--- /w/hello.txt\n+++ /w/hello.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+gamma\n",
                    "filediff": {
                        "file": "/w/hello.txt",
                        "patch": "--- /w/hello.txt\n+++ /w/hello.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+gamma\n",
                        "additions": 1,
                        "deletions": 1
                    }
                }
            }
        });
        let (events, _) = mapper.map(&update(part), "ses_1");
        let SubagentEvent::ToolCall { kind, .. } = &events[0] else {
            panic!("expected a tool call");
        };
        assert_eq!(*kind, ToolKind::Edit);

        let part = json!({
            "type": "tool", "id": "prt_e", "sessionID": "ses_1", "messageID": "msg_a",
            "callID": "call_e", "tool": "edit",
            "state": {
                "status": "completed",
                "input": { "filePath": "/w/hello.txt" },
                "output": "Edit applied successfully.",
                "title": "hello.txt",
                "metadata": {
                    "diff": "Index: /w/hello.txt\n===================================================================\n--- /w/hello.txt\n+++ /w/hello.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+gamma\n",
                    "filediff": {
                        "file": "/w/hello.txt",
                        "patch": "--- /w/hello.txt\n+++ /w/hello.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+gamma\n",
                        "additions": 1,
                        "deletions": 1
                    }
                }
            }
        });
        let (events, _) = mapper.map(&update(part), "ses_1");
        let SubagentEvent::ToolCallUpdate { content, .. } = &events[0] else {
            panic!("expected a tool update");
        };
        let diff = content.diff.as_ref().expect("a diff summary");
        assert_eq!(diff.path, "/w/hello.txt");
        assert_eq!(diff.added, 1);
        assert_eq!(diff.removed, 1);
        let unified = diff.unified.as_deref().expect("a unified diff");
        assert!(unified.starts_with("--- "), "{unified}");
        assert!(unified.contains("-beta"));
        assert!(unified.contains("+gamma"));
    }

    #[test]
    fn events_from_other_sessions_are_dropped() {
        let mut mapper = Mapper::new(true);
        let mut event = update(text_part("prt_1", "other"));
        event["properties"]["part"]["sessionID"] = json!("ses_2");
        let (events, signals) = mapper.map(&event, "ses_1");
        assert!(events.is_empty());
        assert!(signals.is_empty());

        let idle = json!({
            "type": "session.idle",
            "properties": { "sessionID": "ses_2" }
        });
        let (_, signals) = mapper.map(&idle, "ses_1");
        assert!(signals.is_empty());
    }

    #[test]
    fn idle_and_errors_are_signalled_for_this_session() {
        let mut mapper = Mapper::new(false);
        let idle = json!({ "type": "session.idle", "properties": { "sessionID": "ses_1" } });
        assert!(matches!(
            mapper.map(&idle, "ses_1").1.as_slice(),
            [Signal::Idle]
        ));

        let error = json!({
            "type": "session.error",
            "properties": {
                "sessionID": "ses_1",
                "error": { "name": "ProviderAuthError", "data": { "message": "missing key" } }
            }
        });
        match mapper.map(&error, "ses_1").1.as_slice() {
            [Signal::Error(message)] => {
                assert_eq!(message, "ProviderAuthError: missing key");
            }
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn token_accounting_and_retries_are_signalled() {
        let mut mapper = Mapper::new(false);
        let usage = json!({
            "type": "message.updated",
            "properties": { "info": {
                "id": "msg_a", "role": "assistant", "sessionID": "ses_1",
                "providerID": "opencode", "modelID": "iceman-free",
                "tokens": { "total": 7504, "input": 7359, "output": 52, "reasoning": 93,
                            "cache": { "read": 0, "write": 0 } }
            } }
        });
        match mapper.map(&usage, "ses_1").1.as_slice() {
            [Signal::Usage(info)] => {
                assert_eq!(info.provider, "opencode");
                assert_eq!(info.model, "iceman-free");
                assert_eq!(info.in_context(), 7411);
            }
            other => panic!("expected usage, got {other:?}"),
        }

        // The pre-turn placeholder (all zeros) reports nothing.
        let placeholder = json!({
            "type": "message.updated",
            "properties": { "info": {
                "id": "msg_b", "role": "assistant", "sessionID": "ses_1",
                "tokens": { "input": 0, "output": 0, "reasoning": 0,
                            "cache": { "read": 0, "write": 0 } }
            } }
        });
        assert!(mapper.map(&placeholder, "ses_1").1.is_empty());

        let retry = json!({
            "type": "session.status",
            "properties": {
                "sessionID": "ses_1",
                "status": { "type": "retry", "attempt": 2, "message": "rate limited", "next": 1 }
            }
        });
        match mapper.map(&retry, "ses_1").1.as_slice() {
            [Signal::Retry { attempt, message }] => {
                assert_eq!(*attempt, 2);
                assert_eq!(message, "rate limited");
            }
            other => panic!("expected a retry, got {other:?}"),
        }
    }

    #[test]
    fn the_session_title_is_signalled() {
        let mut mapper = Mapper::new(false);
        let event = json!({
            "type": "session.updated",
            "properties": { "info": { "id": "ses_1", "title": "diff-probe" } }
        });
        assert!(matches!(
            mapper.map(&event, "ses_1").1.as_slice(),
            [Signal::Title(title)] if title == "diff-probe"
        ));
        let other = json!({
            "type": "session.updated",
            "properties": { "info": { "id": "ses_2", "title": "other" } }
        });
        assert!(mapper.map(&other, "ses_1").1.is_empty());
    }

    #[test]
    fn a_dropped_header_still_normalizes_to_a_panel_diff() {
        assert_eq!(
            normalize_unified("Index: x\n===\n--- a\n+++ b\n"),
            "--- a\n+++ b\n"
        );
        assert_eq!(normalize_unified("no diff here"), "");
        assert_eq!(diff_counts("--- a\n+++ b\n-old\n+new\n+more\n"), (2, 1));
    }

    #[test]
    fn the_todo_list_becomes_the_session_plan() {
        let mut mapper = Mapper::new(false);
        let event = json!({
            "type": "todo.updated",
            "properties": {
                "sessionID": "ses_1",
                "todos": [
                    { "content": "read the crate", "status": "completed", "priority": "high" },
                    { "content": "port the mapper", "status": "in_progress", "priority": "medium" }
                ]
            }
        });
        let (events, _) = mapper.map(&event, "ses_1");
        let SubagentEvent::Plan { entries } = &events[0] else {
            panic!("expected a plan");
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].content, "read the crate");
        assert_eq!(entries[0].status, PlanEntryStatus::Completed);
        assert_eq!(entries[0].priority, PlanEntryPriority::High);
        assert_eq!(entries[1].status, PlanEntryStatus::InProgress);
        assert_eq!(entries[1].priority, PlanEntryPriority::Medium);
    }

    #[test]
    fn permissions_carry_their_own_answer_vocabulary() {
        let mut mapper = Mapper::new(false);
        let event = json!({
            "type": "permission.updated",
            "properties": {
                "id": "per_1", "type": "bash", "sessionID": "ses_1",
                "title": "Run `rm -rf build`", "metadata": { "command": "rm -rf build" }
            }
        });
        let (_, signals) = mapper.map(&event, "ses_1");
        let [Signal::Permission(notice)] = signals.as_slice() else {
            panic!("expected a permission request");
        };
        assert_eq!(notice.id, "per_1");
        assert_eq!(notice.kind, ToolKind::Execute);
        assert_eq!(
            notice.reply("ses_1", "always"),
            (
                "/session/ses_1/permissions/per_1".into(),
                json!({ "response": "always" })
            )
        );
        assert_eq!(
            permission_options()
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            vec!["once", "always", "reject"]
        );

        use crate::events::PanePermissionAnswer;
        assert_eq!(
            permission_answer(Some(&PanePermissionAnswer::Selected("always".into()))),
            "always"
        );
        assert_eq!(
            permission_answer(Some(&PanePermissionAnswer::Cancel)),
            "reject"
        );
        assert_eq!(permission_answer(None), "reject");
    }
}
