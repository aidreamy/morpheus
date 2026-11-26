//! Per-turn decision logic for a delegated pane turn: how its input is
//! composed, which directory the turn runs in, and how its ending is
//! classified.
//!
//! Everything here is PURE over plain data — no `App`, no split state, no
//! timeline types. That is what lets the same rules serve the TUI today and a
//! GUI (or a headless coordinator) tomorrow, and what lets the tests pin the
//! behavior without a harness.

use std::path::{Path, PathBuf};

use morpheus_agents::events::AgentConfigOption;

use crate::harness::agent_coworkers::AgentCoworkerTurnEnd;

/// Compose a pane turn's input from its optional layers, in wire order: the
/// worktree HANDOFF block first (the agent learns where it now works and what
/// it was doing), the awareness PREAMBLE next (siblings), then the raw message.
///
/// Both optional layers already carry their own trailing blank line where the
/// block form needs one; this function only joins.
pub fn assemble_turn_input(
    handoff_block: Option<String>,
    preamble: Option<String>,
    msg: String,
) -> String {
    match (handoff_block, preamble) {
        (Some(block), Some(preamble)) => format!("{block}{preamble}\n\n{msg}"),
        (Some(block), None) => format!("{block}{msg}"),
        (None, Some(preamble)) => format!("{preamble}\n\n{msg}"),
        (None, None) => msg,
    }
}

/// A pane's EFFECTIVE working directory — the ONE resolution every consumer
/// reads: the worktree path when the pane is pinned (`/worktree`), the
/// session's working directory otherwise.
///
/// The ACP turn's `cwd`, the pane harness's `HarnessAssembly::cwd` (harness
/// root, `MorpheusTools` root, LSP, hooks) and the fs sandbox root all funnel
/// through it, so a binding is either honored by every layer or by none.
pub fn effective_cwd(worktree: Option<&Path>, session_cwd: &str) -> PathBuf {
    worktree
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(session_cwd))
}

/// Which terminal event the pane loop emitted. The three shapes are NOT
/// interchangeable, and the distinction is exactly what the pane used to lose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalKind {
    /// The loop closed by itself (`HarnessEvent::Done`).
    Done,
    /// The loop observed a stop request (`HarnessEvent::Stopped`). For a pane
    /// this is the USER's stop and nothing else: the coworker role has no
    /// `stop_agent_loop`, so the model cannot stop its own loop.
    Stopped,
    /// The loop closed on an error (`HarnessEvent::Error`).
    Error,
}

/// The loop's terminal event, captured by the bridge so the pane's close can
/// report what actually happened.
#[derive(Debug, Clone)]
pub struct TerminalCause {
    pub kind: TerminalKind,
    /// The error text (`Error`) or the guard-stop audit (`Done` with a
    /// `checkup_verdict`); `None` for a clean natural completion and for
    /// `Stopped`.
    pub detail: Option<String>,
}

/// Decide the pane turn's report and how it ended — the ONE place a pane turn
/// is classified.
///
/// The rule, in order:
///
/// 1. **A closure** → `end_turn`. The loop promoted its own final text.
/// 2. **No terminal event at all** → `error`. The loop vanished without saying
///    why; that is a harness defect, not a result.
/// 3. **`HarnessEvent::Error`** → `error`, carrying the real message. The
///    report IS the failure text (a requester must see it).
/// 4. **`HarnessEvent::Stopped`** → `cancelled`. The single legitimate
///    `cancelled`: only the user stops a pane turn.
/// 5. **`Done` without a closure, but the model wrote something** → `partial`:
///    the turn's real last text, marked as not necessarily final.
/// 6. **`Done` with nothing written at all** → `incomplete`, naming the guard
///    verdict when there is one, so "why did it stop?" stays answerable.
///
/// Note what is NOT here: any branch that produces a generic message while
/// real text exists, or that reports `cancelled` for a turn nobody stopped.
pub fn close_pane_turn(
    cause: Option<&TerminalCause>,
    final_answer: Option<String>,
    fallback: Option<String>,
) -> (String, AgentCoworkerTurnEnd) {
    if let Some(report) = final_answer {
        return (report, AgentCoworkerTurnEnd::EndTurn);
    }
    // No closure. From here the terminal cause decides, and the fallback text
    // (when the model wrote anything) becomes the report.
    let fallback = fallback.filter(|text| !text.trim().is_empty());
    match cause.map(|c| c.kind) {
        None => (
            "The pane turn ended without any terminal event from the harness. \
             That is a bug, not a result: no report was produced."
                .to_string(),
            AgentCoworkerTurnEnd::Error,
        ),
        Some(TerminalKind::Error) => (
            cause
                .and_then(|c| c.detail.clone())
                .unwrap_or_else(|| "the pane turn failed".to_string()),
            AgentCoworkerTurnEnd::Error,
        ),
        Some(TerminalKind::Stopped) => (
            "The user stopped this pane turn.".to_string(),
            AgentCoworkerTurnEnd::Cancelled,
        ),
        Some(TerminalKind::Done) => match fallback {
            // The model wrote something: deliver it as the report, marked.
            Some(text) => (text, AgentCoworkerTurnEnd::Partial),
            None => {
                let verdict = cause
                    .and_then(|c| c.detail.as_deref())
                    .map(|v| format!(" (guard stop: {v})"))
                    .unwrap_or_default();
                (
                    format!(
                        "The pane turn ended without producing any text{verdict}. \
                         The model wrote no report at all."
                    ),
                    AgentCoworkerTurnEnd::Incomplete,
                )
            }
        },
    }
}

/// Which ACP session config option a standardized control binds to.
///
/// Shared vocabulary: the coordinator uses it to decide which model a pane
/// turn runs with, and a frontend uses the same matching rule to seed the
/// control the user sees — one rule, so the box and the turn agree by
/// construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigTarget {
    /// Match an option by its exact id (e.g. `provider` — no category exists
    /// for it in the spec).
    ById(&'static str),
    /// Match an option by its semantic category (e.g. `model` — harnesses
    /// predate the category system, so the id alone is not stable).
    ByCategory(&'static str),
}

/// Match one session config option against a [`ConfigTarget`].
///
/// `ByCategory` is a TWO-PASS match: an option whose `id` equals the category
/// name wins first — cline categorizes BOTH its `provider` and `model` options
/// as `Model`, so a single-pass category scan in list order would hand the
/// model box the provider option. Only when no such id exists does the
/// category field decide (harnesses that predate ids — the reason the
/// category fallback exists at all). A flat per-element OR is wrong for the
/// same reason: it cannot tell "the model option" from "the provider option".
#[must_use]
pub fn match_option(
    options: &[AgentConfigOption],
    target: ConfigTarget,
) -> Option<&AgentConfigOption> {
    match target {
        ConfigTarget::ById(id) => options.iter().find(|o| o.id == id),
        ConfigTarget::ByCategory(category) => {
            options.iter().find(|o| o.id == category).or_else(|| {
                options
                    .iter()
                    .find(|o| o.category.as_deref() == Some(category))
            })
        }
    }
}

/// The pane option the MODEL surface rides, resolved through
/// [`match_option`]'s two-pass `ByCategory("model")`. Returns the option's OWN
/// id — a pick must ride the id the harness advertises
/// (`apply_session_picks` matches against the advertised list).
#[must_use]
pub fn model_option_id(options: &[AgentConfigOption]) -> Option<&str> {
    match_option(options, ConfigTarget::ByCategory("model")).map(|o| o.id.as_str())
}

/// The turn's config picks, in precedence order: the pane's own USER picks
/// (the config-box confirm wrote the option's current and recorded its id)
/// first, then the persisted per-agent model pick — the CLI-parity "remember
/// my last model". A freshly created pane carries no picks of its own, while
/// an agent's native CLI reuses the last chosen model for every NEW session;
/// the persisted record restores that behavior (the headless ACP harness never
/// runs the interactive layer that would remember it itself). The registry
/// default still applies when neither exists: `acp::call` injects `entry.model`
/// only into a model-less pick set. A persisted model the harness no longer
/// offers is skipped inside `apply_session_picks` — it degrades to the harness
/// default instead of failing the turn.
#[must_use]
pub fn turn_picks(
    config_options: Option<&[AgentConfigOption]>,
    picked_config_ids: &[String],
    persisted_model: Option<&str>,
) -> Vec<(String, String)> {
    let mut picks = config_options
        .map(|opts| {
            opts.iter()
                .filter(|o| !o.current.is_empty() && picked_config_ids.iter().any(|id| id == &o.id))
                .map(|o| (o.id.clone(), o.current.clone()))
                .collect::<Vec<(String, String)>>()
        })
        .unwrap_or_default();
    // The persisted pick seeds under the MODEL option's OWN id — a harness
    // that names the option differently but tags the category only honors a
    // pick carrying ITS id (`apply_session_picks` matches against the
    // advertised list). No model option advertised at all: fall back to the
    // canonical `model` id (the pick is then skipped as unadvertised,
    // degrading to the harness default exactly as designed). The suppression
    // check compares against that SAME seed id, not a hard-coded `model`, so
    // the pane's own pick suppresses the seed whichever way the harness names
    // the option.
    if let Some(model) = persisted_model {
        let seed_id = config_options
            .and_then(model_option_id)
            .unwrap_or("model")
            .to_string();
        if !picks.iter().any(|(id, _)| id == &seed_id) {
            picks.push((seed_id, model.to_string()));
        }
    }
    picks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_input_composes_handoff_then_preamble_then_message() {
        let input = assemble_turn_input(
            Some("[moved]\n\n".into()),
            Some("## Agents Panes\n- claude#1".into()),
            "the message".into(),
        );
        assert_eq!(
            input,
            "[moved]\n\n## Agents Panes\n- claude#1\n\nthe message"
        );
        // Preamble without handoff — the Phase 1 shape.
        assert_eq!(
            assemble_turn_input(None, Some("## Agents Panes".into()), "msg".into()),
            "## Agents Panes\n\nmsg"
        );
        // Neither layer — the input goes out verbatim.
        assert_eq!(assemble_turn_input(None, None, "msg".into()), "msg");
    }

    /// The worktree path wins over the session directory; without a binding the
    /// session's directory is the turn's directory.
    #[test]
    fn effective_cwd_prefers_the_bound_worktree() {
        let worktree = PathBuf::from("/tmp/wt/feature");
        assert_eq!(
            effective_cwd(Some(worktree.as_path()), "/home/user/project"),
            worktree
        );
        assert_eq!(
            effective_cwd(None, "/home/user/project"),
            PathBuf::from("/home/user/project")
        );
    }

    /// The classification table behind every pane turn's close. This is the
    /// test that pins the ACTUAL bug: a turn that ended on its own without a
    /// closure must never be reported as `cancelled`, and a turn whose model
    /// DID write text must never report "no report".
    #[test]
    fn a_pane_turn_is_classified_from_its_real_terminal_cause() {
        let done = TerminalCause {
            kind: TerminalKind::Done,
            detail: None,
        };
        let stopped = TerminalCause {
            kind: TerminalKind::Stopped,
            detail: None,
        };
        let errored = TerminalCause {
            kind: TerminalKind::Error,
            detail: Some("connector exploded".to_string()),
        };
        let guard = TerminalCause {
            kind: TerminalKind::Done,
            detail: Some("termination checkup @ max-iterations".to_string()),
        };

        // 1. A closure is the natural end.
        let (text, end) = close_pane_turn(Some(&done), Some("the final answer".to_string()), None);
        assert_eq!(
            (text.as_str(), end),
            ("the final answer", AgentCoworkerTurnEnd::EndTurn)
        );

        // 2. THE REGRESSION: no closure, but the model wrote text one item
        // earlier (the turn ended on a tool result). It must be delivered as
        // a `partial` report — never as "ended without a report", and never
        // as `cancelled`.
        let (text, end) =
            close_pane_turn(Some(&done), None, Some("now I'll edit core.rs".to_string()));
        assert_eq!(text, "now I'll edit core.rs");
        assert_eq!(end, AgentCoworkerTurnEnd::Partial);
        assert!(!end.is_failure(), "a partial report is real content");

        // 3. Nothing written at all and a guard fired: `incomplete`, naming
        // the guard so "why did it stop?" stays answerable.
        let (text, end) = close_pane_turn(Some(&guard), None, None);
        assert_eq!(end, AgentCoworkerTurnEnd::Incomplete);
        assert!(text.contains("max-iterations"), "{text}");

        // A whitespace-only "answer" is NOT a report (the same trim contract
        // the gate applies).
        let (_, end) = close_pane_turn(Some(&done), None, Some("   ".to_string()));
        assert_eq!(end, AgentCoworkerTurnEnd::Incomplete);

        // 4. `Stopped` is the USER's stop and the ONLY legitimate
        // `cancelled` — for a pane the model cannot stop its own loop.
        let (_, end) = close_pane_turn(Some(&stopped), None, Some("half done".to_string()));
        assert_eq!(end, AgentCoworkerTurnEnd::Cancelled);

        // 5. An error carries its real message as the report.
        let (text, end) = close_pane_turn(Some(&errored), None, Some("partial".to_string()));
        assert_eq!(text, "connector exploded");
        assert_eq!(end, AgentCoworkerTurnEnd::Error);

        // 6. No terminal event at all is a harness defect, surfaced as such.
        let (text, end) = close_pane_turn(None, None, Some("orphan text".to_string()));
        assert_eq!(end, AgentCoworkerTurnEnd::Error);
        assert!(text.contains("bug, not a result"), "{text}");
    }
    /// The turn picks' precedence contract: the pane's own user picks ride
    /// first; a persisted per-agent model pick seeds ONLY when the pane has
    /// none (a fresh pane), and the absence of both leaves the pick set
    /// model-less so the registry default still applies.
    #[test]
    fn turn_picks_precedence() {
        use morpheus_agents::events::AgentConfigOption;

        let model = AgentConfigOption {
            id: "model".to_string(),
            name: "Model".to_string(),
            description: None,
            category: Some("model".to_string()),
            values: Vec::new(),
            current: "pane-picked".to_string(),
            boolean: None,
        };
        let reasoning = AgentConfigOption {
            id: "reasoning".to_string(),
            name: "Reasoning".to_string(),
            description: None,
            category: Some("thought_level".to_string()),
            values: Vec::new(),
            current: "high".to_string(),
            boolean: None,
        };
        let untouched = AgentConfigOption {
            id: "provider".to_string(),
            name: "Provider".to_string(),
            description: None,
            category: None,
            values: Vec::new(),
            current: "harness-current".to_string(),
            boolean: None,
        };

        // A fresh pane (no picked ids) gets the persisted pick seeded.
        let picks = turn_picks(
            Some(&[model.clone(), untouched.clone()]),
            &[],
            Some("persisted-model"),
        );
        assert_eq!(
            picks,
            vec![("model".to_string(), "persisted-model".to_string())],
            "a fresh pane rides the persisted per-agent pick"
        );

        // The pane's OWN pick wins over the persisted one.
        let picks = turn_picks(
            Some(&[model.clone(), reasoning.clone()]),
            &["model".to_string(), "reasoning".to_string()],
            Some("persisted-model"),
        );
        assert_eq!(
            picks,
            vec![
                ("model".to_string(), "pane-picked".to_string()),
                ("reasoning".to_string(), "high".to_string()),
            ],
            "the pane's user pick precedes the persisted one and suppresses the seed"
        );

        // No pane picks and no persisted pick: the set stays EMPTY (the
        // registry default in `acp::call` still applies).
        let picks = turn_picks(Some(std::slice::from_ref(&untouched)), &[], None);
        assert!(picks.is_empty(), "untouched options ride no picks");

        // No config options at all (the pane never saw a report): the
        // persisted pick still seeds.
        let picks = turn_picks(None, &[], Some("persisted-model"));
        assert_eq!(
            picks,
            vec![("model".to_string(), "persisted-model".to_string())]
        );
    }

    /// A harness that names the model option differently but tags the
    /// category: the seed rides the option's REAL id, and the pane's own pick
    /// of that same option suppresses the seed — the id comparison, not a
    /// hard-coded `model`, keeps the user's fresher choice from being
    /// overridden by the persisted record in `apply_session_picks`.
    #[test]
    fn turn_picks_seed_suppression_matches_the_option_id() {
        use morpheus_agents::events::AgentConfigOption;

        let renamed = AgentConfigOption {
            id: "session_model".to_string(),
            name: "Model".to_string(),
            description: None,
            category: Some("model".to_string()),
            values: Vec::new(),
            current: "pane-picked".to_string(),
            boolean: None,
        };

        // The seed rides the option's own id.
        let picks = turn_picks(
            Some(std::slice::from_ref(&renamed)),
            &[],
            Some("persisted-model"),
        );
        assert_eq!(
            picks,
            vec![("session_model".to_string(), "persisted-model".to_string())],
            "a differently-named model option seeds under its own id"
        );

        // The pane's own pick of that option suppresses the seed.
        let picks = turn_picks(
            Some(std::slice::from_ref(&renamed)),
            &["session_model".to_string()],
            Some("persisted-model"),
        );
        assert_eq!(
            picks,
            vec![("session_model".to_string(), "pane-picked".to_string())],
            "the pane's own pick suppresses the seed by id, not by the canonical name"
        );
    }
}
