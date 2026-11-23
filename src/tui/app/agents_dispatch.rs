//! Dispatch for the `/agents` split-pane mode: one submitted prompt goes to
//! the FOCUSED pane — or, when `Alt+0` left NO pane focused, to the MAIN
//! agent itself (the normal-mode loop, unchanged). External agent CLIs each
//! get an independent ACP turn (`morpheus_tools::subagent::acp::call` — the
//! same one-shot contract the prompt corrector uses); each morpheus pane (at
//! any split position) runs the MAIN agent itself on its own real morpheus
//! session — persisted in the session store (visible in the history panel)
//! with the mode's normal prompt minus the self-review loop, and without
//! the workspace-mutating tools delegated work never needs.
//!
//! Routing identity: each pane owns its event channel, so pane INDEX
//! replaces the `call_ref` the harness tool path would provide. Events
//! reach the TUI through the shared `HarnessEvent` channel as
//! `AgentsPaneEvent` (live stream) / `AgentsPaneDone` (final report).

use crate::routes::session::right_panel::types::SubagentTimelineEntry;
use morpheus::coordinator::turn::{
    TerminalCause, TerminalKind, assemble_turn_input, close_pane_turn,
};
use morpheus::harness::events::HarnessEvent;
use morpheus_tools::subagent::events::{SubagentEvent, ToolCallStatus, ToolOutputBlock};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::App;
use crate::routes::session::agents_split::{
    AgentPane, AgentsSplitState, MANAGER_LABEL, PaneModelSelection,
};
use crate::ui::toast::{ToastOptions, ToastVariant};
use crossterm::event::KeyCode;

/// Sync the slash menu's command source to the split's focus: a FOCUSED
/// pane serves its own (blocklist-filtered) command list — or its spinner
/// row while the snapshot probe is still in flight — and NO focus returns
/// the morpheus set (the main agent's commands). Only the LIVE split counts:
/// a parked split's stale `focused` must not hijack the menu (its prompt
/// routing goes to the main agent). Idempotent by content: an unchanged
/// source keeps the menu's selection, so this is safe to call on every
/// focus change and probe arrival.
pub(super) fn sync_slash_source(app: &mut App) {
    let next = app
        .agents_split
        .as_ref()
        .and_then(|split| split.slash_source());
    match next {
        Some((commands, loading)) => {
            if !slash_lists_equal(&commands, &app.slash_menu.commands) {
                app.slash_menu.set_agent_commands(commands);
            }
            app.slash_menu.set_loading(loading);
        }
        None => {
            let morpheus = crate::ui::slash_menu::SlashMenu::morpheus_commands();
            if !slash_lists_equal(&morpheus, &app.slash_menu.commands) {
                app.slash_menu.set_morpheus_commands();
            }
            app.slash_menu.set_loading(false);
        }
    }
}

/// Menu-source equality by (name, desc) pairs — a cheap change detector so
/// repeated syncs never reset the user's selection/filter position.
fn slash_lists_equal(
    a: &[crate::ui::slash_menu::SlashCommand],
    b: &[crate::ui::slash_menu::SlashCommand],
) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| x.name == y.name && x.desc == y.desc)
}

/// Route a slash command picked while a LIVE split pane holds the focus, by
/// the command's DECLARED SCOPE (`slash_menu::command_scope`). Returns `true`
/// when consumed (the caller closes the menu and clears the draft); `false`
/// means "not this layer's" and the App handles it.
///
/// The cases, in order — this order IS the contract:
///
/// 1. **Global** (`themes`, `background`, `bell`, `spinner`,
///    `settings`, `router`, `providers`, `new`, `rename`, `undo`, `agents`):
///    app/TUI state, of which there is exactly one instance per process.
///    Never handed to a pane, whoever is focused — a `/background` picked on
///    a claude pane is still this one terminal's background.
/// 2. **Agent**, and applied by morpheus itself: `/worktree`, the pane's cwd
///    binding. Consumed here for ANY pane kind — the MANAGER included, since
///    a bound morpheus pane runs its turns in the checkout exactly like a bound
///    ACP pane.
/// 3. **Agent**, on a MANAGER pane: the manager's agent IS the main loop, so
///    its `/models`, `/compact`, … are the App's own handlers (they write the
///    very `llm_config` that loop reads).
/// 4. **Agent**, on an ACP pane: a mapped group with a config target
///    (models / provider / mode) opens the standardized box bound to the
///    pane's own ACP config option; everything else (compact, review, custom
///    harness commands …) is sent to the pane VERBATIM as its next prompt
///    (`/name`) and the harness interprets its own command.
///
/// Unfocused, or no live split: `false` — the App handles the command, which
/// for an Agent-scoped one means the main agent, the only agent that exists
/// in that situation.
pub(super) fn route_agent_slash_command(
    app: &mut App,
    cmd: &crate::ui::slash_menu::SlashCommand,
) -> bool {
    use crate::ui::slash_menu::{CommandScope, command_scope};
    let Some(split) = app.agents_split.as_ref() else {
        return false;
    };
    let Some(focused) = split.focused else {
        return false;
    };
    let Some(pane) = split.panes.get(focused) else {
        return false;
    };
    // 1. Global: one instance per process — always the App's.
    if command_scope(&cmd.name) == CommandScope::Global {
        return false;
    }
    if cmd.name == "worktree" {
        // 2. morpheus's own git-style command: intercepted BEFORE the manager
        // guard and the passthrough (the worktree binding is morpheus-side,
        // never the agent's own) — its row is listed in EVERY pane's menu,
        // so a pick here must always be consumed, refusals included. The
        // typed argument tail rides the prompt — read it here, then apply.
        let tail =
            crate::ui::slash_menu::SlashMenu::command_arg_tail(&app.prompt_view.input, &cmd.name);
        apply_worktree_command(app, tail.as_deref());
        return true;
    }
    // 2b. The manager pane's /models: the pane's OWN model picker. The
    // manager's agent is this app's own loop, so the fall-through below
    // would open the GLOBAL picker and move every agent's model with it —
    // but the split's contract is per-pane agents: a pick here lands on
    // THIS pane's session only (the pane's own selection plus its session
    // markers, never the global config). External agents keep their own
    // config-dialog path (the `resolve` branch below) — the picker here is
    // the morpheus pane's, which has no ACP config options to offer.
    if cmd.name == "models" && pane.agent == MANAGER_LABEL {
        app.open_pane_model_dialog(focused);
        return true;
    }
    // 3. The manager pane's agent is this app's own loop: the App's handlers
    // ARE its handlers, so nothing is passed to it as prose.
    if pane.agent == MANAGER_LABEL {
        return false;
    }
    if let Some(target) =
        crate::routes::session::agent_command_map::resolve(&cmd.name).and_then(|group| group.target)
    {
        app.open_agent_config_dialog(target);
        return true;
    }
    // Direct passthrough: the command goes to the pane as its prompt.
    app.dispatch_to_agents_split(format!("/{}", cmd.name));
    true
}

/// Parse and apply the `/worktree` arguments on the focused pane (git-style,
/// no dialogs):
///
/// - `add <path> <branch>` — worktree at `<path>` on a new branch, based on
///   the pane's current HEAD;
/// - `add <branch>` — same, at the default temp path (the slug of the
///   branch under `<OS temp>/morpheus-worktrees/`);
/// - `exit` — drop the pane's worktree binding and return it to the
///   session's cwd. Nothing is removed: the worktree directory and its
///   branch survive (the name deliberately does not suggest deletion —
///   only the pane's cwd binding is dropped).
///
/// Anything else (no args, unknown subcommand) is refused with a usage
/// toast. The MANAGER pane is a pane like any other here: morpheus's own agent
/// loop resolves its cwd from the SAME per-pane binding an ACP pane uses
/// (`dispatch_to_pane` → `HarnessAssembly::cwd`), so binding it really does
/// move the turns into the checkout. The applied binding re-syncs the
/// registry (the worktree name reaches the main agent's pane listing) and
/// persists through the session metadata write-back. A SUCCESS is silent:
/// the pane's badge and the outcome text carry it, only refusals toast.
pub(super) fn apply_worktree_command(app: &mut App, tail: Option<&str>) {
    match parse_worktree_command(tail) {
        Some(WorktreeCommand::Add { path, branch }) => {
            assign_focused_pane_worktree(app, path.as_deref(), &branch);
        }
        Some(WorktreeCommand::Exit) => clear_focused_pane_worktree(app),
        Some(WorktreeCommand::List) => {
            // The FOCUSED pane is the binding a split user is looking at, so
            // that is the row `/worktree list` marks here.
            let bound = app
                .agents_split
                .as_ref()
                .and_then(|split| split.focused)
                .and_then(|index| app.agents_split.as_ref()?.panes.get(index))
                .and_then(|pane| pane.worktree.as_ref())
                .map(|worktree| worktree.path.clone());
            show_worktree_list(app, bound.as_deref());
        }
        // The grammar is shared with the MAIN agent's path (`parse_
        // worktree_command`), so "malformed" means the same thing for both.
        None => worktree_usage_toast(app),
    }
}

/// One parsed `/worktree` invocation. The pane path and the MAIN agent's
/// path share this parse: the two must accept byte-identical arguments, so
/// the grammar lives here once
/// (`add [<path>] <branch>` / `exit` / `list`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum WorktreeCommand {
    /// `add [<path>] <branch>` — `path` is `None` for the managed temp root.
    Add {
        path: Option<String>,
        branch: String,
    },
    /// `exit` — drop the binding; nothing on disk is touched.
    Exit,
    /// `list` — show the repository's worktrees, marking the bound one.
    List,
}

/// Split the argument tail into tokens, honouring double quotes.
///
/// `split_whitespace` is not enough here: a worktree path may contain a
/// space, and `/worktree list` deliberately prints copyable paths. Without
/// quoting, a copied `/tmp/my tree` could not be pasted back into `add` —
/// it would parse as two tokens and refuse with a branch/path error that
/// says nothing about the real problem. `"<path with spaces>"` round-trips.
///
/// Inside quotes, `\` escapes the next character (so a path can contain a
/// quote). An unterminated quote is malformed (`None`), never a guess about
/// where the path ends.
fn split_args(tail: &str) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    // A token exists even when it is empty (`""`), so this tracks "started"
    // separately from the buffer's contents.
    let mut has_token = false;
    let mut chars = tail.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            '\\' if in_quotes => {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                    has_token = true;
                }
            }
            ch if ch.is_whitespace() && !in_quotes => {
                if has_token {
                    out.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            ch => {
                current.push(ch);
                has_token = true;
            }
        }
    }
    if in_quotes {
        return None;
    }
    if has_token {
        out.push(current);
    }
    Some(out)
}

/// Parse the argument tail. `None` means malformed — the caller shows the
/// usage toast (an empty tail is malformed: the command takes arguments).
///
/// A path containing spaces is written quoted: `/worktree add "/tmp/my
/// tree" feat`.
fn parse_worktree_command(tail: Option<&str>) -> Option<WorktreeCommand> {
    let parts = split_args(tail?)?;
    match parts.as_slice() {
        [sub] if sub.as_str() == "exit" => Some(WorktreeCommand::Exit),
        [sub] if sub.as_str() == "list" => Some(WorktreeCommand::List),
        [add, branch] if add.as_str() == "add" => Some(WorktreeCommand::Add {
            path: None,
            branch: branch.clone(),
        }),
        [add, path, branch] if add.as_str() == "add" => Some(WorktreeCommand::Add {
            path: Some(path.clone()),
            branch: branch.clone(),
        }),
        _ => None,
    }
}

/// Render `/worktree list`: every worktree of the session repo, one row per
/// entry, the bound one marked.
///
/// A copyable TOAST, not a dialog. Two reasons, both structural:
///
/// - Every `DialogType` variant is dismissed and driven by its OWN
///   `handle_*_dialog_key` in `keys.rs`; there is no generic close. A new
///   variant would be a box the user could not leave — worse than a
///   transient one.
/// - The row count is unbounded (~35 checkouts on a working machine), and
///   the toast renderer already word-wraps over as many rows as the screen
///   holds, keeps the message copyable through the ⧉ button, and expires on
///   its own. A picker would add a selection state, a confirm handler and a
///   key mapping for a READ-ONLY listing.
///
/// The list is rebuilt from `git worktree list --porcelain` (the same
/// parser adoption uses), so it can never disagree with what `add` would
/// accept.
fn show_worktree_list(app: &mut App, bound: Option<&std::path::Path>) {
    use crate::ui::toast::{ToastOptions, ToastVariant};
    let repo = std::path::PathBuf::from(&app.state.working_directory);
    // `empty` and `failed` are tracked separately: an empty list is a fine
    // answer that deserves an Info toast, while only a git refusal is an
    // Error. Folding them together (both produced `rows: None`) rendered
    // "no worktrees reported" in the error style.
    let (message, rows, empty) = match morpheus::git::worktree::list(&repo) {
        Ok(entries) if entries.is_empty() => (
            format!("No worktrees reported for {}.", repo.display()),
            None,
            true,
        ),
        Ok(entries) => {
            let mut shown = String::new();
            let mut plain = String::new();
            for entry in &entries {
                let branch = entry.branch.as_deref().unwrap_or("(detached HEAD)");
                // Canonicalised on both sides: the binding stores the path
                // the engine resolved, git reports its own resolved
                // spelling, and a symlinked temp root (`/tmp` →
                // `/private/tmp`) would otherwise hide the marker.
                let is_bound = bound.is_some_and(|bound| {
                    match (
                        std::fs::canonicalize(bound),
                        std::fs::canonicalize(&entry.path),
                    ) {
                        (Ok(bound), Ok(entry)) => bound == entry,
                        _ => bound == entry.path,
                    }
                });
                let marker = if is_bound { "→ " } else { "  " };
                // The marker is part of the COPYABLE text too: the clipboard
                // is how a user takes a path from here into `add`.
                shown.push_str(&format!("{marker}{} — {branch}", entry.path.display()));
                plain.push_str(&format!("{} — {branch}", entry.path.display()));
                if entry.is_main {
                    shown.push_str(" (main checkout)");
                    plain.push_str(" (main checkout)");
                }
                shown.push('\n');
                plain.push('\n');
            }
            // One leading line so the marker column is explained without a
            // legend row per entry.
            (
                format!(
                    "{} worktree(s) of {} (→ = current binding):\n{}",
                    entries.len(),
                    repo.display(),
                    shown
                ),
                Some(plain),
                false,
            )
        }
        Err(error) => (format!("{error}"), None, false),
    };
    // Only a git REFUSAL is an error; an empty list is a fine answer.
    let failed = !empty && rows.is_none();
    let options = ToastOptions {
        title: Some("Worktrees".into()),
        message,
        variant: if failed {
            ToastVariant::Error
        } else {
            ToastVariant::Info
        },
        // Long enough to read and copy from, unlike the 6s refusal toasts.
        duration_ms: 30_000,
    };
    match rows {
        Some(plain) => app.toast_state.show_copyable(options, plain),
        None => app.toast_state.show(options),
    }
}

/// The `/worktree` usage toast — the command takes arguments, so an empty
/// or malformed tail is a refusal, not a silent no-op.
fn worktree_usage_toast(app: &mut App) {
    app.toast_state.show(ToastOptions {
        title: Some("Worktree".into()),
        message: "Usage: /worktree add [<path>] <branch> — or /worktree exit, \"/worktree list\""
            .into(),
        variant: ToastVariant::Warning,
        duration_ms: 6000,
    });
}

/// Assign the focused pane a worktree: create it (from the session repo's
/// current HEAD), stamp the binding (`assigned_by: User` — a slash
/// command IS the user acting), flag the handoff (the pane's next turn
/// carries the context summary across the cwd move) and re-sync +
/// persist. A git refusal or an unusable path surfaces as a toast.
fn assign_focused_pane_worktree(app: &mut App, path: Option<&str>, branch: &str) {
    let repo = std::path::PathBuf::from(&app.state.working_directory);
    let explicit = path.map(std::path::PathBuf::from);
    // The focused INDEX only: the duplicate-binding check below has to read
    // every OTHER pane's binding, so the split cannot be held mutably yet.
    let Some(pane_index) = app.agents_split.as_ref().and_then(|split| split.focused) else {
        return;
    };
    if app
        .agents_split
        .as_ref()
        .is_none_or(|split| pane_index >= split.panes.len())
    {
        return;
    }
    match morpheus::git::worktree::create_or_adopt(
        &repo,
        branch,
        explicit.as_deref(),
        morpheus::git::worktree::AssignedBy::User,
    ) {
        Ok((worktree, binding)) => {
            // Refuse a checkout ANOTHER agent already works in. Two loops in
            // one tree edit each other's files — the failure pane isolation
            // exists to prevent — and this became reachable once adoption
            // replaced the `PathExists` refusal that used to block a second
            // bind at the same path.
            if let Some(holder) = worktree_holder(app, &worktree.path, Some(pane_index), false) {
                app.toast_state.show(ToastOptions {
                    title: Some("Worktree".into()),
                    message: format!(
                        "{holder} already works in {} — two agents in one checkout edit each \
                         other's files; pick another path",
                        worktree.path.display()
                    ),
                    variant: ToastVariant::Error,
                    duration_ms: 8000,
                });
                return;
            }
            let adopted = adoption_toast(&worktree, binding);
            let Some(split) = app.agents_split.as_mut() else {
                return;
            };
            let Some(pane) = split.panes.get_mut(pane_index) else {
                return;
            };
            pane.worktree = Some(worktree);
            pane.worktree_handoff_pending = true;
            app.sync_agents_pane_registry();
            persist_worktree_change(app);
            if let Some(message) = adopted {
                app.toast_state.show(ToastOptions {
                    title: Some("Worktree".into()),
                    message,
                    variant: ToastVariant::Info,
                    duration_ms: 8000,
                });
            }
        }
        // Only the REFUSAL speaks: a CREATION is evident from the pane's
        // badge and from the absence of an error toast. An ADOPTION speaks
        // too (see `adoption_toast`) — re-entering a checkout the user
        // forgot about must not look like nothing happened.
        Err(error) => app.toast_state.show(ToastOptions {
            title: Some("Worktree".into()),
            message: format!("{error}"),
            variant: ToastVariant::Error,
            duration_ms: 8000,
        }),
    }
}

/// The one sentence an ADOPTION says, and nothing for a creation.
///
/// `/worktree add` now does two different things behind one grammar, and the
/// user cannot see which from the badge alone: a creation leaves a new
/// checkout, an adoption re-enters one that already existed (typically after
/// `/worktree exit`, which keeps the checkout on disk by product rule). A
/// silent adoption would make the command a lie about what it built.
fn adoption_toast(
    worktree: &morpheus::git::worktree::PaneWorktree,
    binding: morpheus::git::worktree::Binding,
) -> Option<String> {
    match binding {
        morpheus::git::worktree::Binding::Created => None,
        morpheus::git::worktree::Binding::Adopted => Some(format!(
            "Attached to existing worktree {} ({})",
            worktree.path.display(),
            worktree.branch
        )),
    }
}

/// Which agent ALREADY works in `path`, if any: the MAIN agent's session
/// binding, or another pane's. `except_pane` skips the pane being re-bound,
/// so a second `add` on the SAME pane stays allowed (it is how a pane moves
/// between checkouts).
///
/// Two loops in one checkout edit each other's files — the exact failure
/// pane isolation exists to prevent. `PathExists` used to make this
/// unreachable by ACCIDENT: a second bind at the same path simply failed,
/// with a message about the directory, not about the other agent. Adoption
/// deliberately removed that refusal, so the guard has to be explicit and
/// has to name the holder: "pick another path" is useless without saying
/// who is already there.
///
/// The PARKED split counts. Its panes still hold their bindings, and
/// re-opening it must not put one back into a tree a live agent took over.
fn worktree_holder(
    app: &App,
    path: &std::path::Path,
    except_pane: Option<usize>,
    except_main: bool,
) -> Option<String> {
    use morpheus::git::worktree::same_path;
    // `except_main` when the caller IS the main agent: re-pinning itself at
    // the same checkout is a no-op, not a conflict with itself.
    let main_bound = !except_main
        && app
            .state
            .current_session()
            .and_then(|session| session.main_agent_worktree.as_ref())
            .is_some_and(|worktree| same_path(&worktree.path, path));
    if main_bound {
        return Some("The main agent".to_string());
    }
    // LIVE first (its indices are the ones `except_pane` refers to), then the
    // parked set, which has no focused pane and therefore no exception.
    for (slot, split) in [app.agents_split.as_ref(), app.parked_agents_split.as_ref()]
        .into_iter()
        .enumerate()
    {
        let Some(split) = split else {
            continue;
        };
        for (index, pane) in split.panes.iter().enumerate() {
            if slot == 0 && except_pane == Some(index) {
                continue;
            }
            let bound = pane
                .worktree
                .as_ref()
                .is_some_and(|worktree| same_path(&worktree.path, path));
            if bound {
                return Some(format!("Pane {} ({})", index + 1, pane.agent));
            }
        }
    }
    None
}

/// The MAIN agent's effective working directory: its `/worktree` binding
/// when pinned, the session's startup cwd otherwise. `working_directory` is
/// set once in `App::new` and never mutated by the running app, so it IS the
/// original cwd — `/worktree exit` needs no saved copy, it just drops the
/// binding.
///
/// Deliberately SEPARATE from `pane_effective_cwd`: the main agent is not a
/// pane (no registry id, no `AgentPane`), and the binding lives on the
/// session because it must survive a reopen. Both this and the `&AppState`
/// consumers the footer and file explorer hold resolve through
/// [`AppState::main_agent_cwd`], so the crate has exactly ONE definition of
/// "where the main agent works".
pub(super) fn main_agent_effective_cwd(app: &App) -> PathBuf {
    app.state.main_agent_cwd()
}

/// Pin the MAIN agent into a worktree (`/worktree add` with no `/agents`
/// split open). Same engine, same `assigned_by: User` (a slash command IS
/// the user acting) and same all-or-nothing refusal handling as the pane
/// path — only the binding's home differs: the session, not a pane slot.
fn assign_main_agent_worktree(app: &mut App, path: Option<&str>, branch: &str) {
    // The repo is ALWAYS the startup cwd, never the current binding: a
    // second `add` must branch off the real repository, not off the
    // checkout the main agent is sitting in.
    let repo = PathBuf::from(&app.state.working_directory);
    let explicit = path.map(PathBuf::from);
    match morpheus::git::worktree::create_or_adopt(
        &repo,
        branch,
        explicit.as_deref(),
        morpheus::git::worktree::AssignedBy::User,
    ) {
        Ok((worktree, binding)) => {
            // Same guard as the pane path: a checkout another agent already
            // works in is refused. `except_main` is true because re-pinning
            // the main agent at ITS OWN checkout is a no-op, not a
            // self-conflict.
            if let Some(holder) = worktree_holder(app, &worktree.path, None, true) {
                app.toast_state.show(ToastOptions {
                    title: Some("Worktree".into()),
                    message: format!(
                        "{holder} already works in {} — two agents in one checkout edit each \
                         other's files; pick another path",
                        worktree.path.display()
                    ),
                    variant: ToastVariant::Error,
                    duration_ms: 8000,
                });
                return;
            }
            let adopted = adoption_toast(&worktree, binding);
            if let Some(session) = app.state.current_session_mut() {
                session.main_agent_worktree = Some(worktree);
            }
            persist_main_agent_worktree(app);
            if let Some(message) = adopted {
                app.toast_state.show(ToastOptions {
                    title: Some("Worktree".into()),
                    message,
                    variant: ToastVariant::Info,
                    duration_ms: 8000,
                });
            }
        }
        // Only the REFUSAL speaks (same contract as the pane path): a
        // CREATION is evident from the footer badge; an adoption announces
        // itself through the shared `adoption_toast` sentence.
        Err(error) => app.toast_state.show(ToastOptions {
            title: Some("Worktree".into()),
            message: format!("{error}"),
            variant: ToastVariant::Error,
            duration_ms: 8000,
        }),
    }
}

/// Drop the MAIN agent's worktree binding (`/worktree exit`): its next turn
/// runs in the original startup cwd again. Nothing on disk is removed.
fn clear_main_agent_worktree(app: &mut App) {
    let was_pinned = app
        .state
        .current_session()
        .is_some_and(|session| session.main_agent_worktree.is_some());
    if let Some(session) = app.state.current_session_mut() {
        session.main_agent_worktree = None;
    }
    persist_main_agent_worktree(app);
    if !was_pinned {
        app.toast_state.show(ToastOptions {
            title: Some("Worktree".into()),
            message: "The main agent is not in a worktree.".into(),
            variant: ToastVariant::Warning,
            duration_ms: 6000,
        });
    }
}

/// Write the main agent's binding through to the session file. Reuses the
/// metadata delta path, so a reopened session restores the isolation.
fn persist_main_agent_worktree(app: &mut App) {
    let Some(session) = app.state.current_session() else {
        return;
    };
    let session = session.clone();
    app.session_store.save_session(&session);
}

/// The App-side `/worktree` for the MAIN agent (normal mode, no split):
/// the same parsed grammar as the panes, applied to the session's binding.
pub(super) fn apply_main_agent_worktree_command(app: &mut App, tail: Option<&str>) {
    match parse_worktree_command(tail) {
        Some(WorktreeCommand::Add { path, branch }) => {
            assign_main_agent_worktree(app, path.as_deref(), &branch);
        }
        Some(WorktreeCommand::Exit) => clear_main_agent_worktree(app),
        Some(WorktreeCommand::List) => {
            // No split here: the binding in question is the MAIN agent's.
            let bound = app
                .state
                .current_session()
                .and_then(|session| session.main_agent_worktree.as_ref())
                .map(|worktree| worktree.path.clone());
            show_worktree_list(app, bound.as_deref());
        }
        None => worktree_usage_toast(app),
    }
}

/// Drop the focused pane's worktree binding (`/worktree exit`): the pane
/// returns to the session's cwd on its next turn. Nothing on disk is
/// touched — the worktree directory and its branch survive; only the
/// binding is dropped (the name "exit" must not suggest deletion).
fn clear_focused_pane_worktree(app: &mut App) {
    let Some((pane_index, split)) = focused_agent_pane_mut(app) else {
        return;
    };
    let was_pinned = split.panes[pane_index].worktree.is_some();
    let pane = &mut split.panes[pane_index];
    pane.worktree = None;
    pane.worktree_handoff_pending = was_pinned;
    app.sync_agents_pane_registry();
    persist_worktree_change(app);
    if !was_pinned {
        // The only thing worth saying: the command did nothing, and why.
        app.toast_state.show(ToastOptions {
            title: Some("Worktree".into()),
            message: "This pane is not in a worktree.".into(),
            variant: ToastVariant::Warning,
            duration_ms: 6000,
        });
    }
}

impl App {
    /// The App-side `agents_worktree` assign: create the worktree (temp
    /// root — the agent NEVER picks a path), stamp the binding
    /// `AssignedBy::Agent`, flag the handoff (the pane's next turn carries
    /// the context summary across the cwd move) and re-sync + persist —
    /// the SAME side effects as the user's `/worktree add`, difference
    /// being only who is asking. The `Result` IS the op's outcome text for
    /// the asking loop's delivery.
    fn assign_pane_worktree_for_agent(
        &mut self,
        pane_index: usize,
        branch: &str,
    ) -> Result<String, String> {
        let repo = std::path::PathBuf::from(&self.state.working_directory);
        let Some((pane_index, split)) = focused_agent_pane_at_mut(self, pane_index) else {
            return Err("the target pane is not addressable anymore".to_string());
        };
        // The agent NEVER picks a path, so its ask always lands in the
        // managed temp root and always CREATES: `create_or_adopt` is used
        // for the one code path, and the `Binding` is asserted here rather
        // than ignored — an agent ask that silently adopted somebody's
        // checkout would be a bug, not a feature.
        let outcome = match morpheus::git::worktree::create_or_adopt(
            &repo,
            branch,
            None,
            morpheus::git::worktree::AssignedBy::Agent,
        ) {
            Ok((worktree, morpheus::git::worktree::Binding::Created)) => {
                let agent_label = split.panes[pane_index].agent.clone();
                let message = format!(
                    "Pane isolated: {agent_label} now works in {} (branch {}).",
                    worktree.path.display(),
                    worktree.branch
                );
                let pane = &mut split.panes[pane_index];
                pane.worktree = Some(worktree);
                pane.worktree_handoff_pending = true;
                Ok(message)
            }
            // Unreachable by construction (the branch above passes no path,
            // and adoption requires one), but handled EXPLICITLY rather than
            // with a wildcard so the invariant is stated instead of assumed:
            // if a future change ever lets an agent ask for a path, this
            // arm fails loudly rather than quietly reporting an adoption as
            // a creation.
            Ok((worktree, morpheus::git::worktree::Binding::Adopted)) => Err(format!(
                "internal: an agent worktree ask must never adopt (it has no path); got {} ({})",
                worktree.path.display(),
                worktree.branch
            )),
            Err(error) => Err(format!("{error}")),
        };
        if outcome.is_ok() {
            self.sync_agents_pane_registry();
            persist_worktree_change(self);
        } else if let Err(error) = &outcome {
            // Only the REFUSAL speaks (same rule as the user's command): the
            // outcome text itself is the delivery the asking loop receives.
            self.toast_state.show(ToastOptions {
                title: Some("Worktree".into()),
                message: error.clone(),
                variant: ToastVariant::Error,
                duration_ms: 8000,
            });
        }
        outcome
    }

    /// The App-side `agents_worktree` clear: drop the pane's binding (the
    /// pane returns to the session's cwd on its next turn) — nothing on
    /// disk is touched, the worktree directory and its branch survive.
    /// Same side effects and outcome shape as the assign twin.
    fn clear_pane_worktree_for_agent(&mut self, pane_index: usize) -> Result<String, String> {
        let Some((pane_index, split)) = focused_agent_pane_at_mut(self, pane_index) else {
            return Err("the target pane is not addressable anymore".to_string());
        };
        let was_pinned = split.panes[pane_index].worktree.is_some();
        let pane = &mut split.panes[pane_index];
        pane.worktree = None;
        pane.worktree_handoff_pending = was_pinned;
        self.sync_agents_pane_registry();
        persist_worktree_change(self);
        if !was_pinned {
            // A no-op clear is the only case worth a toast.
            self.toast_state.show(ToastOptions {
                title: Some("Worktree".into()),
                message: "This pane is not in a worktree.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 6000,
            });
        }
        if was_pinned {
            Ok("binding cleared — the pane works in the session's working directory again; the worktree and its branch were kept on disk".to_string())
        } else {
            Err("the pane was not in a worktree — nothing to clear".to_string())
        }
    }

    /// The user answered the `agents_worktree` ask in the SESSION
    /// permission box. `Allow`/`AllowOnce` approve (`Allow` also records a
    /// session-wide grant so later asks skip the box); `Deny` is the
    /// DECLINE: the op dies with a plain refusal outcome (the pane keeps
    /// ITS binding — the user's supremacy is the whole point) and NO
    /// binding side effect. Either way the inline box closes and the drain
    /// resumes on the next frame. The pane INDEX is re-resolved from the
    /// op's registry id AT ANSWER TIME: a split that changed while the box
    /// was open (pane closed, panes reordered) must never redirect the
    /// binding to a different pane — a vanished target fails the op
    /// cleanly instead.
    pub(in crate::app) fn resolve_pending_worktree_permission(
        &mut self,
        action: crate::routes::session::permission::PermissionAction,
    ) {
        use crate::routes::session::permission::PermissionAction as PermAction;
        let Some(op) = self.pending_worktree_confirmation.take() else {
            return;
        };
        self.permission_dialog.visible = false;
        self.permission_dialog.request = None;
        // Re-show a harness permission request that was parked while this
        // ask held the box: the main loop's own permission gate is next.
        if let Some(held) = self.pending_permission_request.take() {
            self.permission_dialog.request = Some(held);
            self.permission_dialog.visible = true;
            self.permission_dialog.selected = 1;
        }
        let approved = matches!(action, PermAction::Allow | PermAction::AllowOnce);
        // Re-resolve by the op's stamped IDENTITY, not by the registry
        // label: a label can be recycled onto a DIFFERENT pane by a
        // sibling's removal while the box sat open, and the answer must
        // never redirect onto that pane (Issue 3).
        let pane_index = either_split(&self.agents_split, &self.parked_agents_split)
            .and_then(|split| split.pane_index_by_id(op.coworker_id));
        let outcome = match (approved, pane_index) {
            (true, Some(pane_index)) => {
                // The session-wide grant latches ONLY on a real approval of
                // an executable ask: a target that vanished grants nothing.
                if action == PermAction::Allow {
                    self.worktree_agent_grant = true;
                }
                match op.action {
                    morpheus::harness::agent_coworkers::WorktreeAction::Assign => {
                        let branch = op.branch.clone().unwrap_or_default();
                        self.assign_pane_worktree_for_agent(pane_index, &branch)
                    }
                    morpheus::harness::agent_coworkers::WorktreeAction::Clear => {
                        self.clear_pane_worktree_for_agent(pane_index)
                    }
                }
            }
            (true, None) => Err(format!(
                "pane '{}' no longer exists — the binding cannot change",
                op.coworker
            )),
            (false, _) => Err(format!(
                "the user DECLINED this ask — pane '{}' keeps its current worktree binding; \
                 do not retry the same ask",
                op.coworker
            )),
        };
        morpheus::harness::agent_coworkers::complete_worktree_op(&op, outcome);
        if !self.permission_dialog.visible {
            self.prompt_view.focus();
        }
    }

    /// Test/inspection accessor for the session-wide worktree grant.
    #[cfg(test)]
    pub(in crate::app) fn worktree_agent_grant(&self) -> bool {
        self.worktree_agent_grant
    }

    /// Test/inspection accessor for the parked harness permission request.
    #[cfg(test)]
    pub(in crate::app) fn pending_permission_request(
        &self,
    ) -> Option<&crate::routes::session::permission::PermissionRequest> {
        self.pending_permission_request.as_ref()
    }
}

/// The split + pane index for an AGENT-driven worktree change, resolved
/// from whichever slot owns the panes (live OR parked — the parked split
/// keeps serving `agents_call`, so an agent ask can legitimately target a
/// pane while the mode is closed, and the confirmation resolver resolves
/// its index through the same `either_split` view). Every pane kind is
/// addressable here, the MANAGER included: a
/// morpheus pane runs a real agent session whose assembly takes the same
/// per-pane cwd an ACP pane's does (`pane_effective_cwd`), so isolating it
/// is honored rather than cosmetic. What keeps the user in charge is
/// elsewhere and unchanged: `with_own_coworker_id` strips `agents_worktree`
/// from every pane loop (a pane never rebinds itself or a peer), and a
/// User-assigned binding always goes through the permission box. Only an
/// out-of-range index is refused.
fn focused_agent_pane_at_mut(app: &mut App, pane: usize) -> Option<(usize, &mut AgentsSplitState)> {
    let split = either_split_mut(&mut app.agents_split, &mut app.parked_agents_split)?;
    split.panes.get_mut(pane)?;
    Some((pane, split))
}

/// Persist the session's pane metadata after a worktree binding change:
/// the SAME write-back the picker/teardown paths use (`to_ref` carries the
/// `PaneWorktree` through `AgentPaneRef::Bound`, so a cold restore
/// re-attaches the pane already pinned). The hidden stack rides along
/// unchanged — the delta is the pane set's worktree fields only.
fn persist_worktree_change(app: &mut App) {
    let mode_open = app.agents_split.is_some();
    let panes = either_split(&app.agents_split, &app.parked_agents_split).map(|split| {
        split
            .panes
            .iter()
            .map(crate::routes::session::agents_split::AgentPane::to_ref)
            .collect::<Vec<_>>()
    });
    let hidden = app
        .state
        .current_session()
        .map(|s| s.agents_hidden.clone())
        .unwrap_or_default();
    app.save_agents_metadata(panes, mode_open, hidden);
}

/// The LIVE split's focused pane for mutation — the worktree command is a
/// LIVE-pane command (a parked split cannot take focus, so nothing else
/// can reach it). `None` when no pane is focused. The MANAGER pane is a
/// valid target: its turns run morpheus's own loop, whose assembly takes the
/// SAME per-pane cwd an ACP pane's does (`dispatch_to_pane`), so a binding
/// on it is honored rather than cosmetic.
fn focused_agent_pane_mut(app: &mut App) -> Option<(usize, &mut AgentsSplitState)> {
    let split = app.agents_split.as_mut()?;
    let focused = split.focused?;
    split.panes.get(focused)?;
    Some((focused, split))
}

/// Focus arrived on pane `pane` (hotkey or click): when the pane has never
/// collected its agent's command/config snapshot, spawn the on-demand ACP
/// probe (`session_snapshot` — resume + observe, never prompts) and flag
/// the slash menu's spinner until the result lands. The manager pane is
/// skipped: it is morpheus itself, and the morpheus command set already serves the
/// menu (its slash commands are the TUI's own, not an ACP publication).
/// The LIVE split only — a parked pane cannot take focus.
pub(super) fn ensure_pane_snapshot(app: &mut App, pane: usize) {
    let Some(split) = app.agents_split.as_mut() else {
        return;
    };
    let Some(p) = split.panes.get_mut(pane) else {
        return;
    };
    if p.agent == MANAGER_LABEL
        || (p.commands.is_some() && p.config_options.is_some())
        || p.snapshot_loading
        // A pane with a live session owns its connection: a probe must never
        // open a second one against the same session (the harnesses answer
        // "already active elsewhere"). Chrome arrives with the turn stream.
        || p.acp_session.is_some()
    {
        return;
    }
    // The probe observes the pane's EFFECTIVE cwd (worktree when pinned):
    // commands/config options are per-connection, per-cwd state — the
    // same resolution the turn dispatch applies (`pane_effective_cwd`).
    let pane_probe_cwd = pane_effective_cwd(p, &app.state.working_directory);
    let agent = p.agent.clone();
    let session_id = p.session_id.clone();
    let pane_id = p.id;
    p.snapshot_loading = true;
    if split.focused == Some(pane) {
        app.slash_menu.set_agent_commands(Vec::new());
        app.slash_menu.set_loading(true);
    }
    let cwd = pane_probe_cwd;
    let event_tx = app.event_tx.clone();
    let epoch = app.agents_pane_epoch;
    app.tokio_handle.spawn(async move {
        let result = morpheus_tools::subagent::acp::session_snapshot(&agent, session_id, cwd).await;
        let (commands, options, modes, mode_current) = match result {
            Ok(snapshot) => (
                snapshot.commands,
                snapshot.config_options,
                snapshot.modes,
                snapshot.current_mode,
            ),
            Err(err) => {
                let _ = event_tx.send(HarnessEvent::Toast {
                    message: format!("Command list for {agent} unavailable: {err}"),
                    variant: morpheus::harness::events::ToastVariant::Info,
                });
                (Vec::new(), Vec::new(), Vec::new(), String::new())
            }
        };
        let _ = event_tx.send(HarnessEvent::AgentsPaneEvent {
            pane,
            pane_id,
            epoch,
            // Chrome, not timeline content: belongs to no turn.
            turn: 0,
            event: SubagentEvent::Commands { commands },
        });
        let _ = event_tx.send(HarnessEvent::AgentsPaneEvent {
            pane,
            pane_id,
            epoch,
            turn: 0,
            event: SubagentEvent::ConfigOptions { options },
        });
        let _ = event_tx.send(HarnessEvent::AgentsPaneEvent {
            pane,
            pane_id,
            epoch,
            turn: 0,
            event: SubagentEvent::Modes {
                modes,
                current: mode_current,
            },
        });
    });
}

/// The pane set, live or parked (live preferred): the split keeps
/// serving turns with the mode closed, so every pane reader resolves
/// whichever slot currently owns the panes.
///
/// Free functions — not methods — on purpose: the borrow covers only the
/// two slots, while callers routinely hold the split across mutations of
/// the rest of `App` (e.g. the right-panel mirrors).
pub(super) fn either_split<'a>(
    live: &'a Option<AgentsSplitState>,
    parked: &'a Option<AgentsSplitState>,
) -> Option<&'a AgentsSplitState> {
    live.as_ref().or(parked.as_ref())
}

/// Mutable half of [`either_split`].
pub(super) fn either_split_mut<'a>(
    live: &'a mut Option<AgentsSplitState>,
    parked: &'a mut Option<AgentsSplitState>,
) -> Option<&'a mut AgentsSplitState> {
    live.as_mut().or(parked.as_mut())
}

impl App {
    /// Refresh the slash menu from its input — ALWAYS after re-deriving the
    /// source. Every site that used to call `slash_menu.update(...)` directly
    /// routes through here instead: with the menu open, the source must be
    /// re-derived against the CURRENT split state on EVERY recalculation, not
    /// only on focus changes. This closes the leak class where a stale source
    /// (the split opened/closed/parked between two menu updates) let the morpheus
    /// command list render over a focused agent pane — or an agent's list over
    /// the main agent — and a pick then executed against the WRONG harness.
    pub(in crate::app) fn slash_menu_refresh(&mut self) {
        sync_slash_source(self);
        self.slash_menu.update(&self.prompt_view.input);
    }

    /// The current session's durable input history (main-agent prompts,
    /// queued messages and `/agents` pane dispatches), oldest first.
    ///
    /// A session recorded before the history was persisted carries only its
    /// transcript, so an empty list is seeded from the user messages once —
    /// in memory only; the next recorded input persists the seed together
    /// with itself. Reading the SESSION (not an App-level buffer) is what
    /// makes the list survive a restart and a session switch.
    pub(crate) fn input_history(&mut self) -> Vec<String> {
        let seed = match self.state.current_session() {
            Some(session) if !session.input_history.is_empty() => {
                return session.input_history.clone();
            }
            Some(session) => crate::component::prompt::PromptView::user_message_texts(session),
            None => return Vec::new(),
        };
        if seed.is_empty() {
            return seed;
        }
        if let Some(session) = self.state.current_session_mut() {
            session.input_history = seed.clone();
        }
        seed
    }

    /// Record a user input (main prompt, queued message or pane dispatch)
    /// into the current session's input history, in send order, and persist
    /// it immediately.
    ///
    /// A pane input belongs to another agent's conversation and never
    /// enters this session's transcript, so this list is the only record of
    /// it: waiting for the next regular save would lose it whenever the
    /// process exits first. The write is synchronous (one short metadata
    /// delta) for the same reason.
    pub(crate) fn record_user_input(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        // A legacy session (no history yet) records this input on top of the
        // transcript's messages, so the walk keeps the earlier prompts.
        let seed = match self.state.current_session() {
            Some(session) if session.input_history.is_empty() => Some(
                crate::component::prompt::PromptView::user_message_texts(session),
            ),
            Some(_) => None,
            // No session yet (the very first prompt of a fresh app):
            // `start_agent_loop` creates the session and puts this message on
            // the transcript, where the next seed picks it up.
            None => return,
        };
        let Some(session) = self.state.current_session_mut() else {
            return;
        };
        if let Some(seed) = seed {
            session.input_history = seed;
        }
        session.input_history.push(text.to_string());
        self.session_store.save_session(session);
    }

    /// Force-close ONE pane's turn (the Ctrl+X interrupt): the pane goes
    /// idle, its timeline closes with a `[turn cancelled]` marker carrying
    /// `reason`, its in-flight `agents_call` request fails (the requester's
    /// busy-guard unblocks), and the shared mirror seals as failed. A real
    /// terminal Done from a turn still winding down arrives against an IDLE
    /// pane and is dropped by the handler's pane-idle guard — no double
    /// closure, no re-binding, no double-completed request.
    ///
    /// Recovery never depends on the turn thread answering: the stop
    /// signal is stored first (best-effort graceful cancel — ACP sends
    /// `session/cancel`, the manager loop breaks out), then the state is
    /// closed unconditionally. This is the pane's ONLY keyboard stop path.
    fn force_close_pane(&mut self, pane: usize, reason: &str) {
        // The registry request fails keyed by the pane's id, read BEFORE the
        // exclusive state borrow below.
        let registry_id = either_split(&self.agents_split, &self.parked_agents_split)
            .and_then(|split| split.pane_registry_id(pane));
        let Some(split) = either_split_mut(&mut self.agents_split, &mut self.parked_agents_split)
        else {
            return;
        };
        let Some(p) = split.panes.get_mut(pane) else {
            return;
        };
        if !p.running {
            return;
        }
        if let Some(signal) = &p.stop_signal {
            signal.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        p.running = false;
        p.stop_signal = None;
        // The stopped turn's steering handle dies with it, and its terminal
        // Done is refused (see the generation bump below), so nothing would
        // ever recover what the handle still held. The messages the user
        // chose "next request" for belong to the PANE: they go back to the
        // head of its `next request` queue, exactly as a turn that ended on
        // its own recovers them (`serve_agents_pane_user_queues`).
        if let Some(steer) = p.steer.take() {
            for text in steer.drain() {
                p.queued.next_request.push_back(text);
            }
        }
        // Retire the turn this pane was running: the stop is best-effort
        // (the ACP call sends `session/cancel` and the manager loop breaks
        // out of its next check), so the thread keeps winding down for a
        // while. Bumping the generation HERE — before the queues below can
        // dispatch the pane's next turn in this same frame — is what keeps
        // that winding-down turn's late events and terminal Done from being
        // applied to the turn that replaced it.
        let _ = split.begin_turn(pane);
        // A force-closed turn drops its pending permission box and answer
        // channel: the ACP handler answers `Cancelled` on the closed
        // channel (the spec's contract for a cancelled turn's pending
        // permission), so the agent-side request resolves cleanly.
        split.clear_permission(pane);
        // Ctrl+X IS the user's stop: `cancelled` here is the one place the
        // word is literal (see `AgentCoworkerTurnEnd`). The forced close must not
        // pretend the pane failed on its own — that is exactly the
        // conflation this vocabulary removes.
        split.finish_turn(
            pane,
            reason,
            morpheus::harness::agent_coworkers::AgentCoworkerTurnEnd::Cancelled,
        );
        // The force-close DROPS the winding-down turn's terminal Done (the
        // pane-idle gate in the event handler), which used to drop its
        // session binding with it — the stale-binding bug (issue 1,
        // variant C). The turn's gate-free fact is absorbed HERE so the
        // pane (and its persisted ref) carries the session the turn
        // actually drove.
        self.absorb_pane_session_bindings();
        if let Some(id) = registry_id {
            // Ctrl+X is the user's stop: report it as `cancelled`, never as
            // an infrastructure error — `report_block`'s `Cancelled`
            // guidance is what stops the requester from re-running the pane
            // the user just halted. The requester tag honors the wake
            // promise (a MAIN-requested background call parks a delivery
            // and must wake the idle main agent), and the freed pane's next
            // queued message dispatches in this same frame — the same
            // follow-through as the normal `AgentsPaneDone` handler.
            let requester =
                morpheus::harness::agent_coworkers::cancel_serving(&id, reason.to_string());
            // The user's own queue first: a message the human sent while
            // that pane was busy must not lose to an agent's `agents_call`.
            self.serve_agents_pane_user_queues();
            self.serve_agents_pane_queues();
            // Absorb ONCE MORE after the dispatch: the first absorb above
            // ran before any queued message was served, but the winding-
            // down turn's fact may land in between — and `dispatch_to_pane`
            // snapshots `p.session_id` as its resume id. Without this
            // second pass, a queued request served in that window could
            // resume the STALE session (issue 1's stale-resume window
            // narrowed to zero width best-effort: facts that arrived by
            // now bind before the next frame can dispatch anything else;
            // a fact that lands even later is caught by the frame tick's
            // absorb pass before any further dispatch).
            self.absorb_pane_session_bindings();
            self.wake_main_agent_if_promised(requester);
        }
        self.finish_agents_pane_mirror(pane, true);
    }

    /// Answer the FOCUSED pane's pending permission request from the
    /// keyboard: navigate with Up/Down (the box's highlighted row lives on
    /// the pane state), Enter sends the selected option's id, Esc sends
    /// the ACP `Cancelled` semantic. Returns whether a box was pending —
    /// the caller consumes the key only then (sovereign over the prompt
    /// and every other handler, like the main dialog's gate).
    pub(super) fn pane_permission_key(&mut self, key: KeyCode) -> bool {
        let Some(focused) = self.agents_split.as_ref().and_then(|split| split.focused) else {
            return false;
        };
        let Some(request) = self
            .agents_split
            .as_ref()
            .and_then(|split| split.pending_permission(focused))
        else {
            return false;
        };
        let count = request.options.len().max(1);
        match key {
            KeyCode::Up => {
                let Some(split) = self.agents_split.as_mut() else {
                    return true;
                };
                if let Some(p) = split.panes.get_mut(focused)
                    && let Some(req) = p.pending_permission.as_mut()
                {
                    req.selected = (req.selected + count - 1) % count;
                }
                true
            }
            KeyCode::Down => {
                let Some(split) = self.agents_split.as_mut() else {
                    return true;
                };
                if let Some(p) = split.panes.get_mut(focused)
                    && let Some(req) = p.pending_permission.as_mut()
                {
                    req.selected = (req.selected + 1) % count;
                }
                true
            }
            KeyCode::Enter => {
                let option_id = request.options.get(request.selected).map(|o| o.id.clone());
                let Some(split) = self.agents_split.as_mut() else {
                    return true;
                };
                match option_id {
                    Some(id) => {
                        split.answer_pending_permission(
                            focused,
                            morpheus_tools::subagent::events::PanePermissionAnswer::Selected(id),
                        );
                    }
                    None => {
                        // No options offered: the only legal response is the
                        // cancellation.
                        split.answer_pending_permission(
                            focused,
                            morpheus_tools::subagent::events::PanePermissionAnswer::Cancel,
                        );
                    }
                }
                true
            }
            KeyCode::Esc => {
                if let Some(split) = self.agents_split.as_mut() {
                    split.answer_pending_permission(
                        focused,
                        morpheus_tools::subagent::events::PanePermissionAnswer::Cancel,
                    );
                }
                true
            }
            _ => true,
        }
    }

    /// Ctrl+X on the split: force-close the FOCUSED pane's turn. With
    /// split-Esc park-only, a pane whose stream
    /// stalled silently had no keyboard path out — the pane sat `running`
    /// forever and the dispatch gate refused every new prompt. Ctrl+X is
    /// that path: the pane goes idle immediately and accepts prompts again.
    /// An idle focused pane (or no pane focused — the MAIN agent, which
    /// plain Esc interrupts in the normal view) is a no-op.
    pub(super) fn interrupt_focused_pane(&mut self) {
        let Some(focused) = self.agents_split.as_ref().and_then(|split| split.focused) else {
            return;
        };
        self.force_close_pane(focused, "interrupted by the user (Ctrl+X)");
    }

    /// Send a submitted prompt to the FOCUSED pane of the active split —
    /// or, when NO pane holds the focus (`Alt+0`), to the MAIN agent
    /// itself (the same normal-mode loop, running as manager while the
    /// split is open). The focus IS the routing decision: the digit
    /// hotkeys / a click select which agent receives the message, so a
    /// message never propagates to every pane at once. A pane starts its
    /// own turn on its live session (ACP panes reuse it across turns;
    /// morpheus panes are rebuilt per turn); the shared prompt is cleared by
    /// the caller.
    pub(super) fn dispatch_to_agents_split(&mut self, msg: String) {
        let Some(split) = self.agents_split.as_ref() else {
            return;
        };
        let Some(focused) = split.focused else {
            // Unfocused (`Alt+0`): the message goes to the MAIN agent —
            // the plain normal-mode loop on the current session (unchanged
            // by the split; no manager prompt or tool set).
            self.submit_prompt_message(msg);
            return;
        };
        // A focused pane turn carries text only (no images); drop any
        // pending images up front so a refused submission cannot leak them.
        self.pending_user_images.clear();
        // A compaction in flight has the SAME exclusivity as a running
        // turn — worse: its persist would race the turn's persist over the
        // same session record. The user's prompt is refused with the same
        // toast shape the busy gate uses (finding 1).
        if let Some(pane) = split.focused_pane()
            && morpheus::harness::agent_coworkers::coworker_is_compacting(pane.id)
        {
            self.toast_state.show(ToastOptions {
                title: Some("Agents".into()),
                message: format!(
                    "'{}' is being compacted — try again when it finishes.",
                    pane.id
                ),
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
            return;
        }
        if split.focused_pane().is_some_and(|pane| pane.running) {
            // Same gate the `/new` command applies to a working loop: a
            // second dispatch while that pane's turn is live would
            // interleave two streams inside one pane. (Issue 3 made
            // split-Esc park-only — it no longer stops a pane's turn — so
            // the toast must not claim Esc stops anything.)
            self.toast_state.show(ToastOptions {
                title: Some("Agents".into()),
                message: "The focused agent is still working.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
            return;
        }
        if split.focused_pane().is_some_and(|pane| pane.rehydrating) {
            // The pane's bound history is still arriving from the agent's
            // `session/load` replay: a prompt now would interleave with
            // the reconstruction stream (and resume the session from a
            // half-replayed state). It is a one-pass, seconds-scale wait.
            self.toast_state.show(ToastOptions {
                title: Some("Agents".into()),
                message: "Restoring this agent's history — try again in a moment.".into(),
                variant: ToastVariant::Info,
                duration_ms: 4000,
            });
            return;
        }
        self.record_user_input(&msg);
        self.dispatch_to_pane(focused, msg, false);
    }

    /// The focused pane's stable id — plus whether it has a LIVE loop — WHEN
    /// that pane cannot take a submission right now, or `None` when no pane is
    /// focused or the focused pane is free.
    ///
    /// "Busy" is the union of the three states that refuse a turn: a live
    /// turn, a compaction in flight (same session-record exclusivity — its
    /// persist would race the turn's) and a cold-restore replay (the bound
    /// history is still arriving). The submit paths QUEUE the message for all
    /// three instead of refusing it.
    ///
    /// "Live loop" (the second field) is what makes the dialog offer the
    /// "next request" row: a running pane has a loop that the message can
    /// become the next request of, whatever kind of agent runs it. A pane
    /// that is only compacting or replaying has none — its next turn is a
    /// fresh loop, so only "next loop" applies there.
    pub(super) fn focused_busy_pane(&self) -> Option<(u64, bool)> {
        let pane = self.agents_split.as_ref()?.focused_pane()?;
        let busy = pane.running
            || pane.rehydrating
            || morpheus::harness::agent_coworkers::coworker_is_compacting(pane.id);
        busy.then_some((pane.id, pane.running))
    }

    /// Dispatch ONE message to pane `pane` as its next turn — the shared
    /// entry point for BOTH senders: the user's submit (the focused pane,
    /// `from_agent = false`) and the `agents_call` queue service (an agent's
    /// input that waited for the pane to free, `from_agent = true`). The
    /// turn's input echoes into the pane's timeline as a FRESH entry right
    /// before the turn starts — the user's prompt with the brand rail,
    /// another agent's input with the distinct rail — so the pane shows WHO
    /// is speaking even while the turn's stream has yet to produce its
    /// first chunk.
    fn dispatch_to_pane(&mut self, pane: usize, msg: String, from_agent: bool) {
        // Snapshot everything the spawned turn needs BEFORE any effect
        // below: the agent name, resume id and the manager pane's connector
        // config (only when the focused pane IS the manager pane — the connector
        // resolution runs lazily so a split of ACP agents alone never
        // computes it).
        let Some((agent, resume, picks, pane_cwd, handoff, rewind, coworker_identity)) =
            either_split(&self.agents_split, &self.parked_agents_split)
                .and_then(|split| split.panes.get(pane))
                .map(|p| {
                    (
                        p.agent.clone(),
                        p.session_id.clone(),
                        // The pane's IDENTITY (`AgentPane::id`): the
                        // awareness preamble keys its once-per-generation
                        // bookkeeping on it — a label recycled by a
                        // sibling's removal must announce from scratch
                        // (Issue 3).
                        // The user's picks from the /agents config boxes,
                        // mirrored into the pane's config cache (the box
                        // confirm writes the pick onto the cached option's
                        // current AND records the id in `picked_config_ids`).
                        // Only USER-PICKED ids ride the turn — ACP config
                        // options are per-connection state, so
                        // `apply_session_picks` re-applies them inside the
                        // turn's own connection. Untouched options keep the
                        // harness's own default, letting `acp::call`'s
                        // registry default (e.g. a working free model where
                        // the harness's default is a paid-tier login wall)
                        // still apply.
                        // The picks ride in precedence order (see
                        // [`morpheus::coordinator::turn::turn_picks`]): the pane's own user pick
                        // first, then the persisted per-agent pick, and the
                        // registry default still applies when neither exists.
                        morpheus::coordinator::turn::turn_picks(
                            p.config_options.as_deref(),
                            &p.picked_config_ids,
                            self.agents_model_picks.get(&p.agent),
                        ),
                        // The pane's EFFECTIVE cwd: the worktree path when
                        // the pane is pinned (`/worktree`), the session's
                        // working directory otherwise. Every layer of the
                        // isolation model rides THIS single resolution —
                        // the ACP session cwd, the MANAGER turn's
                        // `HarnessAssembly::cwd` (harness root, MorpheusTools
                        // root, LSP, hooks) and the fs sandbox root all
                        // derive from it inside the turn.
                        pane_effective_cwd(p, &self.state.working_directory),
                        // The handoff summary, built NOW from the pane's
                        // local timeline: when the pane's cwd moved since
                        // its last turn, the next prompt carries this block
                        // so the agent keeps its working context across the
                        // move (the fresh-session fallback of
                        // resume-with-new-cwd is the failure mode it nets).
                        p.worktree_handoff_pending
                            .then(|| pane_handoff_summary(&p.activity.timeline)),
                        // The REWIND handoff: a pane Revert that had to
                        // abandon the agent session (ACP) leaves the kept
                        // prefix here, so the fresh session starts
                        // oriented instead of blind.
                        p.rewind_context.clone(),
                        p.id,
                    )
                })
        else {
            return;
        };
        // The client's harness mode mapped onto one of the pane's advertised
        // ACP session modes (similarity stems, `agent_command_map`). The
        // binding re-applies EVERY turn (per-connection session state), so
        // the map result at dispatch time is what this turn binds. `None`:
        // an unmapped morpheus mode, or an agent that advertises no modes — the
        // agent keeps its own current (the Command-mode omission's
        // counterpart). Yolo deliberately maps to nothing: the split's
        // auto-approve IS the Yolo contract, and the permission-widening
        // agent modes are the same thing for free.
        let own_mode = self.state.mode;
        let mapped_mode = either_split(&self.agents_split, &self.parked_agents_split)
            .and_then(|split| split.panes.get(pane))
            .and_then(|p| p.agent_modes.as_ref())
            .and_then(|modes| {
                crate::routes::session::agent_command_map::map_mode_to_agent(own_mode, modes)
            });
        let is_manager_pane = agent == MANAGER_LABEL;
        // The registry id this pane answers to while THIS turn runs. For the
        // manager pane it is the id `agents_call` would use to reach it; the
        // manager harness carries it so it refuses to address itself (a
        // self-call would queue behind the very turn making it — deadlock).
        // The ACP branch uses the same id for the awareness preamble
        // (`agent_coworkers::take_preamble` keys the announced-generation bookkeeping
        // by it), so it is computed for EVERY pane, not just the manager.
        let pane_registry_id = either_split(&self.agents_split, &self.parked_agents_split)
            .and_then(|split| split.pane_registry_id(pane));
        let own_registry_id = if is_manager_pane {
            pane_registry_id.clone()
        } else {
            None
        };
        // Fallible pre-flight BEFORE any bookkeeping: minting (or reusing)
        // the manager pane's own morpheus session can fail (I/O), and a refused
        // turn must leave the pane idle — never stranded `running` with a
        // spinner and a mirror spinning.
        let manager_session: Option<String> = if is_manager_pane {
            let Some(id) = self.ensure_manager_pane_session(pane) else {
                return;
            };
            Some(id)
        } else {
            None
        };
        // The turn's input echoes into the pane's timeline as a FRESH
        // entry right before the turn starts, so the pane shows WHO is
        // speaking even while the turn's stream has yet to produce its
        // first chunk: the user's own prompt with the brand rail, another
        // agent's queued input with the distinct rail. After the guards
        // above — a refused turn must not write to the timeline either.
        // The echo goes wherever the pane lives (live or parked).
        let handoff_consumed = handoff.is_some();
        let rewind_consumed = rewind.is_some();
        if let Some(split) = either_split_mut(&mut self.agents_split, &mut self.parked_agents_split)
        {
            split.echo_turn_input(pane, &msg, !from_agent);
            // The handoff block rode THIS turn's input — consume the flag
            // so later turns go out without it (a new `/worktree` add/exit
            // re-flags it).
            if handoff_consumed {
                split.set_worktree_handoff_pending(pane, false);
            }
            // The rewind block is a one-shot too: it repaired the FIRST
            // post-revert prompt (the fresh session's opening context).
            if rewind_consumed {
                split.set_rewind_context(pane, None);
            }
        }
        // The pane's assembly inputs come from the SAME user config the
        // main loop reads: the session route (provider + model, `auto`
        // resolved through the full fallback chain by the assembly) plus the
        // router's fallbacks. The old hand-cut subset forced Native and
        // froze the first fallback — the assembly honors the user's choices.
        // An EMPTY provider degrades to the router's first fallback (the
        // deleted `daemon_connector`'s behavior): an unconfigured manager route
        // must not fail the pane when a working fallback exists.
        // The pane's OWN model selection (the per-pane /models pick) wins
        // over the global config; a pane that never picked falls back to it
        // — the global model at spawn time, the pre-feature behavior.
        // `auto` (empty provider) degrades to the fallback chain below,
        // exactly like the global config's auto.
        let pane_selection = if is_manager_pane {
            either_split(&self.agents_split, &self.parked_agents_split)
                .and_then(|split| split.panes.get(pane))
                .and_then(|p| self.manager_pane_model_selection(p))
        } else {
            None
        };
        let manager_route = if is_manager_pane {
            let (provider, model) = match pane_selection.as_ref() {
                Some(selection) => (
                    selection.provider.trim().to_string(),
                    Some(selection.model.clone()),
                ),
                None => (
                    self.llm_config.provider.trim().to_string(),
                    self.llm_config.model.clone(),
                ),
            };
            if provider.is_empty() {
                self.router_view
                    .fallbacks
                    .first()
                    .map(|fb| (fb.provider.clone(), Some(fb.model.clone())))
            } else {
                Some((provider, model))
            }
        } else {
            None
        };
        let fallbacks: Vec<(String, String)> = self
            .router_view
            .fallbacks
            .iter()
            .map(|fb| (fb.provider.clone(), fb.model.clone()))
            .collect();
        let summarization_models: Vec<(String, String)> = self
            .setup
            .routing
            .summarization_models
            .iter()
            .map(|entry| (entry.provider.clone(), entry.model.clone()))
            .collect();
        let local_base_urls = self.configured_local_base_urls();
        let skills_config = self.setup.skills.clone();
        // The SAME user knobs the main loop's assembly reads (the assembly's
        // contract): reasoning effort, tool-call delivery mode, cache
        // settings, MCP servers, the effective-context budget and RAG —
        // cloned before the turn thread moves in.
        // A pane with its own model selection runs ITS reasoning level too:
        // the selection is the (provider, model, reasoning) triple, and a
        // recorded `None` is a deliberate default that must NOT be replaced
        // by whatever effort the global config carries (switching the main
        // model's reasoning must not touch a pane that picked its own).
        // Panes without a selection keep the global config.
        let self_llm_reasoning = if is_manager_pane {
            match pane_selection.as_ref() {
                Some(selection) => selection.reasoning.clone(),
                None => self.llm_config.reasoning.clone(),
            }
        } else {
            self.llm_config.reasoning.clone()
        };
        let self_tool_call_mode = self.llm_config.tool_call_mode;
        let self_base_urls = local_base_urls.clone();
        // The pane's effective cwd (worktree path when pinned), resolved in
        // the snapshot above — NOT the session's working directory. The
        // manager pane rides the SAME resolution as an ACP pane: its
        // binding feeds the assembly's `cwd` below (harness root, `MorpheusTools`
        // root, LSP, hooks), so `/worktree add` on a morpheus pane really moves
        // its turns into the checkout.
        let cwd_string = pane_cwd.to_string_lossy().into_owned();
        let anthropic_ttl_1h = self.setup.anthropic_cache_ttl_1h();
        let openai_cache_retention = self.setup.openai_cache_retention().map(String::from);
        let mcp_config = self.setup.mcp.clone();
        let effective_context = self.setup.effective_context;
        // No configured RAG databases → `recall: None`: the assembly hides
        // the `recall_search` tool entirely (the main loop's
        // `maybe_disable_recall_tool` contract) instead of carrying a tool
        // whose description promises databases that do not exist.
        #[cfg(feature = "embed")]
        let recall_inputs = if self.rag_view.registry.dbs.is_empty() {
            None
        } else {
            Some((self.recall_suffix(), self.recall_dbs_vec()))
        };
        // The manager session's persistence rides a CLONE: the turn thread
        // outlives the &mut self borrow.
        let store = self.session_store.clone();
        let cwd = pane_cwd;
        let tokio_handle = self.tokio_handle.clone();

        // Pane bookkeeping: the pane's own stop signal (Esc stops it through
        // it) and the shared-loop spinner so the "agent loop running"
        // indicator keeps spinning while the turn streams. The signal lands
        // wherever the pane lives (a PARKED pane's turn must still be
        // stoppable when the user re-enters the mode).
        // The steering handle: the SAME shared queue the main agent's
        // "next request" pump feeds, cloned into this pane's loop. Only a
        // morpheus pane's turn has request boundaries to drain it at; an ACP
        // agent takes one prompt per turn, so its pane keeps `None` and its
        // queued "next request" goes out at the turn boundary instead, from
        // `serve_agents_pane_user_queues` (no prompt in flight to inject
        // into, but the message still becomes the pane's next request).
        let steer = is_manager_pane.then(morpheus::harness::queued_input::QueuedInput::default);
        let signal = Arc::new(AtomicBool::new(false));
        // The TURN stamp: every event of this turn carries it, and a
        // force-closed turn's generation is never reused — so its late
        // events (a stop is best-effort; the thread keeps winding down) can
        // never close the LIVE turn that started in the meantime.
        let turn = either_split_mut(&mut self.agents_split, &mut self.parked_agents_split)
            .map_or(0, |split| split.begin_turn(pane));
        if let Some(split) = either_split_mut(&mut self.agents_split, &mut self.parked_agents_split)
        {
            split.set_running(pane, true);
            split.set_stop_signal(pane, Some(signal.clone()));
            split.set_steer(pane, steer.clone());
        }
        // The pane's mirror in the shared subagent list starts spinning too
        // (same running flag, pane-index routed).
        self.sync_agents_pane_mirrors();
        self.agent_activity = crate::types::AgentActivity::Working;
        self.agent_spinner = Some(crate::component::any_spinner::AnySpinner::new(
            crate::types::AgentActivity::Working.label(),
            &self.theme,
            self.setup.appearance.spinner,
        ));

        let index = pane;
        // The pane's stable identity, stamped into every event of THIS
        // turn: the event handler routes by id (falling back to the epoch
        // + index gate only when the id names no pane), so a pane set
        // REBUILT around a still-running turn (a fresh picker confirm that
        // keeps the pane) keeps receiving the stream at the pane's NEW
        // index, while events of a pane the rebuild dropped find no pane
        // and are dropped.
        let pane_id = either_split(&self.agents_split, &self.parked_agents_split)
            .and_then(|split| split.panes.get(pane))
            .map(|p| p.id)
            .unwrap_or_default();
        // Dispatch-time split-slot generation: stamped into every event of
        // THIS turn, so a slot swap while it runs (fresh confirm, switch,
        // close) invalidates its terminal delivery — the handler drops
        // stale-epoch events instead of binding them into a replacement.
        let epoch = self.agents_pane_epoch;
        let event_tx = self.event_tx.clone();
        if is_manager_pane {
            // The morpheus pane: the MAIN agent itself, running delegated work
            // on its OWN morpheus session — a real, persisted session (visible
            // in the history panel, openable like any other), not a
            // throwaway nested harness. Its tool set is the user's mode
            // minus what a pane cannot do (see
            // `morpheus::coordinator::policy`): the workspace-mutating tools are
            // NOT removed — only `Mode::Manager` owns that restriction. The
            // session was bound by the pre-flight above (a second ensure
            // would just re-read it), so this only unwraps.
            let Some(morpheus_session) = manager_session else {
                return;
            };
            let context = self.session_store.load_context(&morpheus_session);
            // The cwd may have MOVED under this pane (`/worktree add|exit`
            // re-flags the handoff): the manager loop carries no prompt-side
            // preamble layer — its sibling awareness rides the harness's
            // panes context block — so the handoff block is the ONE thing
            // prepended here. Without it the first post-move turn would
            // start in a new checkout with no word about what it was doing.
            let input = match handoff {
                Some(summary) => {
                    format!(
                        "[workspace moved] Your working directory changed to {} — \
                         work only inside it. What you were doing, carried over:\n\n{summary}\n\n",
                        cwd_string
                    )
                }
                None => msg,
            };
            // The manager runs the user's real mode (the mapped ACP mode
            // above only concerns EXTERNAL agents; this pane needs no
            // mapping — it runs the actual harness). The permission channel
            // rides the mode exactly like the ACP panes': a non-Yolo mode
            // wires the harness loop's `PermissionRequest` events into the
            // pane's permission box (the bridge maps them); Yolo keeps the
            // throwaway channel (the loop auto-approves in Yolo mode
            // without ever emitting the request — but a channel that goes
            // nowhere is the safe default either way). Manager counts as
            // Yolo here: its guardrails auto-allow everything beyond its
            // own deny-lists, so no request can ever be emitted.
            let permission_rx = if !matches!(
                own_mode,
                morpheus::harness::Mode::Yolo | morpheus::harness::Mode::Manager
            ) {
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                if let Some(split) =
                    either_split_mut(&mut self.agents_split, &mut self.parked_agents_split)
                {
                    // The pane state holds the ONLY sender: the box's answer
                    // reaches the manager turn through it (the receiver
                    // below forwards to the harness loop's gate).
                    split.set_permission_tx(pane, Some(tx));
                }
                Some(rx)
            } else {
                None
            };
            std::thread::spawn(move || {
                // The pane's ONE assembly: the assembly's inputs, with the
                // pane's declared deviations from the main loop — the
                // mode's normal prompt minus the self-review loop, the
                // MANAGER requester tag, the bound pane id (strips
                // `agents_call`), the pane's OWN steering queue (the user's
                // "next request" messages — see `pane_steer`), NO decision
                // audit (not distributed) and the pane's OWN session id as
                // the cache-affinity key.
                let assembly = morpheus::assembly::HarnessAssembly {
                    provider: manager_route
                        .as_ref()
                        .map(|(p, _)| p.clone())
                        .unwrap_or_default(),
                    model: manager_route.as_ref().and_then(|(_, m)| m.clone()),
                    reasoning: self_llm_reasoning.clone(),
                    tool_call_mode: self_tool_call_mode,
                    base_url: manager_route
                        .as_ref()
                        .and_then(|(p, _)| self_base_urls.get(p).cloned()),
                    local_base_urls: local_base_urls.clone(),
                    fallbacks: fallbacks.clone(),
                    summarization_models: summarization_models.clone(),
                    cwd: cwd_string.clone(),
                    mode: own_mode,
                    disabled_tools: morpheus::coordinator::policy::pane_disabled_tools(
                        &HashSet::new(),
                    ),
                    // The MODE's normal prompt minus the self-review loop:
                    // a pane does delegated work whose deliverable is the
                    // task report — it does not run the review sub-agent
                    // (`subagent_call` is foreground-only here and a pane
                    // must not nest reviews over its delegator's head).
                    // Build/Yolo/Command map to the Build BASE (the full
                    // Build prompt minus `INSTRUCTIONS_BUILD_SELF_REVIEW`),
                    // with Yolo interpolated by the cwd-containment block
                    // (`INSTRUCTIONS_YOLO_BASE`) — tool freedom is intended,
                    // directory drift is not; Ask never had a review
                    // section, so it stays whole.
                    instructions: Some(match own_mode {
                        morpheus::harness::Mode::Build | morpheus::harness::Mode::Command => {
                            morpheus::harness::core::INSTRUCTIONS_BUILD_BASE
                        }
                        morpheus::harness::Mode::Yolo => {
                            morpheus::harness::core::INSTRUCTIONS_YOLO_BASE
                        }
                        morpheus::harness::Mode::Manager => {
                            morpheus::harness::core::INSTRUCTIONS_MANAGER
                        }
                        morpheus::harness::Mode::Ask => morpheus::harness::core::INSTRUCTIONS_ASK,
                    }),
                    anthropic_ttl_1h,
                    openai_cache_retention: openai_cache_retention.clone(),
                    cache_key: Some(morpheus_session.clone()),
                    mcp_config: mcp_config.clone(),
                    skills_config: skills_config.clone(),
                    effective_context,
                    #[cfg(feature = "onnx")]
                    checkup: None,
                    #[cfg(feature = "embed")]
                    recall: recall_inputs.clone(),
                    ctx_state: context,
                    as_manager: true,
                    own_coworker_id: own_registry_id,
                    user_images: Vec::new(),
                };
                run_manager_turn(
                    index,
                    pane_id,
                    epoch,
                    turn,
                    input,
                    permission_rx,
                    signal,
                    steer.clone().unwrap_or_default(),
                    assembly,
                    morpheus_session,
                    store,
                    event_tx,
                );
            });
        } else {
            // External ACP pane: one tokio task, one channel, the same
            // select! shape the prompt corrector drains with. The turn
            // resumes the pane's stored session so the agent keeps its
            // context across prompts.
            // ACP has no system prompt, so the pane's awareness of its
            // siblings rides the prompt itself: `take_preamble` hands back
            // the peer listing ONLY when the pane's announcement went
            // stale (first turn, or the pane set/worktrees changed since
            // the last one) — otherwise the input goes out untouched.
            // A pending worktree HANDOFF rides BEFORE the preamble: the
            // agent first learns where it now works and what it was
            // doing, then who its siblings are.
            let handoff_block = handoff.map(|summary| {
                format!(
                    "[workspace moved] Your working directory changed to {} — \
                         work only inside it. What you were doing, carried over:\n\n{summary}\n\n",
                    cwd.display()
                )
            });
            // A pane Revert on an ACP pane abandons the agent's session
            // (the protocol has no way to truncate it) and leaves the kept
            // prefix here: this turn opens a FRESH session, so the block is
            // its only memory.
            let rewind_block = rewind.map(|summary| {
                format!(
                    "[conversation rewound] The user rolled this conversation back to an \
                     earlier point. Everything after that point was discarded — treat the \
                     history below as the whole conversation so far:\n\n{summary}\n\n"
                )
            });
            // Both blocks can be pending on the same turn (a revert right
            // after a worktree move, before the first turn), and each says
            // something the other does not: WHERE the pane works, and WHAT
            // the conversation is. Carry both — the flags below are cleared
            // because the text really rode out, so dropping one here would
            // lose it for good.
            let carried_block = match (handoff_block, rewind_block) {
                (Some(handoff), Some(rewind)) => Some(format!("{handoff}{rewind}")),
                (handoff, rewind) => rewind.or(handoff),
            };
            let input = assemble_turn_input(
                carried_block,
                morpheus::harness::agent_coworkers::take_preamble(coworker_identity),
                msg,
            );
            // The permission channel rides the MODE (the split's Yolo
            // contract): a non-Yolo mode makes this turn's
            // `session/request_permission` requests surface on the pane's
            // permission box and await the user's pick; Yolo keeps the
            // headless auto-approve. Manager counts as Yolo here too: its
            // guardrails auto-allow everything beyond its own deny-lists,
            // so no request can ever be emitted. The pane state holds the
            // ONLY sender (the box's answer reaches the turn through it);
            // the receiver goes into the turn.
            let permission_rx = if !matches!(
                own_mode,
                morpheus::harness::Mode::Yolo | morpheus::harness::Mode::Manager
            ) {
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                if let Some(split) =
                    either_split_mut(&mut self.agents_split, &mut self.parked_agents_split)
                {
                    split.set_permission_tx(pane, Some(tx));
                }
                Some(rx)
            } else {
                None
            };
            // The pane's LIVE session: reuse it when it is still alive and
            // rooted at the same workspace, otherwise open a new one. These
            // are the only rebuild triggers reachable here (rewind clears the
            // handle, a worktree move fails the cwd match, death fails
            // `is_alive`); the closed list lives in the guarantees doc.
            let existing = either_split(&self.agents_split, &self.parked_agents_split)
                .and_then(|split| split.panes.get(pane))
                .and_then(|p| p.acp_session.clone());
            let mut live = existing
                .clone()
                .filter(|handle| handle.is_alive() && handle.matches_cwd(&cwd));
            // N22: every dispatch is auditable as reuse/open/rebuild.
            match (&existing, live.is_some()) {
                (Some(_), true) => log::debug!("ACP pane {pane_id}: reuse live session"),
                (Some(_), false) => log::debug!(
                    "ACP pane {pane_id}: rebuild — the previous session is gone or in another cwd"
                ),
                (None, _) => log::debug!("ACP pane {pane_id}: open — no live session"),
            }
            if live.is_none() {
                // A stale handle (dead, or rooted elsewhere) is closed before
                // the replacement opens — one owner per pane (N1).
                if let Some(stale) = either_split_mut(
                    &mut self.agents_split,
                    &mut self.parked_agents_split,
                )
                .and_then(|split| split.panes.get_mut(pane))
                .and_then(|p| p.acp_session.take())
                {
                    stale.shutdown();
                }
                match morpheus_tools::subagent::acp::open_session(
                    &agent,
                    resume.clone(),
                    cwd.clone(),
                ) {
                    Ok(handle) => {
                        if let Some(p) = either_split_mut(
                            &mut self.agents_split,
                            &mut self.parked_agents_split,
                        )
                        .and_then(|split| split.panes.get_mut(pane))
                        {
                            p.acp_session = Some(handle.clone());
                        }
                        live = Some(handle);
                    }
                    Err(error) => {
                        // The pane was already marked running: close it with
                        // the same terminal contract a failed turn uses.
                        let _ = event_tx.send(HarnessEvent::AgentsPaneDone {
                            pane: index,
                            pane_id,
                            epoch,
                            turn,
                            output: error,
                            stop_reason: "error".to_string(),
                            session: None,
                        });
                        return;
                    }
                }
            }
            let live = live.expect("the live session was opened above");
            tokio_handle.spawn(async move {
                let (chunk_tx, mut chunk_rx) =
                    tokio::sync::mpsc::unbounded_channel::<SubagentEvent>();
                let turn_future = live.run_turn(
                    input,
                    &picks,
                    mapped_mode.as_deref(),
                    permission_rx,
                    signal,
                    chunk_tx,
                );
                tokio::pin!(turn_future);
                let mut chunks_open = true;
                let outcome = loop {
                    if chunks_open {
                        tokio::select! {
                            outcome = &mut turn_future => break outcome,
                            chunk = chunk_rx.recv() => match chunk {
                                Some(event) => {
                                    let _ = event_tx.send(HarnessEvent::AgentsPaneEvent {
                                        pane: index,
                                        pane_id,
                                        epoch,
                                        turn,
                                        event,
                                    });
                                }
                                // The channel closes only when the turn
                                // future is dropped (session closed): keep
                                // awaiting the future for its terminal
                                // outcome.
                                None => chunks_open = false,
                            },
                        }
                    } else {
                        break turn_future.await;
                    }
                };
                // The binding FACT first, gate-free: the id this turn
                // actually drove (resumed, or the fresh `session/new`
                // fallback) is a property of the turn, not of the Done's
                // routing. The Done handler's gates (unknown pane id, stale
                // epoch, already-idle pane) may legitimately DROP the event
                // — a pane force-closed by Ctrl+X or hidden mid-turn — and
                // with it the binding; the App's absorb pass still finds the
                // fact here and promotes the pane (live, parked OR hidden).
                // Preserves the deliberate no-propagate-on-error contract: a
                // failed turn carries `session = None` and records nothing.
                if let Some(id) = &outcome.session_id {
                    morpheus::harness::agent_coworkers::record_session_binding(pane_id, id.clone());
                }
                let _ = event_tx.send(HarnessEvent::AgentsPaneDone {
                    pane: index,
                    pane_id,
                    epoch,
                    turn,
                    output: outcome.output,
                    stop_reason: outcome.stop_reason,
                    session: outcome.session_id,
                });
            });
        }
    }

    /// The wake promise (user-reported: "se o agente principal mandou uma
    /// mensagem para um agente em um pane, ele precisa ter um output no
    /// futuro"). A pane turn whose `agents_call` request was made by the
    /// MAIN loop resolves the promise here: when the main loop is NOT
    /// running (it ended before the report arrived — it may even have been
    /// stopped), the parked deliveries are drained and become the input of
    /// a FRESH main-agent turn — the main agent is woken to receive the
    /// report. A USER-initiated pane turn tracks no request
    /// (`complete_request` → `None`) and wakes nothing: the pane works in
    /// harmony with a running or idle main agent. A live main loop is left
    /// alone — it drains its deliveries at its own natural points (the
    /// per-iteration drain and the pre-Done busy-guard).
    pub(super) fn wake_main_agent_if_promised(&mut self, requester: Option<&'static str>) {
        if requester != Some(morpheus::harness::agent_coworkers::MAIN) {
            return;
        }
        if self.state.status == crate::types::SessionStatus::Working {
            return;
        }
        // The USER's stop is ABSOLUTE: a `Cancelled` delivery is the registry's
        // record of a turn the user stopped (Ctrl+X) — waking the main agent
        // with it would override the interrupt with a synthetic turn. Cancelled
        // deliveries are consumed and DISCARDED here (not left parked: a later
        // unrelated wake must never deliver a stale cancellation).
        let notes: Vec<String> = morpheus::harness::agent_coworkers::take_ready_deliveries(
            morpheus::harness::agent_coworkers::MAIN,
        )
        .into_iter()
        .filter_map(|delivery| {
            if delivery.end == morpheus::harness::agent_coworkers::AgentCoworkerTurnEnd::Cancelled {
                log::info!(
                    target: "laya",
                    "wake promise: a cancelled pane report (the user stopped the turn) is dropped, never delivered"
                );
                return None;
            }
            Some(morpheus::harness::agent_coworkers::delivery_note(&delivery))
        })
        .collect();
        if notes.is_empty() {
            // Nothing parked (a wait-mode resolution whose receiver died —
            // the requester's turn was stopped): no promise left to keep.
            return;
        }
        let report = notes.join("\n\n");
        self.start_agent_loop(report);
    }

    /// The pending-todos wake (the manager/coordination counterpart the
    /// improper-stop fix deliberately left out): a MAIN-agent turn that
    /// ended while its OWN todo list still shows open work is re-woken with
    /// the EXACT message the harness's natural-stop gate resumes a
    /// report-owing turn with (`guards::todo_rerun_message`) — the list is
    /// the record that gate trusts, so the wake must speak the same words.
    ///
    /// The trigger is the one the improper-stop fix could not cover: the
    /// agent coordinating `/agents` panes parks its turn to be woken, and a
    /// stale todo that survived a completed turn is nobody's improper stop —
    /// it is just unfinished bookkeeping. Waking here avoids keeping the
    /// main agent running the whole time it is only coordinating.
    ///
    /// Conditions, each exact:
    /// - the main agent is IDLE (no loop running — a working loop drains its
    ///   own todo state in-turn, the wake must never interleave);
    /// - NO agent is running anywhere (no main loop, no pane turn): the wake
    ///   must not race a pane whose report is about to land;
    /// - the todo list has open (pending/in-progress) items;
    /// - the open listing CHANGED since the previous wake: an agent that
    ///   stopped without reconciling anything must not be re-woken forever;
    /// - the wake budget still has a unit (a listing that changes every
    ///   turn — a status toggle, a reworded item — would otherwise read as
    ///   "changed" forever); any non-wake loop start resets it;
    /// - Command mode is OFF: there the input is dispatched to `bash_run`
    ///   verbatim, and the steering text (with its free-text item
    ///   descriptions) must never become shell commands;
    /// - the Queue Actions hold is OFF: no auto-start while the user is
    ///   operating on queued messages (the same contract
    ///   `handle_loop_end` defers its own auto-start under).
    pub(super) fn wake_main_agent_if_pending_todos(
        &mut self,
        todo: Option<&morpheus_tools::plan::types::TodoList>,
    ) {
        if self.any_agent_running() {
            return;
        }
        if self.state.mode == morpheus::harness::Mode::Command {
            return;
        }
        if self.queue_actions_hold_active() {
            return;
        }
        // Keyless install with the free-gateway offer pending: the offer
        // dialog must decide first. A wake parked behind the opt-in would
        // replay its steering through the replay path as a VISIBLE user
        // message — the one start path the transcript-skip cannot cover.
        if self.gateway_recommendation_offers(false).is_some() {
            return;
        }
        let Some(todo) = todo else {
            return;
        };
        let listed = morpheus::harness::core::todo_rerun_open_items(todo);
        if listed.is_empty() {
            return;
        }
        if self.todo_wake_last_items.as_deref() == Some(listed.as_str()) {
            return;
        }
        if self.todo_wakes_left == 0 {
            log::info!(
                target: "laya",
                "pending-todos wake refused: the budget is spent (listing changed again, {} open items)",
                listed.lines().count()
            );
            return;
        }
        self.todo_wake_last_items = Some(listed.clone());
        self.todo_wakes_left -= 1;
        self.todo_wake_in_flight = true;
        log::info!(
            target: "laya",
            "pending-todos wake: starting a turn with the re-run steering ({} open items, {} wakes left)",
            listed.lines().count(),
            self.todo_wakes_left
        );
        self.start_agent_loop(morpheus::harness::core::todo_rerun_message(Some(&listed)));
    }

    /// Close every live ACP pane session (live and parked splits). The quit
    /// path calls this: the process is about to exit and no harness process
    /// may keep running behind it.
    pub(super) fn shutdown_agents_sessions(&mut self) {
        for split in self
            .agents_split
            .iter_mut()
            .chain(self.parked_agents_split.iter_mut())
        {
            for pane in &mut split.panes {
                if let Some(handle) = pane.acp_session.take() {
                    handle.shutdown();
                }
            }
        }
    }

    /// Whether ANY agent turn is running: the main loop (`Working`) or any
    /// `/agents` pane turn (live or parked split). The pending-todos wake
    /// must not fire while an agent is active — the report of a running
    /// pane is the fresher material, and a working main loop reconciles its
    /// own list.
    fn any_agent_running(&self) -> bool {
        self.state.status == crate::types::SessionStatus::Working
            || either_split(&self.agents_split, &self.parked_agents_split)
                .is_some_and(|split| split.panes.iter().any(|p| p.running))
    }

    /// Absorb the session-binding FACTS the pane turns recorded gate-free
    /// (see `morpheus::harness::agent_coworkers::record_session_binding`): promote the
    /// matching pane object — LIVE, PARKED or HIDDEN — to the session the
    /// turn actually drove.
    ///
    /// This is the reconciliation half that decouples binding capture from
    /// the `AgentsPaneDone` routing: the Done handler's gates (unknown pane
    /// id, stale epoch, already-idle pane) stay untouched — they protect
    /// the event stream (no double closure, no cross-pane injection) — but
    /// they may drop the event that used to be the binding's only vehicle.
    /// A fact is a property of the turn, not a routing decision, so it
    /// lands here instead:
    /// - a pane id that answers to a LIVE or PARKED pane binds it (the
    ///   same single `bind_agents_pane_session` promotion the happy-path
    ///   Done handler runs — dedup inside, so a Done that DID land just
    ///   no-ops here);
    /// - a pane id that answers to a HIDDEN-stack entry (the pane was
    ///   hidden while its first turn was still running) promotes the
    ///   persisted `Name` ref to `Bound` in place — the restore path then
    ///   rebuilds the pane already bound and its replay rehydrates the
    ///   timeline;
    /// - a pane id that answers to NOTHING (a pane a rebuild dropped, or a
    ///   pre-`pane_id` legacy hidden entry) drops the fact — the same
    ///   anti-contamination verdict the event gates reach, reached
    ///   structurally.
    pub(super) fn absorb_pane_session_bindings(&mut self) {
        let facts = morpheus::harness::agent_coworkers::drain_session_bindings();
        if facts.is_empty() {
            return;
        }
        let mut hidden_dirty = false;
        for (pane_id, session_id) in facts {
            // `0` = never stamped (tests, pre-field legacy entries): no
            // identity to route by — drop the fact.
            if pane_id == 0 {
                continue;
            }
            // LIVE or PARKED pane object first: the same object either slot
            // carries, resolved by its stable id wherever it lives.
            let index = either_split(&self.agents_split, &self.parked_agents_split)
                .and_then(|split| split.pane_index_by_id(pane_id));
            if let Some(index) = index {
                // The same promotion the Done handler runs — reused
                // verbatim. ONE write path: `bind_agents_pane_session` both
                // seeds the live pane's resume id (deduped) and promotes
                // the persisted entry; the absorb adds no parallel write.
                self.bind_agents_pane_session(index, session_id);
                continue;
            }
            // Hidden stack: the pane object is gone (the hide dropped it);
            // its persisted identity rides the `HiddenPane::pane_id` bridge.
            let Some(session) = self.state.current_session_mut() else {
                continue;
            };
            let Some(entry) = session
                .agents_hidden
                .iter_mut()
                .find(|hidden| hidden.pane_id == pane_id)
            else {
                continue;
            };
            if entry.pane.session_id() == Some(session_id.as_str()) {
                continue;
            }
            entry.pane = crate::types::AgentPaneRef::Bound {
                agent: entry.pane.agent().to_string(),
                session_id,
                worktree: entry.pane.worktree().cloned(),
            };
            hidden_dirty = true;
        }
        // ONE save for the whole pass (the hidden-stack path has no per-entry
        // writer of its own — unlike the live path, whose
        // `bind_agents_pane_session` already persists).
        if hidden_dirty {
            self.save_current_agents_metadata();
        }
    }

    /// Feed the RUNNING morpheus panes' "next request" queues into their live
    /// turns — the pane-level twin of [`App::pump_queued_messages`].
    ///
    /// A message goes to the pane's steering handle (the harness drains it
    /// before the turn's next request) and echoes into the pane's timeline
    /// with the user's rail at the same moment, so the pane shows who is
    /// speaking. Only panes that hold a handle qualify: a morpheus pane's turn
    /// has request boundaries, an ACP agent takes one prompt per turn (so no
    /// point to inject at) and a pane between turns has no loop at all. The
    /// ACP case is not stranded by that: its "next request" message is
    /// consumed at the turn boundary by
    /// [`Self::serve_agents_pane_user_queues`], ahead of its "next loop"
    /// queue and ahead of any queued `agents_call`.
    ///
    /// ONE message per pane per frame keeps the FIFO observable between
    /// frames and mirrors the main pump's one-in-flight discipline.
    pub(super) fn pump_agents_pane_queued_requests(&mut self) {
        let Some(split) = either_split_mut(&mut self.agents_split, &mut self.parked_agents_split)
        else {
            return;
        };
        for pane in &mut split.panes {
            if !pane.running || pane.queued.next_request.is_empty() {
                continue;
            }
            let Some(steer) = pane.steer.clone() else {
                continue;
            };
            let Some(text) = pane.queued.next_request.pop_front() else {
                continue;
            };
            pane.activity.open_turn_input(&text, true);
            steer.push(text);
        }
    }

    /// Start the next queued USER message of every free `/agents` pane, FIFO.
    ///
    /// The two queues keep the main agent's meaning for EVERY pane kind:
    /// `next request` is served FIRST (the message the user wanted as the
    /// pane's very next request), then `next loop` (a fresh working stretch).
    /// For a morpheus pane the `next request` queue normally never reaches this
    /// pass while the turn runs — [`Self::pump_agents_pane_queued_requests`]
    /// already fed it into the harness, which drains it before its next
    /// request. An ACP pane has no prompt in flight to inject into, so its
    /// `next request` messages are consumed HERE, at the turn boundary: the
    /// message goes out before the pane is ever seen idle (a `next loop`
    /// message would otherwise have to wait a whole pass, and could lose its
    /// place to other work).
    ///
    /// Called from the frame tick (a message queued while every pane idles —
    /// or one whose pane went away mid-queue — must not wait for an event that
    /// never comes) and right after a pane frees (the `AgentsPaneDone`
    /// handler). The user's queue OUTRANKS any queued `agents_call` request:
    /// starting a turn here leaves the pane running, so
    /// [`Self::serve_agents_pane_queues`] skips it and the agent's request
    /// waits its turn.
    ///
    /// Before starting anything this pass settles the leftovers: a message the
    /// finished turn never drained out of its steering handle is put back by
    /// the queue (nothing is stranded between turn end and the handle being
    /// dropped — the drain is atomic under the handle's own lock). It keeps its
    /// `next request` standing: the pane is free, so that message IS the next
    /// request of the stretch that just closed.
    pub(super) fn serve_agents_pane_user_queues(&mut self) -> bool {
        {
            let Some(split) =
                either_split_mut(&mut self.agents_split, &mut self.parked_agents_split)
            else {
                return false;
            };
            for pane in &mut split.panes {
                if pane.running {
                    continue;
                }
                if let Some(steer) = pane.steer.take() {
                    for text in steer.drain() {
                        pane.queued.next_request.push_back(text);
                    }
                }
            }
        }
        // One turn per pane per pass: dispatching marks the pane running, so
        // the rest of its queue waits for the next free moment.
        let targets: Vec<usize> = {
            let Some(split) = either_split(&self.agents_split, &self.parked_agents_split) else {
                return false;
            };
            split
                .panes
                .iter()
                .enumerate()
                .filter(|(_, p)| !p.running && !p.rehydrating)
                .filter(|(_, p)| !morpheus::harness::agent_coworkers::coworker_is_compacting(p.id))
                .filter(|(_, p)| !p.queued.is_empty())
                .map(|(i, _)| i)
                .collect()
        };
        let mut started = false;
        for pane in targets {
            // `next request` first: it was picked as the pane's very next
            // request, so it may never be starved by a `next loop` message.
            // The flag remembers WHERE it came from for the requeue below.
            let Some((text, as_request)) =
                either_split_mut(&mut self.agents_split, &mut self.parked_agents_split)
                    .and_then(|split| split.panes.get_mut(pane))
                    .and_then(|p| {
                        p.queued
                            .next_request
                            .pop_front()
                            .map(|text| (text, true))
                            .or_else(|| p.queued.next_loop.pop_front().map(|text| (text, false)))
                    })
            else {
                continue;
            };
            self.dispatch_to_pane(pane, text.clone(), false);
            // The dispatch can bail after the guards (a pane session that
            // failed to mint, a pane that vanished): a message the user
            // queued must not vanish with it — put it back at the head.
            let running = either_split(&self.agents_split, &self.parked_agents_split)
                .and_then(|split| split.panes.get(pane))
                .is_some_and(|p| p.running);
            if running {
                started = true;
                continue;
            }
            if let Some(p) = either_split_mut(&mut self.agents_split, &mut self.parked_agents_split)
                .and_then(|split| split.panes.get_mut(pane))
            {
                if as_request {
                    p.queued.next_request.push_front(text);
                } else {
                    p.queued.next_loop.push_front(text);
                }
            }
        }
        started
    }

    /// Dispatch every queued `agents_call` request whose target pane is
    /// idle. Called from the frame tick (a request enqueued while every
    /// pane idles would otherwise wait for a pane event that never comes —
    /// the requester's busy-guard would hang) and right after a pane frees
    /// (the `AgentsPaneDone` handler). One request per pane per pass: the
    /// dispatch marks the pane running, so the rest of its queue drains on
    /// the following passes, FIFO.
    pub(super) fn serve_agents_pane_queues(&mut self) {
        let targets: Vec<(usize, String)> = {
            let Some(split) = either_split(&self.agents_split, &self.parked_agents_split) else {
                return;
            };
            split
                .panes
                .iter()
                .enumerate()
                // A running pane serves later (the queue holds), a
                // rehydrating pane first finishes its replay. The morpheus pane
                // is a normal queue target — its own turn's `agents_call` to
                // itself is rejected at the tool layer (see
                // `Harness::with_own_coworker_id`), so a queued request always
                // belongs to a pane that CAN free itself.
                .filter(|(_, p)| {
                    !p.running
                        && !p.rehydrating
                        && !morpheus::harness::agent_coworkers::coworker_is_compacting(p.id)
                })
                .filter_map(|(i, _)| {
                    let id = split.pane_registry_id(i)?;
                    morpheus::harness::agent_coworkers::has_queued_request(&id).then_some((i, id))
                })
                .collect()
        };
        for (pane, id) in targets {
            let Some(request) = morpheus::harness::agent_coworkers::pop_next_request(&id) else {
                continue;
            };
            self.dispatch_to_pane(pane, request.text, true);
        }
    }

    /// The `agents_worktree` op drain — the App-side half of the tool: pops
    /// parked asks from the pane registry, supervises them and executes
    /// them with the SAME engine calls and side effects as the user's own
    /// `/worktree` command, tagging the binding `AssignedBy::Agent`.
    /// Supervision (design decision 3): an ask that would change or clear a
    /// worktree the USER assigned opens the session permission box (Allow /
    /// Allow Once / Deny) — the user's supremacy over their own binding
    /// holds even in Yolo mode — and the drain parks until the box resolves
    /// it (`pending_worktree_confirmation`). Any other ask executes
    /// immediately. The paused tool call consumes the outcome itself
    /// (`take_ready_worktree_delivery`), so it becomes the tool result; a
    /// Worktree-kind delivery (`complete_worktree_op`) is only the fallback
    /// for an ask interrupted by a stop request.
    pub(super) fn serve_agents_pane_worktree_ops(&mut self) {
        // A permission box for an earlier ask is on screen: it is the
        // sovereign ask — the user's answer resolves it and the drain
        // resumes on a later frame. Nothing else pops meanwhile.
        if self.pending_worktree_confirmation.is_some() {
            return;
        }
        // ANY visible dialog defers the pop: stacking this permission box
        // over a live one (quit, session delete, RAG delete) would let the
        // resolve path steal the answer the user aimed at THEIR dialog.
        if self.dialog.visible() || self.permission_dialog.visible {
            return;
        }
        let Some(op) = morpheus::harness::agent_coworkers::peek_worktree_op() else {
            return;
        };
        // Resolve the target pane by registry id (the tool speaks ids, the
        // split speaks indexes). A pane that vanished since the ask was
        // parked fails the op — the registry's `set_coworkers` cleanup usually
        // gets there first; this is the same net result.
        // Resolve the target by the op's stamped IDENTITY (`pane_index_by_id`):
        // a label recycled onto another pane by a sibling's removal must not
        // receive the ask (Issue 3).
        let resolved =
            either_split(&self.agents_split, &self.parked_agents_split).and_then(|split| {
                let index = split.pane_index_by_id(op.coworker_id)?;
                let mut needs_confirmation = split.panes[index]
                    .worktree
                    .as_ref()
                    .is_some_and(|w| w.assigned_by == morpheus::git::worktree::AssignedBy::User);
                // The user granted session-wide worktree permission
                // ("Allow" on an earlier ask): the agent may rebind even
                // User-assigned panes without asking again for the rest
                // of this app run.
                if self.worktree_agent_grant {
                    needs_confirmation = false;
                }
                Some((index, needs_confirmation))
            });
        let Some((pane_index, needs_confirmation)) = resolved else {
            let op = morpheus::harness::agent_coworkers::pop_worktree_op();
            if let Some(op) = op {
                morpheus::harness::agent_coworkers::complete_worktree_op(
                    &op,
                    Err(format!("pane '{}' no longer exists", op.coworker)),
                );
            }
            return;
        };
        if needs_confirmation {
            // The user's binding, the user's call: open the SESSION
            // permission box (the same inline `PermissionDialog` the main
            // loop's permission requests use) so the user can allow for
            // the whole session, allow once, or deny — and park the op
            // until the answer. The `tool` field marks the ask as a
            // worktree change, so the key/mouse resolution routes the
            // answer here instead of into the main loop's `perm_tx`
            // (this ask is resolved App-side, never by the harness).
            let description = match op.action {
                morpheus::harness::agent_coworkers::WorktreeAction::Assign => format!(
                    "An agent asks to move pane '{}' into a NEW worktree on branch '{}'. \
                     Allow for this whole session (the agent may then change any pane's \
                     worktree binding), only this once, or deny?",
                    op.coworker,
                    op.branch.as_deref().unwrap_or_default()
                ),
                morpheus::harness::agent_coworkers::WorktreeAction::Clear => format!(
                    "An agent asks to move pane '{}' out of the worktree YOU assigned \
                     (the binding is dropped; nothing on disk is deleted). Allow for this \
                     whole session (the agent may then change any pane's worktree binding), \
                     only this once, or deny?",
                    op.coworker
                ),
            };
            self.permission_dialog.request =
                Some(crate::routes::session::permission::PermissionRequest {
                    tool: "agents_worktree".to_string(),
                    description,
                    args: format!("pane: {}", op.coworker),
                });
            self.permission_dialog.visible = true;
            // Default to "Allow Once" (index 1), same default as the
            // harness permission requests.
            self.permission_dialog.selected = 1;
            let op = morpheus::harness::agent_coworkers::pop_worktree_op();
            if let Some(op) = op {
                // The index was only the resolution vehicle — the op's
                // registry id is the durable handle (re-resolved at
                // answer time, issue 1 of the review).
                self.pending_worktree_confirmation = Some(op);
            }
            return;
        }
        let Some(op) = morpheus::harness::agent_coworkers::pop_worktree_op() else {
            return;
        };
        let outcome = match op.action {
            morpheus::harness::agent_coworkers::WorktreeAction::Assign => {
                let branch = op.branch.clone().unwrap_or_default();
                self.assign_pane_worktree_for_agent(pane_index, &branch)
            }
            morpheus::harness::agent_coworkers::WorktreeAction::Clear => {
                self.clear_pane_worktree_for_agent(pane_index)
            }
        };
        morpheus::harness::agent_coworkers::complete_worktree_op(&op, outcome);
    }

    /// The App-side `agents_compact` drain: pop parked compaction asks and
    /// execute them — running the TARGET pane's own session through
    /// `build_manual_compaction_harness` + `compact_on_demand`, the EXACT
    /// mechanism the user's `/compact` slash command drives on a session.
    /// Nothing here is compact-specific: the harness is built once, the
    /// pane's persisted context is restored into it, the shared funnel
    /// runs, and the compacted snapshot is persisted back to the pane's
    /// session record.
    ///
    /// Guards, in order — a refused ask completes with its reason instead
    /// of parking forever:
    ///
    /// - a pane busy with a live turn (compaction would race the loop's
    ///   context manager — the same "between messages" rule `/compact`
    ///   applies to the main session);
    /// - a pane whose bound session has no context records yet (nothing to
    ///   compact).
    ///
    /// The paused `agents_compact` tool call consumes the outcome itself
    /// (`take_ready_compact_delivery`), so it becomes the tool result; a
    /// Compact-kind delivery (`complete_compact_op`) is only the fallback
    /// for an ask interrupted by a stop request. The pane's own timeline
    /// is not rewritten here: its next turn restores the compacted context
    /// records from its session, so the summary arrives as its new
    /// baseline without colliding with any live stream.
    pub(super) fn serve_agents_pane_compact_ops(&mut self) {
        let Some(op) = morpheus::harness::agent_coworkers::peek_compact_op() else {
            return;
        };
        // Resolve the target pane by the op's stamped IDENTITY
        // (`pane_index_by_id`): a label recycled onto another pane by a
        // sibling's removal must not receive the ask (Issue 3, same rule
        // the worktree drain applies).
        let resolved = either_split(&self.agents_split, &self.parked_agents_split)
            .and_then(|split| split.pane_index_by_id(op.coworker_id))
            .and_then(|pane_index| {
                either_split(&self.agents_split, &self.parked_agents_split)
                    .and_then(|split| split.panes.get(pane_index))
                    .map(|pane| (pane_index, pane.running, pane.session_id.clone()))
            });
        let Some((pane_index, running, session_id)) = resolved else {
            let op = morpheus::harness::agent_coworkers::pop_compact_op();
            if let Some(op) = op {
                morpheus::harness::agent_coworkers::complete_compact_op(
                    &op,
                    Err(format!("pane '{}' no longer exists", op.coworker)),
                );
            }
            return;
        };
        if running {
            // Do NOT pop: the ask stays parked and this drain retries on a
            // later frame — the pane frees and the compaction runs then,
            // instead of failing an ask the model correctly made.
            return;
        }
        let Some(session_id) = session_id else {
            let op = morpheus::harness::agent_coworkers::pop_compact_op();
            if let Some(op) = op {
                morpheus::harness::agent_coworkers::complete_compact_op(
                    &op,
                    Err(format!(
                        "pane '{}' has no bound session yet — nothing to compact",
                        op.coworker
                    )),
                );
            }
            return;
        };
        let Some(op) = morpheus::harness::agent_coworkers::pop_compact_op() else {
            return;
        };
        let Some(ctx_state) = self.session_store.load_context(&session_id) else {
            morpheus::harness::agent_coworkers::complete_compact_op(
                &op,
                Err("nothing to compact yet — the pane's session has no context records".into()),
            );
            return;
        };
        // The SAME inputs the user's `/compact` passes: the pane's session
        // is a real morpheus session, so it compacts through the identical
        // connector resolution and cache affinity (keyed to the PANE's own
        // session id — never the main session's).
        let provider = self.llm_config.provider.clone();
        let model = self.llm_config.model.clone();
        let base_url = self.base_url_for(&provider);
        let summarization_models = self.manual_summarization_models();
        let local_base_urls = self.configured_local_base_urls();
        let anthropic_ttl = self.setup.anthropic_cache_ttl_1h();
        let retention = self.setup.openai_cache_retention().map(String::from);
        let reasoning = self.llm_config.reasoning.clone();
        let cwd = self.state.working_directory.clone();
        let store = self.session_store.clone();
        let stop_signal = Arc::new(AtomicBool::new(false));
        let pane = op.coworker.clone();
        self.tokio_handle.spawn(async move {
            // A DEDICATED event channel, deliberately NOT the app's: the
            // compaction's lifecycle events (ContextInfo, ContextSnapshot,
            // the streamed summary tokens) belong to the PANE's session —
            // forwarded to the app channel they would land in the MAIN
            // session's handlers (its footer, and `persist_incrementally`
            // would save the pane's compacted context into the main
            // session record — the exact cross-session leak this feature
            // must never commit). The main `/compact` forwards them
            // because the session it compacts IS the main one; here they
            // are drained and dropped after the run, the tool result
            // carries the only truth the model needs.
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let mut builder_error: Option<String> = None;
            let outcome = match super::commands::build_manual_compaction_harness(
                &provider,
                model.as_deref(),
                base_url,
                summarization_models,
                local_base_urls,
                anthropic_ttl,
                retention,
                reasoning,
                &session_id,
                &cwd,
            ) {
                Ok(mut harness) => {
                    harness.context_manager.restore_state(&ctx_state);
                    let ok = harness.compact_on_demand(&tx, stop_signal).await;
                    if ok == morpheus::harness::ManualCompactionOutcome::Compacted {
                        // Persist the compacted context back to the PANE's
                        // session record (the same path the manager loop
                        // saves with) — the pane's next turn restores from
                        // here. Read from the harness BEFORE dropping it.
                        let context = harness.context_manager.save_state();
                        drop(harness);
                        drop(tx);
                        // The run is over: drain whatever the harness
                        // queued (all senders are gone, so this
                        // terminates) and discard it.
                        while rx.recv().await.is_some() {}
                        let display = manager_context_display(&context);
                        persist_manager_session(&store, &session_id, display, context);
                    } else {
                        drop(harness);
                        drop(tx);
                        while rx.recv().await.is_some() {}
                    }
                    ok
                }
                Err(reason) => {
                    // Finding 5: carry the builder's reason (a connector
                    // misconfiguration) into the tool result, like the
                    // `/compact` path forwards its `HarnessEvent::Error`.
                    builder_error = Some(reason);
                    morpheus::harness::ManualCompactionOutcome::Failed
                }
            };
            let note = match outcome {
                morpheus::harness::ManualCompactionOutcome::Compacted => Ok(format!(
                    "Pane '{pane}' compacted: its context was summarized with the same \
                     mechanism as /compact and persisted to its session. Its next turn \
                     starts from the handoff summary."
                )),
                morpheus::harness::ManualCompactionOutcome::NothingToCompact => {
                    Err(format!("Pane '{pane}' has nothing to compact yet."))
                }
                morpheus::harness::ManualCompactionOutcome::Failed => Err(match builder_error {
                    Some(reason) => format!(
                        "The compaction of pane '{pane}' failed before it could run: \
                         {reason} Nothing changed."
                    ),
                    None => format!(
                        "The compaction of pane '{pane}' failed — the summarization \
                         call did not complete. Nothing changed."
                    ),
                }),
            };
            morpheus::harness::agent_coworkers::complete_compact_op(&op, note);
        });
        // `pane_index` is only the resolution vehicle (same as the
        // worktree drain): the registry pairs the outcome by op id.
        let _ = pane_index;
    }

    /// The manager pane's OWN morpheus session id, minted on first use: the
    /// pane is not a throwaway nested harness — it is a real session in the
    /// store (the history panel lists it from birth, and reopening it
    /// restores the manager's conversation). The claim open-locks the id,
    /// so a second process can never adopt it.
    fn ensure_manager_pane_session(&mut self, pane: usize) -> Option<String> {
        if let Some(session_id) = either_split(&self.agents_split, &self.parked_agents_split)
            .and_then(|split| split.panes.get(pane))
            .and_then(|p| p.session_id.clone())
        {
            return Some(session_id);
        }
        // A pane with its own model selection stamps it on the fresh shell:
        // the session's model markers exist from birth (the SAME fields the
        // main session records), so a restart resumes the pane on ITS model
        // via the marker precedence below — the shell save after this block
        // carries them.
        let pane_selection = either_split(&self.agents_split, &self.parked_agents_split)
            .and_then(|split| split.panes.get(pane))
            .and_then(|p| p.model_selection.as_ref());
        let (pane_provider, pane_model, pane_reasoning) = match pane_selection {
            Some(selection) => (
                if selection.provider.is_empty() {
                    None
                } else {
                    Some(selection.provider.clone())
                },
                Some(selection.model.clone()),
                selection.reasoning.clone(),
            ),
            None => (None, None, None),
        };
        let Some((session_id, now_ms)) = self.session_store.claim_fresh_session_id() else {
            // Every candidate failed to lock (I/O trouble): refuse the turn
            // rather than run an unpersisted manager session.
            self.toast_state.show(ToastOptions {
                title: Some("Agents".into()),
                message: "Could not create the manager session — try again.".into(),
                variant: ToastVariant::Error,
                duration_ms: 4000,
            });
            return None;
        };
        // Persist the empty shell synchronously so the history lists the
        // manager session even if the first turn never completes.
        self.session_store.save_session(&crate::types::Session {
            id: session_id.clone(),
            title: "agents manager".to_string(),
            messages: Vec::new(),
            created_at: now_ms,
            title_generated: false,
            provider: pane_provider,
            model: pane_model,
            reasoning: pane_reasoning,
            agents_panes: None,
            agents_mode_open: false,
            agents_hidden: Vec::new(),
            input_history: Vec::new(),
            ctx_ids: std::collections::HashMap::new(),
            main_agent_worktree: None,
        });
        if let Some(pane) = either_split_mut(&mut self.agents_split, &mut self.parked_agents_split)
            .and_then(|split| split.panes.get_mut(pane))
        {
            pane.session_id = Some(session_id.clone());
        }
        self.state.session_summaries = self.session_store.list_sessions();
        Some(session_id)
    }

    /// The manager pane's OWN model selection, with the session markers'
    /// precedence: the pane's committed per-pane pick wins; absent one, the
    /// pane's morpheus session carries the same provider/model/reasoning markers
    /// the main session does (a pick made in a previous run comes back —
    /// the SAME precedence `restore_current_session_model` honors when the
    /// main view re-enters a session); `None` = the pane has no selection
    /// and inherits the global config at spawn time.
    pub(in crate::app) fn manager_pane_model_selection(
        &self,
        pane: &AgentPane,
    ) -> Option<PaneModelSelection> {
        if let Some(selection) = &pane.model_selection {
            return Some(selection.clone());
        }
        let session = self.session_store.load_session(pane.session_id.as_ref()?)?;
        match (session.provider, session.model) {
            // A recorded explicit selection fully replaces the route — the
            // reasoning could legitimately be `None` (the model's default),
            // which must clear a leftover effort from the global config.
            (Some(provider), Some(model)) => {
                Some(PaneModelSelection::new(provider, model, session.reasoning))
            }
            // A recorded `auto` selection has no provider to pin — the
            // fallback chain stays the source of truth.
            (None, Some(model)) if model == "auto" => Some(PaneModelSelection::new(
                String::new(),
                model,
                session.reasoning,
            )),
            // No usable recorded selection: the pane inherits the global
            // config (the pre-feature behavior).
            _ => None,
        }
    }

    /// The focused pane's PROMPT-BADGE triple (provider, model, reasoning),
    /// resolved for EVERY pane kind — strictly from IN-MEMORY pane state
    /// (render calls this every frame; a session-file read here would parse
    /// the pane's JSONL per repaint). External ACP panes read the pane's
    /// `session_info` — the harness's own `SessionModel` report — and yield
    /// `None` when nothing was reported yet (the badge shows only the
    /// brand name). The manager (morpheus) pane is the main agent itself: its
    /// bridge never emits a `SessionModel` event, so its badge resolves
    /// from the pane's OWN selection (`model_selection` — committed by the
    /// per-pane picker, hydrated from the session markers on every split
    /// rebuild by `hydrate_manager_pane_model_selections`). Without this a
    /// committed per-pane pick changed the pane's TURNS while the badge
    /// kept showing the main agent's global model — the pick looked broken.
    pub(in crate::app) fn effective_pane_prompt_info(
        &self,
        pane: usize,
    ) -> Option<crate::routes::session::agents_split::AgentSessionInfo> {
        let split = either_split(&self.agents_split, &self.parked_agents_split)?;
        let pane_state = split.panes.get(pane)?;
        if pane_state.agent != MANAGER_LABEL {
            return pane_state.session_info.clone();
        }
        let selection = pane_state.model_selection.as_ref()?;
        Some(selection.to_session_info())
    }

    /// Hydrate every manager (morpheus) pane's OWN model selection from its
    /// bound session's markers, ONE disk read per pane per call — never per
    /// frame (the render badge reads the in-memory `model_selection` the
    /// hydration lands in). A pane that already carries a selection (the
    /// picker's commit, or a previous hydration) is left untouched. Called
    /// on every split (re)build — session switch, fresh confirm, park
    /// re-attach — so a pick made in a previous run (or in another session
    /// view) shows on the pane's badge the moment the split takes the
    /// screen, exactly like the dispatch route resolves it.
    pub(in crate::app) fn hydrate_manager_pane_model_selections(&mut self) {
        // Phase 1 (shared borrow): the manager panes still missing a
        // selection, keyed by index — with the selections resolved from
        // their sessions. The store reads happen here, BEFORE the mutable
        // phase: `load_session` borrows `self` immutably and cannot run
        // under the split's `&mut` below.
        let mut hydrated: Vec<(
            usize,
            crate::routes::session::agents_split::PaneModelSelection,
        )> = Vec::new();
        if let Some(split) = either_split(&self.agents_split, &self.parked_agents_split) {
            for (index, pane) in split.panes.iter().enumerate() {
                if pane.agent == MANAGER_LABEL
                    && pane.model_selection.is_none()
                    && let Some(selection) = self.manager_pane_model_selection(pane)
                {
                    hydrated.push((index, selection));
                }
            }
        }
        // Phase 2 (exclusive borrow): the selection AND the badge chrome
        // land together — the pane's serving pair exists from the hydration
        // on, so the pick shows without waiting for a turn report.
        if let Some(split) = either_split_mut(&mut self.agents_split, &mut self.parked_agents_split)
        {
            for (index, selection) in hydrated {
                let Some(pane) = split.panes.get_mut(index) else {
                    continue;
                };
                // The selection AND the badge chrome land together (the one
                // shared mapping): the pane's serving pair exists from the
                // hydration on, so the pick shows without waiting for a
                // turn report.
                pane.session_info = Some(selection.to_session_info());
                pane.model_selection = Some(selection);
            }
        }
    }

    /// Cold-restore ONE pane's bound session: drive an ACP `session/load`
    /// replay pass against a fresh harness connection and stream the
    /// replayed history into the pane's timeline through the SAME
    /// `AgentsPaneEvent` channel a live turn uses (epoch-stamped like any
    /// other pane event). Nothing is prompted; the terminal
    /// `AgentsPaneReplayed` clears the pane's `rehydrating` flag.
    ///
    /// Called from the split (re)open paths for a `Bound` pane whose
    /// timeline is empty — the post-reboot state. A failed pass (harness
    /// gone, no `loadSession` support) delivers `ok: false` and the pane
    /// stays on its empty live timeline; the next prompt falls back to
    /// `session/new` through the normal turn path.
    pub(super) fn dispatch_agents_pane_replay(&mut self, pane: usize) {
        // A rewound pane must never rehydrate the session it abandoned (the
        // replay would restore every turn the undo removed).
        if either_split(&self.agents_split, &self.parked_agents_split)
            .and_then(|split| split.panes.get(pane))
            .is_some_and(|p| p.rewound)
        {
            return;
        }
        let Some(session_id) = self
            .agents_split
            .as_ref()
            .and_then(|split| split.panes.get(pane))
            .and_then(|p| p.session_id.clone())
        else {
            return;
        };
        // Re-borrow mutably to set the in-flight flag (the read above is
        // done — the session id snapshot moved out).
        let agent = self
            .agents_split
            .as_mut()
            .and_then(|split| split.panes.get_mut(pane))
            .map(|p| {
                p.rehydrating = true;
                (p.agent.clone(), p.id)
            });
        let Some((agent, pane_id)) = agent else {
            return;
        };
        // The manager pane is NOT an ACP client — its bound session is a
        // real morpheus session in THIS app's store, so cold-restore reads the
        // session directly (no `session/load` round-trip): the persisted
        // CONTEXT — the same authoritative snapshot a resume would restore
        // into a fresh harness — replays as the pane's typed event stream
        // through the SAME pane-event channel, then the terminal replayed
        // event clears the in-flight flag.
        if agent == MANAGER_LABEL {
            let epoch = self.agents_pane_epoch;
            let mut events = Vec::new();
            if let Some(context) = self.session_store.load_context(&session_id) {
                events.extend(manager_context_replay_events(&context));
            } else {
                // Sessions persisted before the context snapshot existed in
                // the store (or display-only shells) carry none: degrade to
                // the old display projection — prose-only, but never empty
                // when the session had text.
                if let Some(session) = self.session_store.load_session(&session_id) {
                    for message in &session.messages {
                        for part in &message.parts {
                            // Reasoning parts replay as Thought timeline
                            // entries (dimmed, same as the live pane's
                            // thinking blocks); arrival order is preserved
                            // because the persist side wrote reasoning
                            // BEFORE the text part.
                            match part {
                                crate::types::Part::Text(text) => {
                                    let event = if matches!(
                                        message.role,
                                        crate::types::MessageRole::User
                                    ) {
                                        SubagentEvent::UserMessage {
                                            text: text.text.clone(),
                                        }
                                    } else {
                                        SubagentEvent::Message {
                                            text: text.text.clone(),
                                        }
                                    };
                                    events.push(event);
                                }
                                crate::types::Part::Reasoning(reasoning) => {
                                    events.push(SubagentEvent::Thought {
                                        text: reasoning.text.clone(),
                                    });
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            for event in events {
                let _ = self.event_tx.send(HarnessEvent::AgentsPaneEvent {
                    pane,
                    pane_id,
                    epoch,
                    // A replay belongs to no turn: unstamped.
                    turn: 0,
                    event,
                });
            }
            if let Some(p) = self
                .agents_split
                .as_mut()
                .and_then(|split| split.panes.get_mut(pane))
            {
                p.rehydrating = false;
            }
            let _ = self.event_tx.send(HarnessEvent::AgentsPaneReplayed {
                pane,
                pane_id,
                epoch,
                ok: true,
                error: String::new(),
            });
            return;
        }
        let cwd = either_split(&self.agents_split, &self.parked_agents_split)
            .and_then(|split| split.panes.get(pane))
            .map(|p| pane_effective_cwd(p, &self.state.working_directory))
            .unwrap_or_else(|| PathBuf::from(&self.state.working_directory));
        let epoch = self.agents_pane_epoch;
        let event_tx = self.event_tx.clone();
        let tokio_handle = self.tokio_handle.clone();
        // The mirror follows into the rehydrating spinner.
        self.sync_agents_pane_mirrors();
        // Tests never spawn real ACP harnesses: the replay pipeline is
        // exercised by feeding `HarnessEvent`s straight into the app's
        // event channel (the same way the pane lifecycle tests drive
        // turns). The in-flight flag stays set until a synthetic
        // `AgentsPaneReplayed` arrives — which is exactly what those
        // tests assert on.
        #[cfg(not(test))]
        tokio_handle.spawn(async move {
            let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel::<SubagentEvent>();
            let pass = morpheus_tools::subagent::acp::replay(&agent, session_id, cwd, chunk_tx);
            tokio::pin!(pass);
            let mut chunks_open = true;
            let result = loop {
                if chunks_open {
                    tokio::select! {
                        result = &mut pass => break result,
                        chunk = chunk_rx.recv() => match chunk {
                            Some(event) => {
                                let _ = event_tx.send(HarnessEvent::AgentsPaneEvent {
                                    pane,
                                    pane_id,
                                    epoch,
                                    // A replay belongs to no turn: unstamped.
                                    turn: 0,
                                    event,
                                });
                            }
                            None => chunks_open = false,
                        },
                    }
                } else {
                    break pass.await;
                }
            };
            let (ok, error) = match result {
                Ok(()) => (true, String::new()),
                Err(error) => (false, error),
            };
            let _ = event_tx.send(HarnessEvent::AgentsPaneReplayed {
                pane,
                pane_id,
                epoch,
                ok,
                error,
            });
        });
        #[cfg(test)]
        let _ = (
            agent,
            session_id,
            cwd,
            pane_id,
            epoch,
            event_tx,
            tokio_handle,
        );
    }
}

/// One worktree HANDOFF summary line from a pane timeline entry: the
/// recent user messages and agent replies that describe what the pane was
/// doing, so a cwd move never silently orphans its working context. Bounded
/// (the block rides the NEXT prompt once — a transcript would defeat it);
/// tool-call entries contribute their `detail` (the path/query/command the
/// call targeted) rather than their output tails.
fn pane_handoff_summary_line(entry: &SubagentTimelineEntry) -> Option<String> {
    const MAX_LINE: usize = 200;
    let mut line = match entry {
        SubagentTimelineEntry::UserMessage { text } => {
            format!("user: {}", text.trim())
        }
        SubagentTimelineEntry::AgentMessage { text } => {
            format!("agent: {}", text.trim())
        }
        SubagentTimelineEntry::Message { text } => {
            format!("reply: {}", text.trim())
        }
        SubagentTimelineEntry::Tool(call) => {
            let detail = call.detail.as_deref().unwrap_or_default();
            format!("tool {}: {}", call.title.trim(), detail.trim())
        }
        // Thoughts and plans are the agent's own scratchpad — the carried
        // context is the WORK, not the narration.
        _ => return None,
    };
    // Collapse whitespace: timeline text carries hard wraps and stream
    // chunking; the handoff wants one readable line per entry.
    line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > MAX_LINE {
        let cut: String = line.chars().take(MAX_LINE).collect();
        line = format!("{cut}…");
    }
    (!line.trim_end().ends_with(':')).then_some(line)
}

/// Build the worktree handoff block from a pane's local timeline: the LAST
/// few meaningful entries (`pane_handoff_summary_line`), newest last — the
/// same order the pane renders. An empty or all-noise timeline yields an
/// empty string: the block still announces the move, just carries nothing.
pub(in crate::app) fn pane_handoff_summary(timeline: &[SubagentTimelineEntry]) -> String {
    const MAX_ENTRIES: usize = 12;
    timeline
        .iter()
        .rev()
        .filter_map(pane_handoff_summary_line)
        .take(MAX_ENTRIES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

/// A pane's EFFECTIVE working directory — the ONE resolution every
/// consumer reads: the worktree path when the pane is pinned (`/worktree`),
/// the session's working directory otherwise. The ACP turn's
/// `acp::call(cwd)`, the MANAGER turn's `HarnessAssembly::cwd` (harness
/// root, `MorpheusTools` root, LSP, hooks) and the focus-time snapshot probe
/// all funnel through it, so a binding is either honored by every layer or
/// by none — the manager pane included (a morpheus pane is a pane like any
/// other here; only the `agents_worktree` TOOL refuses to target one).
pub(super) fn pane_effective_cwd(pane: &AgentPane, session_cwd: &str) -> PathBuf {
    morpheus::coordinator::turn::effective_cwd(
        pane.worktree.as_ref().map(|w| w.path.as_path()),
        session_cwd,
    )
}

/// One manager-pane turn: the pane's harness assembled through the ONE
/// assembly (`morpheus::assembly::assemble`), whose plain event stream is bridged
/// into the SAME typed `SubagentEvent` stream the external ACP panes emit —
/// both pane flavors render identically. The final report (the loop's
/// closure) arrives as `AgentsPaneDone`.
#[allow(clippy::too_many_arguments)]
fn run_manager_turn(
    pane: usize,
    pane_id: u64,
    epoch: u64,
    // The turn's generation (see `AgentPane::turn_id`): stamped into every
    // event this turn emits, so a stopped turn's late stream and terminal
    // Done are recognizable as stale.
    turn: u64,
    input: String,
    permission_rx: Option<
        tokio::sync::mpsc::UnboundedReceiver<
            morpheus_tools::subagent::events::PanePermissionAnswer,
        >,
    >,
    stop_signal: Arc<AtomicBool>,
    // The pane's steering queue: the user's "next request" messages, drained
    // by the harness before each of this turn's requests (the same contract
    // the main loop's queued input has).
    steer: morpheus::harness::queued_input::QueuedInput,
    assembly: morpheus::assembly::HarnessAssembly,
    session_id: String,
    store: crate::session::store::SessionStore,
    event_tx: tokio::sync::mpsc::UnboundedSender<HarnessEvent>,
) {
    // The panic path (outside the closure) still needs a sender: the
    // original moves into the closure below. The session id is cloned for
    // the same reason — the panic Done must carry the binding too (issue 6:
    // a panicked FIRST turn must still leave the pane bound on disk).
    let panic_tx = event_tx.clone();
    let panic_session = session_id.clone();

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // THE shared process runtime (`morpheus::assembly::agent_runtime`), not
        // a per-turn current_thread one: the pane now assembles MCP (and
        // may touch the LSP singleton) through the SAME assembly as the main
        // loop, and transports spawned on a per-turn runtime would be
        // killed (kill_on_drop) the moment the turn ends.
        let rt = morpheus::assembly::agent_runtime();
        rt.block_on(async move {
            let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
            // The manager loop's permission channel: with a TUI-connected
            // receiver the answers forwarded below reach the loop's
            // permission gate; without one (Yolo) the throwaway pair keeps
            // the loop self-contained.
            let (manager_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
            let (loop_tx, mut loop_rx) =
                tokio::sync::mpsc::unbounded_channel::<morpheus::harness::HarnessEvent>();

            // THE assembly (`morpheus::assembly::assemble`): the pane assembles
            // through the SAME path the main loop does — connector with the
            // user's tool-call mode + reasoning resolution, prompt-cache
            // headers keyed to the PANE's own session, MCP boot, RAG,
            // effective-context budget, skills — with its role declared as
            // parameters (pane instructions, MANAGER requester tag, bound
            // pane id, no queued input, no audit). An assembly failure
            // closes the pane's turn with it instead of an Error event (the
            // pane has its own Done contract, not the main chat's).
            let mut manager = match morpheus::assembly::assemble(assembly, &event_tx).await {
                Ok(manager) => manager,
                Err(error) => {
                    // Gate-free fact capture (see the external turn's record):
                    // the Done may be dropped by the handler's gates, the
                    // binding fact may not be lost with it.
                    morpheus::harness::agent_coworkers::record_session_binding(
                        pane_id,
                        session_id.clone(),
                    );
                    let _ = event_tx.send(HarnessEvent::AgentsPaneDone {
                        pane,
                        pane_id,
                        epoch,
                        turn,
                        output: error,
                        stop_reason: "error".to_string(),
                        // The manager session was already minted and its
                        // shell persisted by the pre-flight — the binding
                        // must reach the metadata even on a failed first
                        // turn, or a reboot would rebuild the pane UNBOUND
                        // (issue 6).
                        session: Some(session_id),
                    });
                    return;
                }
            };

            // The TUI's answer → the manager loop's permission action. The
            // mapping mirrors the main dialog's semantics: the first
            // allow-kind option the agent offered selects `Allow`/
            // `AllowOnce` per its kind; the cancel semantic denies.
            // Spawned AFTER the assemble: on an assembly failure the
            // forwarder would park forever on the receiver (a task leak).
            if let Some(mut permission_rx) = permission_rx {
                let forward_perm_tx = manager_perm_tx.clone();
                tokio::spawn(async move {
                    while let Some(answer) = permission_rx.recv().await {
                        // The manager's own box offers exactly the three
                        // options the bridge synthesized (`allow`,
                        // `allow_once`, `deny`) — matched EXACTLY, never by
                        // substring (a label like "don't always allow" must
                        // not read as an allow). An unknown id denies: an
                        // option this build didn't offer has no trustworthy
                        // allow semantics.
                        let action = match answer {
                            morpheus_tools::subagent::events::PanePermissionAnswer::Selected(
                                id,
                            ) => {
                                match id.as_str() {
                                    "allow" => morpheus::harness::PermissionAction::Allow,
                                    "deny" => morpheus::harness::PermissionAction::Deny,
                                    // `allow_once` AND any unknown id: the
                                    // once-grant cannot be replayed across
                                    // tools, so the blast radius stays one
                                    // call.
                                    _ => morpheus::harness::PermissionAction::AllowOnce,
                                }
                            }
                            morpheus_tools::subagent::events::PanePermissionAnswer::Cancel => {
                                morpheus::harness::PermissionAction::Deny
                            }
                        };
                        // `Err` = the loop already ended: nothing to answer.
                        let _ = forward_perm_tx.send(action);
                    }
                });
            }

            // Bridge: manager-loop events → typed SubagentEvents → the pane
            // channel. Same mapping the harness's internal sub-agent uses,
            // routed by pane index instead of call_ref.
            //
            // The bridge is ALSO the only place the loop's TERMINAL event is
            // visible (`Done` / `Stopped` / `Error`), so it records it here.
            // It used to fall into `_ => Vec::new()` alongside every
            // display-only event, which threw the information away and left
            // the turn's close unable to say anything but "stopped or
            // errored" — the reason a cancelled-looking message appeared for
            // turns nobody cancelled. `Arc<Mutex<_>>` because the future
            // below is `move`d into the bridge task while the close reads it
            // after the join.
            let terminal_cause: Arc<std::sync::Mutex<Option<TerminalCause>>> =
                Arc::new(std::sync::Mutex::new(None));
            let cause_sink = terminal_cause.clone();
            let bridge_tx = event_tx.clone();
            let bridge = tokio::spawn(async move {
                use morpheus_tools::subagent::internal::InternalEventBridge;
                let mut internal = InternalEventBridge::new();
                while let Some(event) = loop_rx.recv().await {
                    // The terminal event: record WHY the loop ended, then let
                    // the display mapping below decide whether it also has a
                    // visual representation (none of the three does — the
                    // pane renders its own close).
                    match &event {
                        HarnessEvent::Done {
                            checkup_verdict, ..
                        } => {
                            *cause_sink.lock().unwrap() = Some(TerminalCause {
                                kind: TerminalKind::Done,
                                detail: checkup_verdict.clone(),
                            });
                        }
                        HarnessEvent::Stopped { .. } => {
                            *cause_sink.lock().unwrap() = Some(TerminalCause {
                                kind: TerminalKind::Stopped,
                                detail: None,
                            });
                        }
                        HarnessEvent::Error { message, .. } => {
                            *cause_sink.lock().unwrap() = Some(TerminalCause {
                                kind: TerminalKind::Error,
                                detail: Some(message.clone()),
                            });
                        }
                        _ => {}
                    }
                    let events: Vec<SubagentEvent> = match event {
                        HarnessEvent::Token { text } => {
                            vec![InternalEventBridge::message(text)]
                        }
                        HarnessEvent::Reasoning { text } => {
                            vec![InternalEventBridge::thought(text)]
                        }
                        HarnessEvent::ToolCall { tool, input, .. } => {
                            let mut events = vec![internal.tool_call(&tool, &input)];
                            if tool == "plan_todo_write"
                                && let Some(plan) =
                                    InternalEventBridge::plan_from_todo_write(&input)
                            {
                                events.push(plan);
                            }
                            events
                        }
                        HarnessEvent::ToolResult { output, .. } => {
                            internal.tool_result(&output).into_iter().collect()
                        }
                        HarnessEvent::ToolError { error, .. } => {
                            internal.tool_error(&error).into_iter().collect()
                        }
                        HarnessEvent::ContextInfo { info } => {
                            vec![InternalEventBridge::usage(
                                info.total_tokens,
                                info.max_tokens,
                            )]
                        }
                        HarnessEvent::PermissionRequest {
                            tool,
                            description,
                            args,
                        } => {
                            // The loop awaits a permission decision: the
                            // pane's box opens (the SAME
                            // `SubagentEvent::PermissionRequest` the ACP
                            // panes emit — one UI answers both). The
                            // synthetic call id is unique per request (the
                            // box answers through the pane's channel, not
                            // by patching a row).
                            static MANAGER_CALL: std::sync::atomic::AtomicU64 =
                                std::sync::atomic::AtomicU64::new(1);
                            let call_id = format!(
                                "manager-perm-{}",
                                MANAGER_CALL.fetch_add(1, std::sync::atomic::Ordering::Relaxed,)
                            );
                            vec![SubagentEvent::PermissionRequest {
                                call_id,
                                title: description,
                                kind: morpheus_tools::subagent::events::ToolKind::Execute,
                                raw_input: serde_json::json!({ "tool": tool, "args": args }).into(),
                                options: vec![
                                    morpheus_tools::subagent::events::AgentPermissionOption {
                                        id: "allow".to_string(),
                                        name: "Allow".to_string(),
                                        kind: "allow_always".to_string(),
                                    },
                                    morpheus_tools::subagent::events::AgentPermissionOption {
                                        id: "allow_once".to_string(),
                                        name: "Allow Once".to_string(),
                                        kind: "allow_once".to_string(),
                                    },
                                    morpheus_tools::subagent::events::AgentPermissionOption {
                                        id: "deny".to_string(),
                                        name: "Deny".to_string(),
                                        kind: "reject_once".to_string(),
                                    },
                                ],
                            }]
                        }
                        _ => Vec::new(),
                    };
                    for event in events {
                        let _ = bridge_tx.send(HarnessEvent::AgentsPaneEvent {
                            pane,
                            pane_id,
                            epoch,
                            turn,
                            event,
                        });
                    }
                }
            });

            morpheus::assembly::run_assembled_loop(
                &mut manager,
                &input,
                loop_tx,
                answer_rx,
                perm_rx,
                stop_signal,
                // The pane's own "next request" queue: `pump_agents_pane_
                // queued_requests` feeds it one message at a time while the
                // user chose that target, and the harness drains it before
                // each request. Leftovers at closure are recovered into the
                // pane's "next loop" queue (`serve_agents_pane_user_queues`)
                // instead of being stranded.
                Some(steer),
            )
            .await;

            // The report is the loop's closure (its last post-tool-call
            // message). A loop that closed without one still OWES a report
            // (see `AgentCoworkerTurnEnd`), so the last assistant text is used as the
            // fallback — the same policy the ACP panes' `TurnClosure`
            // already applies (its `previous` segment).
            //
            // `last_assistant_text` (not `final_answer`) is what makes the
            // fallback possible: `close_loop` promotes only a TRAILING
            // assistant item, so a turn that ended on a tool result has its
            // text one item earlier, invisible to `final_answer`. Scanning
            // past tool calls/results recovers exactly the text the model
            // really wrote.
            manager.context_manager.close_loop();
            let final_answer = manager.context_manager.final_answer();
            let fallback = manager.context_manager.last_assistant_text();
            // Persist the manager session BEFORE dropping the harness: the
            // context snapshot must be taken while the manager still owns
            // it. The pane is a REAL morpheus session — the history panel lists
            // it and reopening it restores the conversation.
            let context_state = manager.context_manager.save_state();
            let display = manager_context_display(&context_state);
            drop(manager);
            let _ = bridge.await;
            persist_manager_session(&store, &session_id, display, context_state);

            // WHY the loop ended, as captured by the bridge. `None` means the
            // loop never emitted a terminal event at all (a panic path the
            // catch_unwind below handles separately, or a receiver drop).
            let cause = terminal_cause.lock().unwrap().take();
            let (output, end) = close_pane_turn(cause.as_ref(), final_answer, fallback);
            // Gate-free fact capture (see the external turn's record): the
            // morpheus pane's binding survives a Done the handler drops.
            morpheus::harness::agent_coworkers::record_session_binding(pane_id, session_id.clone());
            let _ = event_tx.send(HarnessEvent::AgentsPaneDone {
                pane,
                pane_id,
                epoch,
                turn,
                output,
                stop_reason: end.as_str().to_string(),
                // The morpheus pane's session id IS the bound session: carried
                // here so the Done handler seeds `pane.session_id` AND
                // promotes the persisted pane ref to `Bound { agent: morpheus,
                // session_id }` — without it the binding never reached
                // disk and a reboot rebuilt the pane UNBOUND (empty chat,
                // issue 6), even though the manager session itself was
                // fully persisted in the store.
                session: Some(session_id),
            });
        });
    }));
    if outcome.is_err() {
        // The panic path binds too (issue 6's contract): capture the fact
        // gate-free so the binding lands even when the handler drops the
        // synthetic Done.
        morpheus::harness::agent_coworkers::record_session_binding(pane_id, panic_session.clone());
        // A panicking turn must never leave the pane Running forever (the
        // spinner would spin with nothing behind it and Esc would have
        // nothing to stop).
        let _ = panic_tx.send(HarnessEvent::AgentsPaneDone {
            pane,
            pane_id,
            epoch,
            turn,
            output: "the manager turn panicked".to_string(),
            stop_reason: "error".to_string(),
            session: Some(panic_session),
        });
    }
}

/// Project a manager context snapshot into the DISPLAY messages the session
/// store persists: user prompts (synthetic provenance kept) and assistant
/// text/closures, each carrying its DISPLAY-ONLY reasoning (`None` for user
/// prompts and reasoning-less responses — see
/// [`morpheus::harness::context::ContextItem::Assistant::reasoning`]). Tool
/// calls/results are structural context, not display prose — the pane's live
/// timeline already showed them.
/// Rebuild the manager pane's replayed event stream from the session's
/// persisted CONTEXT — the same authoritative snapshot `run_manager_turn`
/// restores into a fresh harness on resume.
///
/// The old cold-restore replayed the session's DISPLAY projection instead:
/// `manager_context_display` persists only User/Assistant prose (text +
/// reasoning), so every `ContextItem::ToolCall`/`ToolResult` — the tool rows
/// and output tails that make up most of a working manager pane's visible
/// history — was silently dropped from the rebuilt timeline. The bug
/// surfaced when adding a pane: `open_agents_split` rebuilds the split from
/// scratch (`AgentsSplitState::new`), `trigger_agents_pane_replays` refills
/// every bound pane with an empty timeline, and the morpheus pane came back
/// stripped to its prose while the ACP panes (full `session/load` replay)
/// kept their tool rows.
///
/// The mapping mirrors the LIVE bridge (`InternalEventBridge` plus the
/// `HarnessEvent` → `SubagentEvent` match in `run_manager_turn`), driven by
/// the same item order the harness appended during its turns:
///
/// - `User` → `UserMessage` (the pane's input rail, same as the old
///   display-projection replay).
/// - `Assistant`/`Closure` → the item's reasoning first (the live loop
///   streams reasoning BEFORE the text), then the text as `Message`.
/// - `ToolCall` → an `InProgress` row with the tool's real name and its raw
///   arguments as the initial tail — the same shape the bridge synthesizes.
/// - `ToolResult` → a `ToolCallUpdate` patching the call row (paired by
///   the exact `call_id`, stronger than the live bridge's FIFO contract)
///   to `Completed` with its output tail and, for the edit family, the
///   same one-line diff summary the bridge derives.
/// - `Error` → a `Message` entry, so a failed turn's error line survives
///   a process restart like any other transcript row.
/// - `Compaction` → `UserMessage` with the checkpoint's summary (the
///   "conversation continues from here" prose is model-visible, so the
///   replayed pane shows it too).
///
/// NO model-visibility filtering here: the pane timeline is a TRANSCRIPT
/// (what streamed live into it), not the model view. Useless-chain sweeps,
/// cancelled-run debris and compaction boundaries only hide items from the
/// MODEL — the user watched every one of them stream into the pane, so the
/// rebuilt timeline keeps them; masking likewise never rewrote the row.
pub(in crate::app) fn manager_context_replay_events(
    context: &morpheus::harness::context::ContextManagerState,
) -> Vec<SubagentEvent> {
    use morpheus::harness::context::ContextItem;

    let mut events: Vec<SubagentEvent> = Vec::new();
    // Announced-but-unresolved calls by their context `call_id` → the tool
    // name (the edit-family diff summary is derived from it). A result
    // pairs back through this map; an unpaired result (its call was hidden
    // or absent) lands nowhere, exactly like the live bridge's FIFO
    // contract — and the TUI's apply locates the row to patch BY ID, so
    // the update never needs an index.
    let mut announced: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for item in &context.items {
        match item {
            ContextItem::User { original, .. } => {
                if !original.is_empty() {
                    events.push(SubagentEvent::UserMessage {
                        text: original.clone(),
                    });
                }
            }
            ContextItem::Assistant {
                original,
                reasoning,
                ..
            }
            | ContextItem::Closure {
                content: original,
                reasoning,
                ..
            } => {
                if let Some(reasoning) = reasoning.as_ref().filter(|r| !r.is_empty()) {
                    events.push(SubagentEvent::Thought {
                        text: reasoning.clone(),
                    });
                }
                if !original.is_empty() {
                    events.push(SubagentEvent::Message {
                        text: original.clone(),
                    });
                }
            }
            ContextItem::ToolCall {
                id: _,
                call_id,
                name,
                arguments,
                ..
            } => {
                let input: serde_json::Value =
                    serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null);
                events.push(SubagentEvent::ToolCall {
                    id: call_id.clone(),
                    title: name.clone(),
                    kind: morpheus_tools::subagent::internal::tool_kind(name),
                    status: ToolCallStatus::InProgress,
                    raw_input: Some(input),
                    tool: Some(name.clone()),
                });
                announced.insert(call_id.clone(), name.clone());
            }
            ContextItem::ToolResult {
                id: _,
                call_id,
                content,
                ..
            } => {
                let Some(tool) = announced.remove(call_id) else {
                    continue;
                };
                events.push(SubagentEvent::ToolCallUpdate {
                    id: call_id.clone(),
                    status: Some(ToolCallStatus::Completed),
                    title: None,
                    raw_output: None,
                    content: ToolOutputBlock {
                        text: morpheus_tools::subagent::internal::output_tail(content),
                        skipped: 0,
                        diff: morpheus_tools::subagent::internal::diff_summary_from_result(
                            tool, content,
                        ),
                    },
                });
            }
            ContextItem::Error { content, .. } => {
                events.push(SubagentEvent::Message {
                    text: content.clone(),
                });
            }
            ContextItem::Compaction { summary, .. } => {
                events.push(SubagentEvent::UserMessage {
                    text: summary.clone(),
                });
            }
            _ => {}
        }
    }
    events
}

/// Project a manager context snapshot into the DISPLAY messages the session
/// store persists: ONE message per context item GROUP, using the shared rule
/// [`morpheus::harness::context::display_groups`] that the store's `update_ctx_ids`
/// also derives its mapping from (a tool call absorbs every result carrying
/// its `call_id`; every other item is a message of its own).
///
/// The 1:1 GROUP↔MESSAGE invariant is load-bearing, not cosmetic: the store
/// only records a message → context boundary when the display projection
/// lines up with the context groups, and Revert/Fork resolve a clicked
/// message id through those boundaries. The old prose-only projection
/// (User/Assistant/Closure, tool calls dropped) broke the invariant on any
/// turn that used a tool, so `update_ctx_ids` refused to guess and left the
/// map empty — every Revert/Fork on a real manager session failed with
/// "Could not record the revert" / "Could not create the branch".
///
/// The projection is also what the history view shows when the manager
/// session is reopened as a normal chat, so tool rows belong here anyway.
pub(in crate::app) fn manager_context_display(
    context: &morpheus::harness::ContextManagerState,
) -> Vec<(crate::types::MessageRole, Vec<crate::types::Part>)> {
    use crate::types::{MessageRole, Part, ReasoningPart, TextPart, ToolPart, ToolStatus};
    use morpheus::harness::context::ContextItem;

    fn text_part(text: &str) -> Part {
        Part::Text(TextPart {
            text: text.to_string(),
            synthetic: false,
        })
    }

    /// Reasoning precedes the text, mirroring the live timeline's arrival order
    /// (`push_reasoning` before the text tokens). `None` reasoning emits no
    /// part, so reasoning-less turns persist as before.
    fn push_text(parts: &mut Vec<Part>, text: &str, reasoning: Option<&String>) {
        if let Some(reasoning) = reasoning.filter(|r| !r.is_empty()) {
            parts.push(Part::Reasoning(ReasoningPart {
                text: reasoning.clone(),
                collapsed: true,
            }));
        }
        parts.push(text_part(text));
    }

    let mut display: Vec<(MessageRole, Vec<Part>)> = Vec::new();
    for group in morpheus::harness::context::display_groups(context) {
        // The group's FIRST item sets the role: compaction and commands read
        // as user turns (the model-visible checkpoint prose included), a tool
        // call and its results (or an orphan result) as assistant output.
        let role = match group[0] {
            ContextItem::User { .. }
            | ContextItem::Compaction { .. }
            | ContextItem::UserCommand { .. } => MessageRole::User,
            _ => MessageRole::Assistant,
        };
        let mut parts: Vec<Part> = Vec::new();
        for item in group {
            match item {
                ContextItem::User { original, .. } => parts.push(text_part(original)),
                ContextItem::Compaction { summary, .. } => parts.push(text_part(summary)),
                ContextItem::UserCommand { content, .. } => parts.push(text_part(content)),
                ContextItem::Error { content, .. } => parts.push(text_part(content)),
                ContextItem::Assistant {
                    original,
                    reasoning,
                    ..
                } => push_text(&mut parts, original, reasoning.as_ref()),
                ContextItem::Closure {
                    content, reasoning, ..
                } => push_text(&mut parts, content, reasoning.as_ref()),
                ContextItem::ToolCall {
                    call_id,
                    name,
                    arguments,
                    ..
                } => {
                    let input = serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null);
                    parts.push(Part::Tool(ToolPart {
                        tool: name.clone(),
                        input,
                        output: None,
                        status: ToolStatus::Completed,
                        tool_call_id: Some(call_id.clone()),
                        is_start: true,
                        is_streaming: false,
                        cached_line_count: None,
                        lsp_notes: None,
                    }));
                }
                // Same group as its call (the shared rule pairs them), so the
                // output lands on that call's part instead of a message of
                // its own. An orphan result has no tool part to land on: it
                // stays prose, exactly as the mapper counts it.
                ContextItem::ToolResult { content, .. } => {
                    match parts.iter_mut().find(|part| matches!(part, Part::Tool(_))) {
                        Some(Part::Tool(tool)) => {
                            tool.output = Some(match tool.output.take() {
                                Some(previous) => format!("{previous}\n{content}"),
                                None => content.clone(),
                            });
                        }
                        _ => parts.push(text_part(content)),
                    }
                }
            }
        }
        display.push((role, parts));
    }
    // A call whose result never arrived was cut off mid-dispatch (the turn
    // was interrupted): it is the LAST item of the snapshot, so leaving it
    // `Completed` would claim an outcome that never happened. A call in the
    // MIDDLE without a result lost it to a sweep — those did complete, so
    // only the trailing one is demoted.
    if let Some(ContextItem::ToolCall { .. }) = context.items.back()
        && let Some((_, parts)) = display.last_mut()
        && let Some(Part::Tool(tool)) = parts.iter_mut().find(|part| matches!(part, Part::Tool(_)))
        && tool.output.is_none()
    {
        tool.status = ToolStatus::Running;
    }
    display
}

/// Persist one manager turn: load the session shell (for `created_at`),
/// replace its display messages with the fresh projection, and enqueue the
/// save with the context snapshot. A missing shell (first turn) builds one.
pub(in crate::app) fn persist_manager_session(
    store: &crate::session::store::SessionStore,
    session_id: &str,
    display: Vec<(crate::types::MessageRole, Vec<crate::types::Part>)>,
    context: morpheus::harness::ContextManagerState,
) {
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let mut session = store
        .load_session(session_id)
        .unwrap_or_else(|| crate::types::Session {
            id: session_id.to_string(),
            title: "agents manager".to_string(),
            created_at,
            messages: Vec::new(),
            title_generated: false,
            provider: None,
            model: None,
            reasoning: None,
            agents_panes: None,
            agents_mode_open: false,
            agents_hidden: Vec::new(),
            input_history: Vec::new(),
            ctx_ids: std::collections::HashMap::new(),
            main_agent_worktree: None,
        });
    // The projection is already grouped one-message-per-context-group (see
    // `manager_context_display`), so the ids are purely positional.
    session.messages = display
        .into_iter()
        .enumerate()
        .map(|(i, (role, parts))| crate::types::Message {
            id: format!("msg-{i}"),
            role,
            parts,
            created_at: 0,
            agent: None,
            model: None,
        })
        .collect();
    store.save_session_async_with_context(&mut session, context);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tl_user(text: &str) -> SubagentTimelineEntry {
        SubagentTimelineEntry::UserMessage {
            text: text.to_string(),
        }
    }

    fn tl_message(text: &str) -> SubagentTimelineEntry {
        SubagentTimelineEntry::Message {
            text: text.to_string(),
        }
    }

    fn tl_tool(title: &str, detail: Option<&str>) -> SubagentTimelineEntry {
        SubagentTimelineEntry::Tool(
            crate::routes::session::right_panel::types::SubagentToolCall {
                id: "t1".into(),
                title: title.to_string(),
                kind: morpheus_tools::subagent::events::ToolKind::Read,
                tool: None,
                status: morpheus_tools::subagent::events::ToolCallStatus::Completed,
                output_tail: None,
                diff: None,
                detail: detail.map(str::to_string),
            },
        )
    }

    fn tl_thought(text: &str) -> SubagentTimelineEntry {
        SubagentTimelineEntry::Thought {
            text: text.to_string(),
        }
    }

    #[test]
    fn handoff_summary_carries_the_recent_work_lines() {
        let timeline = vec![
            tl_user("find the failing test in the worktree"),
            tl_message("Looking for failing tests in the worktree…"),
            tl_tool("Search", Some("src/lib.rs")),
            tl_thought("hmm"),
            tl_user("fix it"),
            tl_message("Fixed it."),
        ];
        let summary = pane_handoff_summary(&timeline);
        let lines: Vec<&str> = summary.lines().collect();
        // One collapsed line per carrying entry, chronological order,
        // thoughts dropped.
        assert_eq!(lines.len(), 5, "thoughts are dropped: {summary}");
        assert!(summary.contains("user: find the failing test"));
        assert!(summary.contains("tool Search: src/lib.rs"));
        assert!(summary.ends_with("reply: Fixed it."));
    }

    #[test]
    fn handoff_summary_is_bounded_and_whitespace_collapsed() {
        let long = "x".repeat(600);
        let wrapped = format!("{long}\n{long}");
        let timeline = (0..40)
            .map(|i| tl_message(&format!("chunk-{i} {wrapped}")))
            .collect::<Vec<_>>();
        let summary = pane_handoff_summary(&timeline);
        // Only the LAST 12 entries survived, each capped at 200 chars.
        assert!(
            summary.lines().count() <= 12,
            "bounded to 12: {len}",
            len = summary.lines().count()
        );
        for line in summary.lines() {
            assert!(line.chars().count() <= 201, "line capped: {line}");
        }
        assert!(summary.contains("chunk-39"), "the newest entry survived");
        assert!(!summary.contains("chunk-27"), "older entries dropped");
    }

    #[test]
    fn an_empty_timeline_yields_an_empty_handoff_but_the_block_still_announces() {
        assert_eq!(pane_handoff_summary(&[]), "");
        let input = assemble_turn_input(
            Some("[workspace moved] …\n\n".into()),
            None,
            "do the thing".into(),
        );
        assert!(input.starts_with("[workspace moved]"));
        assert!(input.ends_with("do the thing"));
    }

    /// A path with a space in it is the reason quoting exists: `/worktree
    /// list` prints copyable paths, so a listing row must be pasteable back
    /// into `add`. Splitting on whitespace made that impossible — the path
    /// broke into two tokens and the refusal that followed named a branch
    /// and a path the user never typed.
    #[test]
    fn a_quoted_path_keeps_its_spaces_as_one_token() {
        assert_eq!(
            split_args("add \"/tmp/my tree\" feat").expect("well-formed"),
            vec!["add", "/tmp/my tree", "feat"]
        );
        // Quotes may appear mid-token and concatenate with bare text.
        assert_eq!(
            split_args("add /tmp/\"my tree\" feat").expect("well-formed"),
            vec!["add", "/tmp/my tree", "feat"]
        );
        // An escaped quote inside quotes is a literal quote character.
        assert_eq!(
            split_args(r#"add "a\"b" feat"#).expect("well-formed"),
            vec!["add", "a\"b", "feat"]
        );
        // No quotes at all: the ordinary shape is unchanged.
        assert_eq!(
            split_args("add feat").expect("well-formed"),
            vec!["add", "feat"]
        );
        // Runs of whitespace collapse; leading/trailing spaces add no token.
        assert_eq!(
            split_args("  add    feat  ").expect("well-formed"),
            vec!["add", "feat"]
        );
    }

    /// An unterminated quote is refused rather than guessed at: guessing
    /// where the path ends would silently bind a DIFFERENT directory than
    /// the user meant.
    #[test]
    fn an_unterminated_quote_is_malformed() {
        assert_eq!(split_args("add \"/tmp/my tree feat"), None);
        assert_eq!(parse_worktree_command(Some("add \"/tmp/x feat")), None);
    }

    /// The quoted form reaches the command parser intact, so `add` sees a
    /// two-argument tail even when the path contains a space.
    #[test]
    fn parse_accepts_a_quoted_path_as_one_argument() {
        match parse_worktree_command(Some("add \"/tmp/my tree\" feat")) {
            Some(WorktreeCommand::Add { path, branch }) => {
                assert_eq!(path.as_deref(), Some("/tmp/my tree"));
                assert_eq!(branch, "feat");
            }
            other => panic!("expected a two-argument add, got {other:?}"),
        }
        // `list` and `exit` still parse exactly as before.
        assert!(matches!(
            parse_worktree_command(Some("list")),
            Some(WorktreeCommand::List)
        ));
        assert!(matches!(
            parse_worktree_command(Some("exit")),
            Some(WorktreeCommand::Exit)
        ));
        // An empty tail stays malformed (the command takes arguments).
        assert!(parse_worktree_command(Some("   ")).is_none());
    }
}
