//! Map a Morpheus harness mode onto an ACP agent's advertised session mode.
//!
//! The ACP contract fixes no mode spellings: Claude publishes `code`/`plan`,
//! OpenCode `build`/`plan`, and others publish nothing at all. Matching is
//! therefore a documented similarity rule — the first advertised mode whose
//! normalized name (falling back to its description) carries one of the
//! mode's stems. An unmapped mode binds nothing and the agent keeps its own
//! current mode.
//!
//! This lives outside the TUI so every surface (the `/agents` split and the
//! WhatsApp interface) maps modes with the SAME rule.

use morpheus_agents::events::AgentSessionMode;

use super::Mode;

struct ModeMatch {
    mode: Mode,
    /// Substring stems matched against the agent's normalized mode label.
    stems: &'static [&'static str],
}

const MODE_MATCHES: &[ModeMatch] = &[
    // Build: the working mode — write/edit tools at full effect.
    ModeMatch {
        mode: Mode::Build,
        stems: &["build", "code", "write", "edit", "agent", "work"],
    },
    // Ask: read-and-reason, no modifications — most harnesses call this
    // "plan" (plan mode is read-only analysis before the work).
    ModeMatch {
        mode: Mode::Ask,
        stems: &["plan", "ask", "read", "discuss", "chat"],
    },
    // Yolo is NOT mapped deliberately: the client already auto-approves all
    // permission requests in that mode, so widening the agent's own mode
    // would add nothing.
];

/// Map `mode` onto one of the agent's advertised session modes.
///
/// Returns the ACP mode id to bind through `session/set_mode`, or `None` when
/// nothing matches (an unmapped mode, `Mode::Yolo`/`Mode::Command`/
/// `Mode::Manager`, or an agent that advertises no modes).
#[must_use]
pub fn map_mode_to_agent(mode: Mode, modes: &[AgentSessionMode]) -> Option<String> {
    let entry = MODE_MATCHES.iter().find(|entry| entry.mode == mode)?;
    let normalize =
        |text: &str| -> String { text.trim().to_lowercase().replace(['-', ' ', '.'], "_") };
    modes
        .iter()
        .find(|agent_mode| {
            let name = normalize(&agent_mode.name);
            let description = agent_mode
                .description
                .as_deref()
                .map(normalize)
                .unwrap_or_default();
            entry
                .stems
                .iter()
                .any(|stem| name.contains(stem) || description.contains(stem))
        })
        .map(|agent_mode| agent_mode.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(id: &str, name: &str) -> AgentSessionMode {
        AgentSessionMode {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
        }
    }

    #[test]
    fn build_and_ask_map_to_the_closest_advertised_label() {
        let modes = vec![mode("code", "Code"), mode("plan", "Plan")];
        assert_eq!(
            map_mode_to_agent(Mode::Build, &modes).as_deref(),
            Some("code")
        );
        assert_eq!(
            map_mode_to_agent(Mode::Ask, &modes).as_deref(),
            Some("plan")
        );
    }

    #[test]
    fn descriptions_are_matched_when_the_name_does_not_carry_a_stem() {
        let modes = vec![AgentSessionMode {
            id: "readonly".into(),
            name: "Restricted".into(),
            description: Some("read-only analysis".into()),
        }];
        assert_eq!(
            map_mode_to_agent(Mode::Ask, &modes).as_deref(),
            Some("readonly")
        );
    }

    #[test]
    fn unmapped_modes_and_empty_lists_bind_nothing() {
        let modes = vec![mode("code", "Code")];
        assert_eq!(map_mode_to_agent(Mode::Yolo, &modes), None);
        assert_eq!(map_mode_to_agent(Mode::Command, &modes), None);
        assert_eq!(map_mode_to_agent(Mode::Build, &[]), None);
    }
}
