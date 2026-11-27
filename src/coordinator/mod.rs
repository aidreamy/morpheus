//! Coordination - the layer that OWNS the agent tree.
//!
//! `harness` runs ONE agent for ONE turn. This module decides *who runs when*,
//! *with which tools*, and *how a turn's ending is classified* - the parts of
//! coordination that are policy over plain data and therefore have no business
//! living in a renderer.
//!
//! ## Why a module and not a crate
//!
//! The coordinator consumes harness types (`AgentCoworkerTurnEnd`), and a
//! future GUI consumes the coordinator from the SAME library crate. Promoting
//! it to a workspace crate now would freeze those types as public API while the
//! design is still moving. The escape hatch stays open the same way
//! `src/daemon/mod.rs` documents its own: lift the shared pieces into a crate
//! only when a SECOND consumer (a separate GUI process) actually needs the
//! boundary.
//!
//! ## Layering contract (enforced by review, cheap to check)
//!
//! ```text
//! coordinator → harness, (later) assembly, session, git
//! assembly    → harness
//! harness     → mcp, git::worktree::validate_name, morpheus-tools
//! tui (bin)   → all of the above
//! ```
//!
//! - `coordinator` NEVER imports `crate::tui` - that is the whole point.
//! - `harness` NEVER imports `coordinator` or `assembly`: the loop must not
//!   depend on the layer that builds and drives it.
//! - The coworker REGISTRY ([`crate::harness::agent_coworkers`]) stays in
//!   `harness`: it is the loop's peer interface (`harness::core` calls it every
//!   turn), so moving it here would invert the arrow. The coordinator owns the
//!   policy and the effects; the registry is the mailbox.
//!
//! ## What lives here today
//!
//! - [`policy`] - the pane turn's tool policy, and the reason it is NOT the
//!   Manager mode's restriction.
//! - [`turn`] - the pure per-turn decision logic: input composition, effective
//!   cwd, and the ending classification.
//!
//! ## What still lives in the TUI and should move here
//!
//! - `dispatch_to_pane`'s turn execution (the harness assembly it builds and
//!   the spawn it performs), which becomes `coordinator::dispatch` + a
//!   `PaneSpec` snapshot type;
//! - the worktree/compact op executors, the wake policy, session binding and
//!   persistence;
//! - `pane_turn_picks`/`model_option_id`, which must come after
//!   `agent_command_map::match_option` (a pure ACP-option resolver currently
//!   parked under `routes::session`) moves into the library.

pub mod policy;
pub mod turn;
