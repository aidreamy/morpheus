# Terminal UI

`morpheus` starts the interactive session. Type a prompt and press Enter; the
agent works with its tools and streams the answer back into the transcript.
Everything configurable from the UI is written to `setup.json`, so a choice made
once survives restarts and new sessions.

## Modes

The mode decides what the agent may do. **Tab** cycles it, and the choice is
saved for new sessions.

| Mode | Behavior |
| --- | --- |
| **Build** | The working mode: full tool set, file writes allowed. |
| **Ask** | Read and reason only; no modification. |
| **Yolo** | Same as Build, but permission prompts are auto-approved. |
| **Command** | The prompt is a plain shell: input goes straight to `bash_run`, no model call is made. Command and output are kept in the transcript but hidden from the model. |
| **Manager** | Coordination mode: every tool except the file writers, and `bash_run` refuses commands that write to the filesystem. Meant for delegating work to sub-agents and `/agents` panes. |

The cycle is Build → Ask → Yolo → Command → Build. While the session has
`/agents` panes, Command is replaced by Manager, because the shared prompt is
already routed to agents in that state.

## Slash commands

Type `/` to open the command menu and keep typing to filter it.

| Command | What it does |
| --- | --- |
| `/models` | Provider, model and reasoning effort. |
| `/providers` | Add a provider and enter its API key. |
| `/router` | Fallback chain and the fallback prompt corrector. |
| `/settings` | Every persistent setting, grouped by section. |
| `/tools` | Enable or disable internal tools. |
| `/toolcall` | How tool calls are sent: native structured calls, or inline JSON text. |
| `/agents` | Split the chat into agent panes. |
| `/worktree` | Bind the current agent to a git worktree: `add <branch>`, `add <path> <branch>`, `list`, `exit`. |
| `/compact` | Compact the context now instead of waiting for the automatic threshold. |
| `/export` | Export the transcript to Markdown. |
| `/undo` | Undo the last revert: reapply the change the rollback removed. |
| `/new` | Start a new session. |
| `/rename` | Rename the current session. |
| `/themes` | Change the theme. |
| `/background` | Use the terminal's own background instead of the theme's. |
| `/wallpaper` | ASCII wallpaper: `default` (embedded art), an image path with an optional `x,y` cell offset, or `off`. |
| `/spinner` | Pick the agent-loop spinner animation. |
| `/bell` | Ring the terminal bell when the loop finishes. |
| `/voice` | Toggle automatic speech generation (voice builds only). |

With an `/agents` split open, the focused pane serves its own command list (the
external agent's native commands); the Morpheus commands above apply to the main
agent and to app-wide state.

## Keyboard shortcuts

| Key | Action |
| --- | --- |
| `Enter` | Send the prompt. |
| `Shift`/`Ctrl`/`Alt` + `Enter` | Insert a newline instead of sending. |
| `Tab` | Cycle the agent mode. |
| `Esc` | Interrupt the running turn; in a `/agents` split it parks the split first. |
| `PageUp` / `PageDown` | Scroll the transcript. |
| `Ctrl+B` | Toggle the sidebar. |
| `Ctrl+P` | Toggle the right panel. |
| `Ctrl+U` | Usage dashboard. |
| `Ctrl+S` | Session history. |
| `Ctrl+F` | File explorer; opens the selected file in the configured editor. |
| `Ctrl+K` | Help. |
| `Ctrl+R` | Cycle the model's reasoning effort. |
| `Ctrl+T` | Toggle thinking blocks. |
| `Ctrl+D` | Toggle tool-call details. |
| `Ctrl+E` | Toggle diagnostics blocks. |
| `Ctrl+G` | Toggle raw output of generic tools. |
| `Ctrl+C` | Conceal mode: masks answer text with blocks and dims the transcript, for screen sharing. |
| `F1`–`F4` | Maximize the sub-agent, bash, TODO and mixed right-panel sections. |
| `Ctrl+Up` / `Ctrl+Down` | Previous / next prompt from the input history. |

## Providers and models

Morpheus stores API keys in the OS keyring and in provider-specific environment
variables, never in `setup.json`.

1. `/providers` adds a provider and asks for its API key.
2. A provider the built-in list does not cover is registered in `/settings` →
   Integrations → Custom providers: a name, the base URL and the
   `openai-compatible` family. It then appears in `/models` and `/providers`.
3. `/models` picks the provider, model and reasoning effort. `Ctrl+R` cycles the
   reasoning effort from the prompt.
4. `/router` edits the fallback chain used when the active model cannot answer,
   plus models or ACP agents used to correct a prompt.

Run `morpheus --check-keyring` when key storage seems broken. Every provider's
key can be re-entered from `/providers`.

## Sessions

- `/new` starts a fresh session; `/rename` names the current one.
- `Ctrl+S` lists the sessions of this directory.
- `morpheus -c` / `morpheus -s <id>` reopen one from the command line.
- `/export` writes the transcript to Markdown.
- File edits go to the working tree as they happen; `/undo` undoes the last
  revert, putting the removed change back.

Sessions are stored per working directory, so the list only shows sessions
started where you are now.

## Multi-agent panes

`/agents` opens a picker of installed external agents (Claude, Gemini, Codex,
Cline, ACP harnesses and Morpheus itself) with an instance count per agent.
Confirmed panes are independent: each keeps its own session, model and
optionally its own git worktree, and the main agent coordinates them from the
shared prompt.

| Key | Action |
| --- | --- |
| `Alt+1` … `Alt+9` | Focus pane 1–9 (picker order). |
| `Alt+0` | Unfocus every pane: the shared prompt goes to the main agent. |
| `Ctrl+O` | Zoom the focused pane to the whole session area, and back. |
| `Esc` | Park the split. Turns keep running in the background; re-entering `/agents` brings the same panes back. |

A message typed while a pane is focused goes to that pane. With no pane focused
the main agent answers, which is what Manager mode is for: it plans with the TODO
tool and delegates the file work to the panes.

Register a custom harness in `/settings` → Integrations → ACP agents as
`name` + `command args`; it then appears in the picker. `/worktree` isolates one
agent in its own checkout when two of them would otherwise fight over the same
files.

## Context

- Long sessions compact automatically; `/compact` forces it now.
- `/settings` → Automation → Summarization models chooses the model chain used
  for summarization (empty uses the active model).
- `/settings` → Models & caching controls prompt-cache retention for Anthropic
  and OpenAI.

## Appearance and voice

- `/themes`, `/background`, `/wallpaper`, `/spinner` and `/bell` are direct
  toggles; every one of them is persisted.
- `/settings` → Voice (voice builds) sets auto-generation, speaker and speech
  pace. The **Speak** button on a finished answer plays it on demand even when
  auto-generation is off.
- `Ctrl+C` conceal mode is session-only: it never changes the stored settings.

## Settings

`/settings` groups every persistent option:

| Section | Contains |
| --- | --- |
| Automation | Summarization models, PreToolUse and PostToolUse hooks, termination checkup (ONNX builds). |
| Voice | Auto-generation, speaker, speech pace (voice builds). |
| Models & caching | Anthropic and OpenAI cache retention, effective context budget. |
| Environment | Editor, skill directories, language servers. |
| Integrations | MCP servers, ACP agents, custom providers. |
| WhatsApp | Self-chat, allowed contacts, allowed groups, humanized replies. |
| General | Telemetry. |

Navigate with the arrow keys, press Enter to edit and Esc to go back. Every
change is written to `setup.json`; the full file reference is in
[Setup reference](setup.md).
