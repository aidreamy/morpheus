//! The external-agent roster and the addressing grammar.
//!
//! `/agent` is the only way an ACP agent becomes reachable from a chat. This
//! module is pure on purpose: it knows nothing about processes, sessions or
//! the transport, so every rule below is unit-tested without a network or an
//! installed harness.
//!
//! The roster never contains `morpheus`: the main agent is always available
//! without a prefix, so selecting it would only create a second, confusing way
//! to reach the same loop. The cap keeps one chat's fan-out (`@all`) bounded.
//! Repeating a name (`claude,claude`) creates instances: `claude#1` and
//! `claude#2`, addressed with `@claude#2`.

use morpheus_agents::palette::display_name;

/// How many external agent instances one chat may select.
pub const MAX_ROSTER: usize = 4;

/// One selected instance: the agent id plus its 1-based position among the
/// instances of that agent in the roster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterEntry {
    pub agent: String,
    pub index: usize,
    pub total: usize,
}

/// Expand a roster into its instances, in selection order.
#[must_use]
pub fn entries(roster: &[String]) -> Vec<RosterEntry> {
    roster
        .iter()
        .enumerate()
        .map(|(position, agent)| {
            let total = roster.iter().filter(|name| *name == agent).count();
            let index = roster[..=position]
                .iter()
                .filter(|name| *name == agent)
                .count();
            RosterEntry {
                agent: agent.clone(),
                index,
                total,
            }
        })
        .collect()
}

/// The stable session key for one instance. Always includes the index, so a
/// single selection can later grow into instances without losing its session.
#[must_use]
pub fn instance_key(agent: &str, index: usize) -> String {
    format!("{agent}#{index}")
}

/// The display label of one instance: `claude`, or `claude #2` when the agent
/// has several instances.
#[must_use]
pub fn instance_label(agent: &str, index: usize, total: usize) -> String {
    let name = display_name(agent);
    if total > 1 {
        format!("{name} #{index}")
    } else {
        name.to_string()
    }
}

/// Parse and validate a `/agent` roster expression.
///
/// The grammar is a comma-separated list of canonical agent ids
/// (`claude,codex`). Whitespace around entries and case are tolerated; a
/// repeated name creates another instance. Validation is all-or-nothing: on
/// any error the caller keeps the previous roster.
///
/// # Errors
///
/// Returns a user-facing message for an empty entry, too many entries, the
/// main agent, or an agent that is not installed.
pub fn parse_roster(raw: &str, installed: &[String]) -> Result<Vec<String>, String> {
    if raw.trim().is_empty() {
        return Err("Name at least one agent: `/agent <name>[,<name>...]`.".to_string());
    }

    let mut roster: Vec<String> = Vec::new();
    for (index, entry) in raw.split(',').enumerate() {
        let name = entry.trim().to_ascii_lowercase();
        if name.is_empty() {
            return Err(format!(
                "Entry {} is empty. Use `/agent claude,codex` (no blank entries).",
                index + 1
            ));
        }
        if name == "morpheus" {
            return Err(
                "`morpheus` is the main agent: it is always available without a prefix, so it \
                 cannot be selected."
                    .to_string(),
            );
        }
        if !installed.iter().any(|agent| agent == &name) {
            let available = if installed.is_empty() {
                "No ACP agents are installed.".to_string()
            } else {
                format!("Installed: {}.", installed.join(", "))
            };
            return Err(format!(
                "`{name}` is not installed. {available} Send `/agent ls all` to see every \
                 registered agent."
            ));
        }
        roster.push(name);
    }

    if roster.len() > MAX_ROSTER {
        return Err(format!(
            "`/agent` selects at most {MAX_ROSTER} instances; got {}.",
            roster.len()
        ));
    }
    Ok(roster)
}

/// Clean a roster read from disk.
///
/// Persisted state is user-editable, so loading applies the invariants that do
/// not need the installed set: canonical ids, no blanks, no `morpheus`, and the
/// cap. Duplicates are kept (they are instances). An agent that is not
/// currently installed stays in the roster (installation can be transient);
/// routing reports it unavailable instead of silently forgetting the owner's
/// selection.
#[must_use]
pub fn sanitize_roster(roster: Vec<String>) -> Vec<String> {
    let mut clean: Vec<String> = Vec::new();
    for agent in roster {
        let name = agent.trim().to_ascii_lowercase();
        if name.is_empty() || name == "morpheus" {
            continue;
        }
        if clean.len() == MAX_ROSTER {
            break;
        }
        clean.push(name);
    }
    clean
}

/// One selected instance as `/agent ls` renders it.
///
/// A read-only snapshot: the formatter below never touches the disk, so the
/// caller (which owns the model picks and the session store) resolves the
/// values first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSummary {
    pub agent: String,
    pub index: usize,
    pub total: usize,
    /// The persisted model pick, when the user set one with
    /// `/agent model <name> <model>`.
    pub model: Option<String>,
    /// Whether an ACP session id is stored for this instance.
    pub session: bool,
}

/// The `/agent` and `/agent ls` body.
///
/// Short lines, no tables: this is read on a phone. The example uses the first
/// installed agent so a new user can copy it verbatim.
#[must_use]
pub fn render_list(summaries: &[AgentSummary], installed: &[String]) -> String {
    let mut text = String::from("*External agents*\n\n");
    text.push_str("Main: *morpheus* - always available, no prefix.\n\n");
    text.push_str(&render_summaries(summaries));
    text.push('\n');

    if installed.is_empty() {
        text.push_str("\nInstalled: none. Install an ACP agent and send `/agent ls` again.");
        return text;
    }
    text.push_str(&format!("\nInstalled: {}.\n", installed.join(", ")));
    text.push_str(&format!(
        "\nAdd or replace: `/agent <name>[,<name>...]` (up to {MAX_ROSTER} instances)."
    ));
    if let Some(first) = installed.first() {
        text.push_str(&format!("\nExample: `/agent {first}`"));
    }
    text.push_str("\nRepeat a name to add an instance: `/agent claude,claude`");
    text.push_str("\nTurn them off: `/agent off`");
    text
}

/// The selected instances, one per line, with model and session state.
#[must_use]
pub fn render_summaries(summaries: &[AgentSummary]) -> String {
    if summaries.is_empty() {
        return "Selected: none.".to_string();
    }
    let mut text = String::from("Selected:\n");
    for summary in summaries {
        let model = match summary.model.as_deref() {
            Some(model) => format!("model `{model}`"),
            None => "default model".to_string(),
        };
        let session = if summary.session {
            "session saved"
        } else {
            "new session"
        };
        text.push_str(&format!(
            "• *{}* - {model}, {session}\n",
            instance_label(&summary.agent, summary.index, summary.total)
        ));
    }
    text.trim_end().to_string()
}

/// The `/agent ls all` body: the roster plus every registered agent, marking
/// which ones are installed. Not-installed agents cannot be selected.
#[must_use]
pub fn render_all(
    summaries: &[AgentSummary],
    installed: &[String],
    registered: &[String],
) -> String {
    let mut text = render_list(summaries, installed);
    text.push_str("\n\nRegistered agents:\n");
    if registered.is_empty() {
        text.push_str("• none (the registry is empty)");
        return text;
    }
    for agent in registered {
        let mark = if installed.iter().any(|name| name == agent) {
            "installed"
        } else {
            "not installed"
        };
        text.push_str(&format!("• {agent} - {mark}\n"));
    }
    text.trim_end().to_string()
}

/// The selected instances as `*claude #1*, *claude #2*`, or `none`.
#[must_use]
pub fn render_roster(roster: &[String]) -> String {
    if roster.is_empty() {
        return "none".to_string();
    }
    entries(roster)
        .iter()
        .map(|entry| {
            format!(
                "*{}*",
                instance_label(&entry.agent, entry.index, entry.total)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Where one inbound message goes. Commands never reach this layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// The main agent, with the body already stripped of an explicit
    /// `@morpheus` (or unchanged when there was no address).
    Main { body: String },
    /// One external agent instance.
    Agent {
        agent: String,
        index: usize,
        total: usize,
        body: String,
    },
    /// One turn per selected instance.
    Fanout { body: String },
    /// Answer the user with this text; start no turn.
    Hint(String),
    /// Keep the whole message as ordinary text for the main agent (unknown
    /// `@` tokens are preserved verbatim).
    Passthrough(String),
}

/// Resolve one message's destination.
///
/// Only a token at the very start (after leading whitespace) is an address.
/// The token is removed with at most one following whitespace character; the
/// rest is passed through verbatim. Unknown `@` tokens and invalid address
/// syntax stay ordinary text for the main agent, which keeps prose and group
/// mentions from being hijacked.
#[must_use]
pub fn route(text: &str, roster: &[String], installed: &[String]) -> Route {
    let trimmed = text.trim_start();
    let Some(rest) = trimmed.strip_prefix('@') else {
        return Route::Main {
            body: trimmed.to_string(),
        };
    };

    // A space between `@` and the name breaks the address: `@ claude` is
    // ordinary text, not an attempt to reach claude.
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        return Route::Passthrough(trimmed.to_string());
    }
    let Some(token) = rest.split_whitespace().next() else {
        return Route::Passthrough(trimmed.to_string());
    };
    let after = &rest[token.len()..];
    let body = strip_one_separator(after);

    if token.eq_ignore_ascii_case("all") {
        if roster.is_empty() {
            return Route::Hint(
                "No external agents are selected. Send `/agent <name>` first, or `/agent ls` to \
                 see what is installed."
                    .to_string(),
            );
        }
        if body.trim().is_empty() {
            return Route::Hint("Send a message after `@all`.".to_string());
        }
        return Route::Fanout { body };
    }
    if token.eq_ignore_ascii_case("morpheus") {
        if body.trim().is_empty() {
            return Route::Hint("Send a message after `@morpheus`.".to_string());
        }
        return Route::Main { body };
    }

    let Some((agent, requested_index)) = parse_agent_token(token) else {
        return Route::Passthrough(trimmed.to_string());
    };
    if requested_index == Some(0) {
        return Route::Hint("Instances are numbered from 1: use `@claude#1`.".to_string());
    }

    let matching: Vec<RosterEntry> = entries(roster)
        .into_iter()
        .filter(|entry| entry.agent == agent)
        .collect();
    if matching.is_empty() {
        if installed.iter().any(|name| name == &agent) {
            return Route::Hint(format!(
                "`{agent}` is not selected. Send `/agent {agent}` first."
            ));
        }
        return Route::Passthrough(trimmed.to_string());
    }

    let entry = match requested_index {
        Some(index) => match matching.iter().find(|entry| entry.index == index).cloned() {
            Some(entry) => entry,
            None => {
                let total = matching.len();
                return Route::Hint(match total {
                    1 => format!("`{agent}` has one instance: use `@{agent}`."),
                    _ => format!(
                        "`{agent}` has instances 1–{total}: use `@{agent}#1` to `@{agent}#{total}`."
                    ),
                });
            }
        },
        None => {
            if matching.len() > 1 {
                return Route::Hint(format!(
                    "`{agent}` has {} instances. Address one with `@{agent}#1` … `@{agent}#{}`.",
                    matching.len(),
                    matching.len()
                ));
            }
            matching[0].clone()
        }
    };

    if body.trim().is_empty() {
        return Route::Hint(format!("Send a message after `@{agent}`."));
    }
    Route::Agent {
        agent: entry.agent,
        index: entry.index,
        total: entry.total,
        body,
    }
}

/// `claude#2` → `("claude", Some(2))`. Returns `None` for everything that is
/// not a well-formed address token (paths, punctuation, a stray `#`).
fn parse_agent_token(token: &str) -> Option<(String, Option<usize>)> {
    let (name, index) = match token.split_once('#') {
        Some((name, index)) => {
            if index.is_empty() || !index.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let index = index.parse::<usize>().ok()?;
            (name, Some(index))
        }
        None => (token, None),
    };
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return None;
    }
    Some((name.to_ascii_lowercase(), index))
}

/// Remove at most one whitespace character (the separator after the address).
fn strip_one_separator(text: &str) -> String {
    match text.chars().next() {
        Some(separator) if separator.is_whitespace() => text[separator.len_utf8()..].to_string(),
        _ => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    #[test]
    fn a_roster_is_canonicalized_and_keeps_instances() {
        let agents = installed(&["claude", "codex"]);
        assert_eq!(
            parse_roster(" Claude , codex,CLAUDE ", &agents).expect("valid roster"),
            vec![
                "claude".to_string(),
                "codex".to_string(),
                "claude".to_string()
            ]
        );
    }

    #[test]
    fn selecting_one_agent_is_valid() {
        assert_eq!(
            parse_roster("claude", &installed(&["claude"])).expect("valid"),
            vec!["claude".to_string()]
        );
    }

    #[test]
    fn the_main_agent_cannot_be_selected() {
        let error = parse_roster("morpheus", &installed(&["claude"])).expect_err("rejected");
        assert!(error.contains("main agent"), "{error}");
    }

    #[test]
    fn unknown_and_not_installed_names_are_rejected() {
        let agents = installed(&["claude", "codex"]);
        // Names are matched against the registry, never interpolated into a
        // command line.
        for injected in ["claude;rm -rf /", "$(id)", "../../bin/sh", "claude code"] {
            assert!(
                parse_roster(injected, &agents).is_err(),
                "{injected} must not be accepted"
            );
        }
        let error = parse_roster("maria", &agents).expect_err("rejected");
        assert!(error.contains("`maria` is not installed"), "{error}");
        assert!(error.contains("claude, codex"), "{error}");

        let error = parse_roster("maria", &[]).expect_err("rejected");
        assert!(error.contains("No ACP agents are installed"), "{error}");
    }

    #[test]
    fn empty_entries_are_errors_not_skipped() {
        let agents = installed(&["claude", "codex"]);
        let error = parse_roster("claude,,codex", &agents).expect_err("rejected");
        assert!(error.contains("Entry 2 is empty"), "{error}");

        assert!(parse_roster(",claude", &agents).is_err());
        assert!(parse_roster("claude,", &agents).is_err());
        assert!(parse_roster("   ", &agents).is_err());
    }

    #[test]
    fn the_cap_counts_instances() {
        let agents = installed(&["claude", "codex", "gemini", "opencode", "pi"]);
        let error = parse_roster("claude,codex,gemini,opencode,pi", &agents).expect_err("rejected");
        assert!(error.contains("at most 4"), "{error}");

        assert_eq!(
            parse_roster("claude,codex,gemini,opencode", &agents)
                .expect("four instances")
                .len(),
            4
        );
        assert_eq!(
            parse_roster("claude,claude,claude,claude", &agents)
                .expect("four instances of one agent")
                .len(),
            4
        );
    }

    #[test]
    fn entries_number_instances_in_selection_order() {
        let roster = vec![
            "claude".to_string(),
            "codex".to_string(),
            "claude".to_string(),
        ];
        assert_eq!(
            entries(&roster),
            vec![
                RosterEntry {
                    agent: "claude".into(),
                    index: 1,
                    total: 2
                },
                RosterEntry {
                    agent: "codex".into(),
                    index: 1,
                    total: 1
                },
                RosterEntry {
                    agent: "claude".into(),
                    index: 2,
                    total: 2
                },
            ]
        );
        assert_eq!(instance_key("claude", 2), "claude#2");
        assert_eq!(instance_label("claude", 2, 2), "claude #2");
        assert_eq!(instance_label("claude", 1, 1), "claude");
        assert_eq!(instance_label("opencode", 1, 1), "OpenCode");
    }

    #[test]
    fn persisted_rosters_are_sanitized_without_needing_installation() {
        assert_eq!(
            sanitize_roster(vec![
                " Claude ".into(),
                "".into(),
                "morpheus".into(),
                "claude".into(),
                "gemini".into(),
            ]),
            vec![
                "claude".to_string(),
                "claude".to_string(),
                "gemini".to_string()
            ],
            "unknown names are kept; only structural problems are dropped"
        );
        assert_eq!(
            sanitize_roster(vec![
                "a".into(),
                "b".into(),
                "c".into(),
                "d".into(),
                "e".into()
            ])
            .len(),
            MAX_ROSTER
        );
    }

    fn summary(agent: &str, index: usize, total: usize) -> AgentSummary {
        AgentSummary {
            agent: agent.to_string(),
            index,
            total,
            model: None,
            session: false,
        }
    }

    #[test]
    fn the_list_shows_the_main_agent_roster_and_installed_names() {
        let text = render_list(&[summary("claude", 1, 1)], &installed(&["claude", "codex"]));
        assert!(text.contains("morpheus"), "{text}");
        assert!(text.contains("• *claude*"), "{text}");
        assert!(text.contains("Installed: claude, codex"), "{text}");
        assert!(text.contains("`/agent claude`"), "{text}");
        assert!(text.contains("`/agent off`"), "{text}");
    }

    #[test]
    fn the_list_names_instances() {
        let text = render_list(
            &[summary("claude", 1, 2), summary("claude", 2, 2)],
            &installed(&["claude"]),
        );
        assert!(text.contains("• *claude #1*"), "{text}");
        assert!(text.contains("• *claude #2*"), "{text}");
    }

    #[test]
    fn the_list_shows_the_model_and_session_state() {
        let mut picked = summary("claude", 1, 1);
        picked.model = Some("sonnet-4".to_string());
        picked.session = true;
        let text = render_list(
            &[picked, summary("codex", 1, 1)],
            &installed(&["claude", "codex"]),
        );

        assert!(
            text.contains("• *claude* - model `sonnet-4`, session saved"),
            "{text}"
        );
        assert!(
            text.contains("• *codex* - default model, new session"),
            "{text}"
        );
    }

    #[test]
    fn the_list_handles_an_empty_surface() {
        let text = render_list(&[], &[]);
        assert!(text.contains("Selected: none"), "{text}");
        assert!(text.contains("Installed: none"), "{text}");
        assert!(!text.contains("Example:"), "{text}");
    }

    #[test]
    fn ls_all_marks_installation_but_keeps_registered_agents() {
        let text = render_all(
            &[summary("claude", 1, 1)],
            &installed(&["claude"]),
            &installed(&["claude", "gemini"]),
        );
        assert!(text.contains("• claude - installed"), "{text}");
        assert!(text.contains("• gemini - not installed"), "{text}");
        assert!(text.contains("• *claude*"), "{text}");
    }

    #[test]
    fn unprefixed_messages_go_to_main_unchanged() {
        assert_eq!(
            route("  hello  ", &[], &[]),
            Route::Main {
                body: "hello  ".into()
            }
        );
        // With an agent selected (R2) and after a fan-out (R11), the default
        // still never changes.
        let roster = vec!["claude".to_string()];
        assert_eq!(
            route("hello", &roster, &installed(&["claude"])),
            Route::Main {
                body: "hello".into()
            }
        );
    }

    #[test]
    fn the_address_must_be_the_first_token() {
        assert_eq!(
            route(
                "please @claude fix",
                &["claude".into()],
                &installed(&["claude"])
            ),
            Route::Main {
                body: "please @claude fix".into()
            }
        );
    }

    #[test]
    fn only_the_matched_prefix_and_one_separator_are_removed() {
        let roster = vec!["claude".to_string()];
        assert_eq!(
            route("@claude  fix\nit", &roster, &installed(&["claude"])),
            Route::Agent {
                agent: "claude".into(),
                index: 1,
                total: 1,
                body: " fix\nit".into()
            }
        );
    }

    #[test]
    fn addressing_is_case_insensitive() {
        let roster = vec!["claude".to_string()];
        assert_eq!(
            route("@CLAUDE hi", &roster, &installed(&["claude"])),
            Route::Agent {
                agent: "claude".into(),
                index: 1,
                total: 1,
                body: "hi".into()
            }
        );
    }

    #[test]
    fn morpheus_is_the_explicit_main_address() {
        assert_eq!(
            route("@morpheus summarize", &[], &[]),
            Route::Main {
                body: "summarize".into()
            }
        );
        assert!(matches!(route("@morpheus", &[], &[]), Route::Hint(_)));
    }

    #[test]
    fn all_targets_the_roster_and_excludes_main() {
        assert_eq!(
            route("@all review", &["claude".into()], &[]),
            Route::Fanout {
                body: "review".into()
            }
        );
        assert!(matches!(route("@all review", &[], &[]), Route::Hint(_)));
        assert!(matches!(
            route("@all", &["claude".into()], &[]),
            Route::Hint(_)
        ));
    }

    #[test]
    fn an_installed_but_unselected_agent_gets_a_hint() {
        let route = route("@claude hi", &[], &installed(&["claude"]));
        match route {
            Route::Hint(text) => assert!(text.contains("/agent claude"), "{text}"),
            other => panic!("expected a hint, got {other:?}"),
        }
    }

    #[test]
    fn unknown_and_invalid_addresses_are_preserved_for_main() {
        for text in [
            "@maria hello",
            "@allx hi",
            "@ claude hi",
            "@claude# hi",
            "@claude;rm hi",
        ] {
            assert_eq!(
                route(text, &["claude".into()], &installed(&["claude"])),
                Route::Passthrough(text.to_string()),
                "{text}"
            );
        }
    }

    #[test]
    fn empty_bodies_after_a_valid_address_hint_without_a_turn() {
        let roster = vec!["claude".to_string()];
        assert!(matches!(
            route("@claude", &roster, &installed(&["claude"])),
            Route::Hint(_)
        ));
        assert!(matches!(
            route("@morpheus  ", &roster, &installed(&["claude"])),
            Route::Hint(_)
        ));
        assert!(matches!(
            route("@all ", &roster, &installed(&["claude"])),
            Route::Hint(_)
        ));
    }

    #[test]
    fn instances_are_addressed_by_index() {
        // A single instance is always index 1; asking for #2 is a hint, not a
        // turn on some other process.
        let single = vec!["claude".to_string()];
        assert_eq!(
            route("@claude#1 go", &single, &installed(&["claude"])),
            Route::Agent {
                agent: "claude".into(),
                index: 1,
                total: 1,
                body: "go".into()
            }
        );
        match route("@claude#2 go", &single, &installed(&["claude"])) {
            Route::Hint(text) => assert!(text.contains("one instance"), "{text}"),
            other => panic!("expected a hint, got {other:?}"),
        }

        let roster = vec!["claude".to_string(), "claude".to_string()];
        assert_eq!(
            route("@claude#2 go", &roster, &installed(&["claude"])),
            Route::Agent {
                agent: "claude".into(),
                index: 2,
                total: 2,
                body: "go".into()
            }
        );
        assert!(matches!(
            route("@claude#3 go", &roster, &installed(&["claude"])),
            Route::Hint(_)
        ));
        assert!(matches!(
            route("@claude#0 go", &roster, &installed(&["claude"])),
            Route::Hint(_)
        ));
    }

    #[test]
    fn a_multi_instance_agent_requires_an_index() {
        let roster = vec!["claude".to_string(), "claude".to_string()];
        match route("@claude go", &roster, &installed(&["claude"])) {
            Route::Hint(text) => assert!(text.contains("#1"), "{text}"),
            other => panic!("expected an instance hint, got {other:?}"),
        }
    }
}
