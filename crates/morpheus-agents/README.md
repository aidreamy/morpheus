# morpheus-agents

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/aidreamy/morpheus/blob/main/LICENSE)

Agent integration crate for [Morpheus](https://github.com/aidreamy/morpheus), a coding agent for the terminal.

`morpheus-agents` owns the sub-agent caller used by the agent panel. It is not a
tool crate: the model-facing pieces (`subagent_call`'s description, the review
severity contract) sit next to the caller machinery that runs an external
harness turn end to end — path detection and the ACP registry, the protocol
client, session resume, cancellation, the typed event stream and the persisted
per-agent model picks.

## Modules

| Module        | What it holds                                                                                     |
| ------------- | ------------------------------------------------------------------------------------------------- |
| `acp`         | The ACP client: registry, PATH detection, the initialize → session/new → session/prompt turn, replay |
| `closure`     | The turn's final-message accumulator (only the last message is the report)                         |
| `events`      | Typed event stream mapped from `session/update` notifications, consumed by the TUI sub-agent box   |
| `internal`    | The same typed events synthesized for the internal (nested harness) sub-agent                      |
| `model_picks` | Persisted per-agent model choices, injected-directory store                                       |
| `opencode`    | Native OpenCode transport: spawns `opencode serve` and drives turns over its HTTP + SSE API     |
| `palette`     | Brand palettes for painting each agent with its own logo colors                                   |
| `severity`    | The `code_review` severity header contract and its extraction                                     |
| `sandbox`     | Kernel-pinned `fs/*` handler sandbox (Unix only)                                                  |
| `types`       | `SubAgentCallInput` / `SubAgentCallOutput`                                                        |

## Usage

```rust,ignore
use morpheus_agents::SubAgent;

let sub = SubAgent::new();
sub.resolve_input(Some("review this PR".into()))?; // stores + returns
sub.resolve_input(None)?;                          // reuses the stored message
```

The crate is usable standalone: nothing here reads Morpheus's own directories
or configuration — the caller injects the storage directory for `model_picks`,
and every other piece is pure protocol and state.

### OpenCode runs natively

OpenCode is the one harness with a documented server of its own. Instead of
translating its turn through ACP, `opencode` spawns `opencode serve` in the
workspace and drives the turn over the server's HTTP API: `prompt_async` for
the prompt (or the command endpoint for pane slash commands), server-sent
events for the stream, tool metadata for file-edit diffs, token accounting
for context usage, and REST endpoints for permissions, abort, session
resume, todos, the agent/command catalog and the model list.
`acp::call`, `acp::open_session`, `acp::session_snapshot` and `acp::replay`
route to it automatically, so consumers keep the same types and calls;
`MORPHEUS_OPENCODE_TRANSPORT=acp` restores the generic transport.

## Requirements

- Rust 2024 edition (≥ 1.85)
- External agent CLIs only when dispatching to them through `subagent_call`

## Related crates

- [`morpheus-tools`](https://crates.io/crates/morpheus-tools): the tools the harness exposes to the LLM
- [`morpheus-sdk`](https://crates.io/crates/morpheus-sdk): agent harness SDK
- [`morpheus-tui`](https://crates.io/crates/morpheus-tui): terminal interface

## License

Apache-2.0. See [LICENSE](https://github.com/aidreamy/morpheus/blob/main/LICENSE).
