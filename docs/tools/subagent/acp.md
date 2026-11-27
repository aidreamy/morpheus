# `acp` - the external ACP harness engine

`acp` is the part of the crate that actually talks to other agent
harnesses: it resolves the agent name, spawns the harness in ACP server
mode, drives a full [Agent Client Protocol](https://agentclientprotocol.com/)
turn as a client, streams the agent's output in real time, and accumulates
the result until the turn ends.

This module is also the crate's public façade: OpenCode is routed to its
native server before any ACP machinery runs (see
[opencode.md](opencode.md)), so the signatures below cover both transports
unchanged.

There are two entry points over the same turn engine:

```rust,ignore
// One turn, one process: the `subagent` tool and prompt correction.
pub async fn call(
    agent: &str,
    input: &str,
    resume: Option<String>,
    picks: &[(String, String)],
    mode: Option<&str>,
    permission_rx: Option<UnboundedReceiver<PanePermissionAnswer>>,
    cwd: PathBuf,
    stop_signal: Arc<AtomicBool>,
    chunk_tx: UnboundedSender<SubagentEvent>,
) -> Result<(String, String, Option<String>), String>;

// One live session per `/agents` pane, reused across turns.
pub fn open_session(
    agent: &str,
    resume: Option<String>,
    cwd: PathBuf,
) -> Result<SessionHandle, String>;
```

`call` is the one-shot shape: it spawns the harness, runs exactly one turn
and tears the process down, returning the accumulated output, the ACP stop
reason and the session id. `open_session` returns a [`SessionHandle`] whose
supervisor thread keeps the process, the handshake and the session alive
between turns; only the named rebuild causes (rewind, cwd change, harness
death, rejected resume, protocol error, hidden/closed pane, restart) open a
new one. The full guarantee contract is
[acp-persistent-session-guarantees.md](acp-persistent-session-guarantees.md).

In both shapes `chunk_tx` receives typed events as they are produced (for the
live TUI display); a `call` session runs on a dedicated current-thread runtime
inside `tokio::task::spawn_blocking`, and a pane session runs on its own
supervisor thread, so the ambient async runtime is never blocked.

---

## The agent registry (`ACP_AGENTS`)

`ACP_AGENTS` is the static table of supported ACP harnesses, modeled by the
[`Agent`] struct:

```rust,ignore
pub struct Agent {
    pub name: &'static str,           // public name the model passes as `agent`
    pub command: &'static str,        // executable that speaks ACP over stdio
    pub args: &'static [&'static str],// args that put it into ACP server mode
    pub requires: &'static [&'static str], // binaries that must be in PATH
    pub install_hint: &'static str,   // guidance when the spawn fails
    pub model: &'static str,          // preferred session model ("" = harness default)
}
```

There are **no per-agent prompt flags**: the prompt travels as the ACP
`session/prompt` payload, never as a command-line argument. Adding an agent
is purely a table entry - anything registered in the official ACP registry
works.

### Model selection (`model`)

Some harnesses pick a **different default model for ACP than for their own
CLI** - and the ACP default may be a paid-tier model the user's existing
login cannot use (kilo, for instance, defaults its ACP sessions to
`kilo/google/gemini-3-pro-image`, which fails the prompt with
`"You need to sign in to use this model."` even though `kilo run` works
out of the box with the provider keys in the environment). When an `Agent`
entry names a `model`, `run_session` issues the standard ACP
[`session/set_config_option`](https://agentclientprotocol.com/) request
after `session/new` to switch the session to it. A model the harness does
not offer aborts the call with a clear error instead of a confusing
auth failure; an empty `model` leaves the harness default untouched.

| Name | ACP invocation |
|---|---|
| `gemini` | `gemini --experimental-acp` |
| `goose` | `goose acp` |
| `opencode` | `opencode acp` |
| `kilo` | `kilo acp` |
| `cline` | `cline --acp` |
| `devin` | `devin acp` |
| `claude` | `npx -y @agentclientprotocol/claude-agent-acp@latest` (official adapter) |
| `codex` | `npx -y @agentclientprotocol/codex-acp@latest` (official adapter) |

Two design points behind the table:

- **Only ACP-capable harnesses are listed.** The protocol contract replaces
  the old one-shot CLI scraping (per-agent `input_flag` tables that broke on
  every CLI update). Agents without ACP support (`cursor`, `aider`,
  `interpreter`) were removed - they cannot satisfy the contract.
- **`claude` and `codex` are not ACP-native yet**: their entries spawn the
  official Zed ACP adapters via `npx` (the same route the SDK's own
  `AcpAgent::claude_agent()` / `AcpAgent::codex()` helpers take), and their
  `requires` list demands both the engine binary and `npx`.

---

## `detect_installed` - which harnesses exist

```rust,ignore
pub fn detect_installed() -> &'static Vec<&'static str>
```

Scans `PATH` for each registered agent's required binaries and returns the
names found. The result is cached in a `OnceLock`, so detection runs **once
per process** - repeated calls return the cached list. `SubAgent::new()`
uses this to tailor the tool description's agent enum to what is actually
installed.

---

## `validate_agent` - name checking

```rust,ignore
pub fn validate_agent(agent: &str) -> Result<(), String>
```

Returns `Ok` when the name is in `ACP_AGENTS`, else an error listing every
supported agent:

```
Unsupported agent 'nope'. Supported agents (ACP): gemini, goose, ….
Use bash_run for shell commands.
```

This runs first inside `call`, so an unknown name fails before anything is
spawned.

---

## The ACP turn pipeline

The steps below are shared by `call` and by the pane supervisor. The only
difference is lifetime: `call` runs them once and exits, the pane supervisor
runs the spawn/handshake once and then repeats picks/mode/prompt per turn.

1. **Validate** the agent name (`validate_agent`) and build the
   [`AcpAgent`](https://docs.rs/agent-client-protocol) launcher for its
   command + args.
2. **Spawn & initialize.** The harness subprocess is spawned speaking
   JSON-RPC over stdio; the client sends `initialize` advertising the `fs`
   capability (read/write text file) so conformant agents may delegate file
   operations to morpheus.
3. **Session.** `session/resume` (or `session/load` for a cold replay) is
   used when the harness advertises the capability and a stored id exists;
   otherwise `session/new` opens a session rooted at `cwd`. When the agent
   entry names a `model`, the client switches the session to it via
   `session/set_config_option` (see [Model
   selection](#model-selection-model) above). On a live pane session the
   picks and mode are only re-sent when the user changed them.
4. **Authenticate lazily.** There is no proactive `authenticate`: the open
   (or a later operation) is rejected with `auth_required`, one advertised
   method is tried, and the same operation is retried on the same session.
5. **Prompt.** `session/prompt` sends the task as a single text content
   block. While the turn runs:
   - `session/update` notifications carrying `AgentMessageChunk` feed the
     turn's closure (see below) and stream through `chunk_tx`;
   - `session/request_permission` requests go to the pane's permission box
     in an interactive mode; without a channel (Yolo, manager, `call`) they
     are auto-approved (AllowAlways, then AllowOnce, then the first option),
     and with no options the request is answered `Cancelled` per the spec;
   - `fs/read_text_file` / `fs/write_text_file` requests operate on the real
     workspace files (1-based lines, absolute paths per the protocol).
6. **Complete.** The prompt response's stop reason is returned with the
   turn's REPORT - the sub-agent's LAST message (the text written after its
   final tool call), not the concatenation of every message it produced.
   Earlier narration between tool calls lives only in the TUI's live
   timeline; the market-standard contract (Claude Code's Task tool, Gemini
   CLI's `generalist`, `codex exec --output-last-message`) returns the
   final message alone, keeping the caller's context clean. A defensive
   fallback applies when the turn ends right after a tool call with no
   post-call text: the last completed message is returned instead of an
   empty report (the mechanism is `subagent::closure::TurnClosure`). There
   is NO time limit: a sub-agent may work for hours, and the turn ends
   when the agent ends it - or earlier only when the user stops it, which
   sends `session/cancel` and yields the spec-mandated `"cancelled"` stop
   reason with the partial output preserved. A harness that ignores
   `session/cancel` gets a bounded grace on a pane session: past it the turn
   fails, the pane drops the handle and the next dispatch opens a fresh
   session instead of waiting forever. In the one-shot `call` shape the
   stopped call is still awaited for its partial output.

---

## Errors

| Situation | Result |
|---|---|
| Unknown agent name | `Err("Unsupported agent …")` - before any spawn |
| ACP handshake/prompt failure, no output | `Err` naming the failing step |
| ACP turn failed, partial output | `Ok((partial_output, "error"))` - with a warning log |
| Turn completed | `Ok((output, stop_reason))` |
