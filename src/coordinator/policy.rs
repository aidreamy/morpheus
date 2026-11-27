//! The pane turn's TOOL POLICY - and why it is not, and must never become, a
//! second copy of the Manager mode's restriction.
//!
//! ## The bug this file fixes
//!
//! The TUI carried a `MANAGER_BLOCKED_TOOLS` list that it applied to every
//! morpheus pane turn, on the stated rationale "delegated work reads, searches,
//! runs commands and reports back - it must not edit the workspace". That
//! rationale was written when the only thing separating a pane from the main
//! loop was which agent name it ran; the pane had no checkout of its own, so a
//! write from a delegated agent landed in the user's tree.
//!
//! Two things made the list wrong:
//!
//! 1. **It duplicated a mode.** `Mode::Manager` already owns exactly that
//!    restriction, where it belongs: `harness::core::tool_routing`
//!    (`is_manager_restricted` hides the four file-writing tools at
//!    REGISTRATION; `is_manager_blocked_bash` fences write commands). The pane
//!    list was a *second, wider* copy - it also removed `fs_rollback`,
//!    `lsp_rename`, `lsp_code_actions`, `computer_*` - so a pane could not even
//!    restore a recorded version, and the two lists drifted independently.
//! 2. **The panic it prevented no longer exists.** A pane pinned by
//!    `/worktree` writes inside its own checkout: `pane_effective_cwd` feeds
//!    the ACP session cwd, the harness root, the `MorpheusTools` root, the LSP and
//!    the fs sandbox root, and the sandbox pins that root. The workspace-
//!    mutating half of the list became an outage, not a guardrail: the morpheus
//!    pane - the one agent whose cost and context the product actually
//!    controls - was the only pane unable to implement anything, while an
//!    external CLI pane could.
//!
//! `Mode::Manager` is NOT what a pane runs. A pane runs the user's session mode
//! (`Build`/`Yolo`/`Ask`/`Command`) through the assembly's `mode`, and its
//! instructions are the Build BASE (the full Build prompt minus the self-review
//! loop). A subordinate agent is a WORKER, so it gets the worker's own, much
//! smaller policy - this file - and nothing of the Manager mode's.
//!
//! ## What is left, and why
//!
//! Only restrictions that follow from the pane being HEADLESS or from its
//! report contract - never from "delegated work must not touch the tree":
//!
//! - `ask_questions` - the pane has no human attached to answer it. The call
//!   would sit in a permission box nobody renders; a headless agent must decide
//!   or report the blocker.
//! - `computer_act`, `computer_control` - the desktop belongs to the user, and
//!   a pane has no confirmed focus. Listings, snapshots and captures stay: they
//!   are how a delegated agent inspects a UI it was asked about.
//! - `stop_agent_loop` - the pane's deliverable IS its final report, and the
//!   close classifier reads a stop as "the user stopped this turn". The harness
//!   already strips it for the coworker ROLE (`harness::core::build`,
//!   `with_own_coworker_id`); it is repeated here as belt-and-braces because the
//!   invariant is load-bearing, not cosmetic.
//!
//! `agents_call`, `agents_worktree` and `agents_compact` are deliberately
//! ABSENT: the harness strips them by role for every coworker, and listing them
//! again here would suggest a pane could keep them if this list changed.

use std::collections::HashSet;

/// Tools a pane turn never gets, for reasons that are about the PANE (headless,
/// deliverable-bounded) and not about delegated work being untrusted.
///
/// See the module docs for the per-entry justification. Adding a workspace-
/// mutating tool here reintroduces the bug this file documents.
pub const PANE_BLOCKED_TOOLS: &[&str] = &[
    "ask_questions",
    "computer_act",
    "computer_control",
    "stop_agent_loop",
];

/// Build the `disabled_tools` set for a pane's harness: the caller's base set
/// (per-pane preferences; the split currently passes an empty one - a pane is
/// a fresh harness, not the user's chat) plus [`PANE_BLOCKED_TOOLS`].
///
/// Note what is NOT done here: the user's chat-level disabled set is not
/// inherited, and neither is anything from `Mode::Manager`. Those are separate
/// policies over separate agents.
pub fn pane_disabled_tools(base: &HashSet<String>) -> HashSet<String> {
    base.iter()
        .cloned()
        .chain(PANE_BLOCKED_TOOLS.iter().map(|tool| (*tool).to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression this module exists for: a pane turn can WRITE. These four
    /// are `Mode::Manager`'s restriction, and a pane is not the manager mode.
    #[test]
    fn a_pane_keeps_the_workspace_mutating_tools() {
        let disabled = pane_disabled_tools(&HashSet::new());
        for tool in ["fs_write", "fs_edit", "fs_edit_lines", "fs_ast_edit"] {
            assert!(
                !disabled.contains(tool),
                "{tool} must reach a pane: the Manager mode's restriction does not apply to a worker"
            );
        }
        // The wider TUI list was not just the Manager mode's four: it also
        // removed a restore and the mutating half of the LSP. Both are legitimate
        // delegated work inside the pane's own checkout.
        for tool in ["fs_rollback", "lsp_rename", "lsp_code_actions"] {
            assert!(
                !disabled.contains(tool),
                "{tool} must reach a pane (delegated work owns its checkout)"
            );
        }
    }

    /// The headless constraints stay, so the fix does not turn into "no policy".
    #[test]
    fn a_pane_stays_headless_and_report_bounded() {
        let disabled = pane_disabled_tools(&HashSet::new());
        for tool in PANE_BLOCKED_TOOLS {
            assert!(disabled.contains(*tool), "{tool} must stay blocked");
        }
        // Read-only desktop inspection is how a delegated agent looks at the UI
        // it was asked about; only the mutating half is gone.
        assert!(!disabled.contains("computer_windows"));
    }

    /// The base set is the caller's, not the chat's: disabling a tool for the
    /// user must not silently travel into a pane, and a pane-level base set must
    /// still be honored.
    #[test]
    fn the_base_set_is_passed_through_untouched() {
        let mut base = HashSet::new();
        base.insert("web_search".to_string());
        let disabled = pane_disabled_tools(&base);
        assert!(disabled.contains("web_search"));
        assert!(disabled.contains("ask_questions"));
        assert_eq!(
            disabled.len(),
            PANE_BLOCKED_TOOLS.len() + 1,
            "nothing else is injected"
        );
    }
}
