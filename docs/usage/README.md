# Morpheus usage

Morpheus is a general-purpose coding agent for the terminal. It ships software
engineering tools (files, shell, search, web, LSP, plans, sub-agents) and
*computer tools* that drive graphical applications through the accessibility
tree, falling back to screenshots only when needed.

This book documents **how to use Morpheus**. It covers every way to run the
agent, every command it exposes and every setting that is worth changing by
hand. Implementation notes for the crates are developer material and are not
part of this book.

## Quick start

Linux, macOS and Windows via Git Bash or WSL:

```sh
curl -fsSL https://raw.githubusercontent.com/aidreamy/morpheus/main/download.sh | bash
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/aidreamy/morpheus/main/download.ps1 | iex
```

Then run `morpheus` in a terminal, pick a model with `/models`, and start
typing. If your provider is not wired up yet, `/providers` registers it and
stores the API key; a provider the built-in list does not cover is added under
`/settings` → Integrations → Custom providers.

## Ways to use Morpheus

| Surface | Start it | What it is for |
| --- | --- | --- |
| Terminal UI | `morpheus` | The main experience: chat, tools, sessions, `/agents` split panes, settings. |
| WhatsApp | `morpheus whatsapp` | Talk to the agent from a phone. The terminal only links the device and reports events; messages are answered in the background. |
| Command line | `morpheus <command>` | Start or resume a session, update, print storage paths, show help. |
| Hooks | `setup.json` → `hooks` | Shell commands around every tool call, to block, allow, rewrite or annotate. |

Morpheus is also scriptable through the environment variables listed in
[Setup reference](setup.md), but the TUI is the intended way to configure it.

## Guides

- [Installation](install.md): installs, build variants, updates, removal, and
  where Morpheus keeps its files.
- [Command line](cli.md): every flag and command, with examples.
- [Terminal UI](tui.md): modes, slash commands, keyboard shortcuts,
  multi-agent panes, sessions, context management and appearance.
- [WhatsApp](whatsapp.md): pairing, who may talk to the agent, and the commands
  available from a chat.
- [Lifecycle hooks](hooks.md): run shell commands before and after tool calls.
- [Setup reference](setup.md): every `setup.json` section and field.
