# Native OpenCode transport

OpenCode speaks ACP, and the generic client in [`acp`](acp.md) used to drive
it like any other harness. OpenCode also ships `opencode serve`: a headless
HTTP server with a published OpenAPI spec, server-sent events for live turns
and REST endpoints for the whole session model. Morpheus now uses that
server directly for OpenCode, which is why an OpenCode pane shows real tool
names and diffs, its todo list as a plan, token usage and its provider/model
catalog instead of the generic protocol translation.

Nothing else changes: the TUI, the WhatsApp bridge and the `subagent_call`
tool still call `acp::call`, `acp::open_session`, `acp::session_snapshot`
and `acp::replay`. Those functions route an OpenCode request to this backend
when the CLI is installed and take the ACP path for every other agent.

## The server lifecycle

`opencode::server::Server` owns one child process:

1. reserve a free loopback port,
2. spawn `opencode serve --hostname 127.0.0.1 --port <port>` in the
   workspace (that is the project root the server uses),
3. poll `GET /global/health` until it answers (30 s budget, stderr tail in
   the error when the process dies early),
4. kill and reap the whole process tree when the last handle drops (on
   Windows the CLI is an npm shim, so killing only the wrapper would leave
   the server listening).

If `OPENCODE_SERVER_PASSWORD` is set, every request carries HTTP basic auth
(username `OPENCODE_SERVER_USERNAME`, default `opencode`). In a terminal
the server is usually started once and reused; a one-shot `subagent_call`
spawns it for that turn and shuts it down afterwards.

## One turn

```
GET  /session/:id/todo            (seed the plan of a resumed session)
GET  /event                       (subscribe first: no missed parts)
POST /session/:id/prompt_async    (204; the answer arrives as events)
  or POST /session/:id/command    (a `/name` the server advertises)
  └── message.part.updated        → Message / Thought / ToolCall(+Update)
  └── message.updated             → Usage (context window + tokens)
  └── todo.updated                → Plan
  └── permission.updated          → PermissionRequest, answered via
                                    POST /session/:id/permissions/:id
  └── session.status (retry)      → a retry note in the pane
  └── session.updated             → SessionInfo (title)
  └── session.idle                → end of turn
POST /session/:id/abort           (caller's stop signal → cancelled)
```

The report contract matches ACP's: only the assistant text written after the
last tool call is returned (see `closure`). Text parts arrive as growing
snapshots, sometimes with a `delta` hint; the mapper remembers how much of
each part it already emitted and forwards only the growth, so the pane and
the report never see duplicates.

### Commands

A pane slash command (`/init`, a project command, …) is dispatched to
`POST /session/:id/command` with the command name, its arguments, the bound
agent and model. Only names the server actually advertises (`GET /command`)
take that path: an unknown `/word` is ordinary text and reaches the model
verbatim. The endpoint answers only when the turn is over, so the POST runs
detached while the SSE stream feeds the pane.

### Diffs

OpenCode's edit/write tools publish `metadata.filediff` (path, patch,
additions, deletions) and `metadata.diff`. The mapper turns that into the
same `ToolDiffSummary` the ACP path builds, normalizing away the `Index:` /
`====` preamble so the panel receives the `--- ` header it renders. Both the
running and the completed updates carry it.

### Usage, retries and title

`message.updated` reports tokens (`input`, `output`, `reasoning`, cache) and
the provider/model that produced them. The context window comes from the
model catalog (`limit.context`); when the server declares one, the pane gets
the same `Usage` event the ACP harnesses send. A provider retry surfaces as a
short note in the timeline instead of a frozen pane, and `session.updated`
updates the pane title.

## Sessions

- Consecutive calls resume the most recent session by default
  (`continue_session`): the id is probed with `GET /session/:id`, and a
  vanished session silently opens a new one.
- A pane keeps one server and one session id between turns
  (`open_session`), through the same `SessionHandle::run_turn` the ACP
  supervisor implements.
- Only a turn that ended cleanly stores its id, exactly like the ACP path.
- A resumed turn first seeds its plan from `GET /session/:id/todo`, so the
  pane shows work planned in earlier turns.

## Snapshot and replay

`session_snapshot` reads the catalog over a throwaway server and, when a
session id is given, the mode and model that session currently runs with:

| Endpoint                                | Becomes                                        |
| --------------------------------------- | ---------------------------------------------- |
| `GET /command`                          | the pane's slash-command menu                  |
| `GET /agent`                            | the pane's modes (OpenCode primary agents)     |
| `GET /config/providers` + `GET /config` | the `model` config option and its values       |
| `GET /session/:id`                      | `current_mode` + the option's `current` value  |

`replay` rebuilds a pane's timeline from `GET /session/:id/message`, mapping
the retained messages and parts through the same event mapper the live
stream uses — including the user's side of the conversation, the session
title and the todo plan.

## What is intentionally not used

The server exposes editor-app endpoints with no counterpart in a delegated
sub-agent turn: projects/VCS, the file and symbol search APIs, PTYs, the TUI
control endpoints, MCP management, session sharing/fork/revert/summarize,
and the provider OAuth flow. Authentication is the one worth knowing about:
when a provider has no credentials the turn fails with the server's
`ProviderAuthError`, and the error message points at `opencode auth login`
instead of trying to drive an interactive OAuth flow from a sub-agent.

## Escape hatch

`MORPHEUS_OPENCODE_TRANSPORT=acp` forces the generic ACP transport, which is
useful when the server path is unavailable (an older CLI without `serve`, a
proxy that blocks loopback ports) or when comparing the two transports.

## Module layout

| Module     | Responsibility                                                        |
| ---------- | --------------------------------------------------------------------- |
| `api`      | Wire types of the documented endpoints (tolerant, all fields optional)  |
| `http`     | HTTP client plus the buffered SSE decoder                              |
| `server`   | Process lifecycle: port, spawn, health, auth, kill                     |
| `event`    | Server events → `SubagentEvent`, diffs, usage, permissions             |
| `turn`     | One prompt/command turn: subscribe, dispatch, permissions, stop        |
| `session`  | Persistent pane session (one server + one session id)                  |
| `snapshot` | Advertised surface, session state and history replay                   |

## Kilo

Kilo is a fork of OpenCode and keeps the same server protocol, so it reuses
this backend instead of a separate module. The operational details differ:
the `kilo` binary, `KILO_SERVER_PASSWORD` / `KILO_SERVER_USERNAME`
(default `kilo`), the `MORPHEUS_KILO_TRANSPORT` override and the
`x-kilo-directory` header on instance-scoped requests. Set
`MORPHEUS_KILO_TRANSPORT=acp` to restore the generic transport.
