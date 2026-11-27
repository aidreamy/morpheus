//! Read-only capabilities used by an orchestrator before choosing an agent.

use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentInventory {
    /// Server-advertised tool IDs. Per-session permissions still apply.
    pub tools: Option<Vec<String>>,
    /// Server-advertised skill names.
    pub skills: Option<Vec<String>>,
    /// Supported non-text ACP prompt inputs.
    pub inputs: Vec<&'static str>,
    pub resume: Option<bool>,
}

/// Query the selected transport without running a turn or opening a session.
pub async fn probe(agent: &str, cwd: &Path, model: Option<&str>) -> Result<AgentInventory, String> {
    if crate::opencode::is_native(agent) {
        crate::opencode::inventory(agent, cwd, model).await
    } else {
        crate::acp::probe_inventory(agent).await
    }
}

/// The inventory and orchestration prompt need names only. Keep remote
/// descriptions, schemas and skill bodies out of the manager's context.
pub fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value.bytes().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b':' | b'/' | b'#' | b'@')
        })
}
