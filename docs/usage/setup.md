# Setup reference

This is the complete reference for Morpheus's `setup.json`. It is intentionally
short: use the TUI first, and edit the file directly only when you need a
precise or advanced configuration.

## Start in the TUI

The distributed **slim** Morpheus starts directly in a session. It has no Home
screen. Type these commands in the chat input and press Enter:

| Command | Opens |
|---|---|
| `/settings` | All persistent settings, grouped by section |
| `/models` | Provider, model, and reasoning selection |
| `/router` | Model fallback and prompt-correction chains |
| `/download` | Local text-to-speech, decision, and embedding models |
| `/providers` | Local provider endpoints |
| `/tools` | Enable or disable internal tools |
| `/themes`, `/background`, `/bell`, `/anim`, `/toolcall` | Direct toggles |

Changes made in the TUI are saved to `setup.json`. Navigate with the arrow
keys, press Enter to edit, and Esc to go back.

## Where the file lives

On Unix, the default path is `~/.config/morpheus/setup.json`. On other platforms,
Morpheus uses the operating system's application configuration directory. Set
`MORPHEUS_CONFIG_DIR` to use another directory; Morpheus then reads and writes
`<that-directory>/setup.json`.

The file is JSON. Missing fields use their defaults. A missing or invalid file
also starts with defaults. `MORPHEUS_DATA_DIR` changes the data directory for
sessions and caches; it does not change the location of `setup.json`.

## Quick map

| JSON section | Main TUI location | Purpose |
|---|---|---|
| `appearance` | `/themes`, `/background`, `/bell`, `/anim` | Theme and visual behavior |
| `tools` | `/toolcall`, `/tools` | Tool-call format and disabled tools |
| `routing` | `/router`; `/settings` → Automation | Fallback and summarization chains |
| `hooks` | `/settings` → Automation | Commands before/after tool calls |
| `providers` | `/providers` | Local model-server URLs |
| `model` | `/models` | Last selected provider/model |
| `mode` | Session mode cycle | Last selected agent mode |
| `cache` | `/settings` → Models & caching | Prompt-cache retention |
| `skills` | `/settings` → Environment | Skill discovery |
| `model_decision` | `/settings` → Automation, ONNX builds only | Local termination decision model |
| `voice` | `/settings` → Voice, `speak` builds only | Local text-to-speech speaker, language and pace |
| `lsp` | `/settings` → Environment | Language-server integration |
| `mcp` | `/settings` → Integrations | External MCP servers |
| `editor` | `/settings` → Environment | Editor opened by the file explorer |
| `telemetry` | `/settings` → General | Anonymous usage telemetry |
| `whatsapp` | `/settings` → WhatsApp | Who may use the agent over `morpheus whatsapp` |

## `appearance`

| Field | Default | What it controls |
|---|---:|---|
| `theme` | `""` | Theme name. Empty uses the registry default. Use `/themes`. |
| `bell_enabled` | `true` | Rings the terminal bell when the agent loop finishes. |
| `transparent_background` | `false` | Uses the terminal's own background color for the TUI base. Panels keep their theme colors. |
| `anim_enabled` | `false` | Shows the animated chat logo on an empty session. |

The TUI commands `/background`, `/bell`, and `/anim` edit the corresponding
boolean fields.

## `tools`

| Field | Default | What it controls |
|---|---:|---|
| `tool_call_mode` | `"native"` | `native` sends structured tool calls; `inline` sends JSON text that Morpheus parses. Change with `/toolcall`. |
| `disabled` | `[]` | Tool names hidden from the model. Manage them with `/tools` → Internal Tools. |

## `routing`

All chains are ordered: Morpheus tries the first entry, then the next one.

| Field | Default | What it controls |
|---|---:|---|
| `fallbacks` | built-in chain | Provider/model fallback when the active model cannot answer. Edit in `/router`. |
| `fallback_prompt_corrector` | `[]` | Ordered models or ACP agents used to correct a prompt. Edit in `/router` → Fallback Prompt Corrector. |
| `summarization_models` | `[]` | Provider/model chain for context summarization. Empty uses the active agent model. Edit in `/settings` → Automation → Summarization models. |

When `fallbacks` is empty or missing, the built-in order is:

1. `nvidia` → `deepseek-ai/deepseek-v4-pro`
2. `openrouter` → `deepseek/deepseek-v4-pro`
3. `groq` → `openai/gpt-oss-120b`
4. `charm` → `deepseek-ai/deepseek-v4-pro`

A model fallback entry is:

```json
{ "provider": "openrouter", "model": "deepseek/deepseek-v4-pro" }
```

Prompt-corrector entries are tagged objects:

```json
[
  { "kind": "model", "provider": "openrouter", "model": "..." },
  { "kind": "acp", "agent": "my-agent" }
]
```

## `hooks`

Use `/settings` → Automation → **PreToolUse hooks** or **PostToolUse hooks**.
The detailed lifecycle and exit-code behavior is documented in
[hooks.md](hooks.md).

| Field | Default | What it controls |
|---|---:|---|
| `pre_tool_use_enabled` | `true` | Enables hooks before a tool call. |
| `post_tool_use_enabled` | `true` | Enables hooks after a successful tool call. |
| `PreToolUse` | `[]` | Hook entries for the pre-call event. |
| `PostToolUse` | `[]` | Hook entries for the post-call event. |

Each event array contains objects with:

| Field | Default | What it controls |
|---|---:|---|
| `name` | `""` | Display name; an empty name displays the command. |
| `matcher` | `""` | Regex matched against the tool name; empty matches every tool. |
| `command` | required | Shell command to run. |
| `timeout` | `30` seconds | Maximum runtime. |

Example:

```json
{
  "hooks": {
    "pre_tool_use_enabled": true,
    "post_tool_use_enabled": true,
    "PreToolUse": [
      { "name": "protect files", "matcher": "^write_file$", "command": "./check-write.sh", "timeout": 10 }
    ],
    "PostToolUse": []
  }
}
```

## `providers`

`providers.local` maps a local provider name to its endpoint. Add it with
`/providers` → **ADD Provider**.

```json
{
  "providers": {
    "local": {
      "ollama": { "base_url": "http://localhost:11434" }
    }
  }
}
```

API keys for regular providers are not `setup.json` settings: Morpheus obtains
them from the OS keyring and/or provider-specific environment variables.

## `model` and `mode`

These sections persist the last choice for **new sessions**; they do not
rewrite existing session history.

| Field | Default | What it controls |
|---|---:|---|
| `model.provider` | `""` | Provider of the last selected model. |
| `model.model` | `""` | Model ID of the last selected model. |
| `model.reasoning` | `null` | Model-specific reasoning effort; `null` uses the model default. |
| `mode.mode` | `""` | Last agent mode: `build`, `ask`, `yolo`, or `command`. Empty/unknown uses `build`. |

Use `/models` for `model.*`. Cycle the agent mode with the session's mode
shortcut (Tab); the selected mode is saved automatically.

## `cache`

Configure these in `/settings` → Models & caching. The input accepts blank,
minutes, `45m`, `1h`, or `1h30m`, up to 24 hours. The stored values are wishes
in minutes; Morpheus maps them to the API values that each provider supports.

| Field | Default | What it controls |
|---|---:|---|
| `anthropic_ttl_min` | `0` | `0` uses Anthropic's 5-minute behavior; a value above 5 selects the 1-hour cache (higher write cost). |
| `openai_retention_min` | `0` | `0` uses the model/provider default; a value above 0 selects 24-hour retention. |

## `skills`

Configure the source list in `/settings` → Environment → Skill directories.
Enter multiple directories separated by `:`. An empty list uses `~/.skills`.
Each source contains one directory per skill, with a `SKILL.md` inside; the
first source wins when names collide. A path beginning with `~` is expanded.

| Field | Default | What it controls |
|---|---:|---|
| `dirs` | `[]` | Skill directories in priority order. The TUI edits this field. |
| `recursive` | `false` | Scans for skills at any depth instead of only direct children. |
| `ignore` | `[]` | Glob-style skill-name exclusions, such as `deprecated-*`. File only. |
| `include` | `[]` | Exact skill-name allowlist. Empty allows all. File only. |

`recursive`, `ignore`, and `include` currently have no TUI control; edit them
directly in `setup.json` when needed.

## `model_decision` (ONNX builds only)

The distributed slim binary has no Home screen and is built without the ONNX
feature, so these settings are not shown in its TUI and no local decision
model runs there. They remain part of the file schema for binaries built with
`--features onnx`. In those builds, use `/settings` → Automation.

| Field | Default | What it controls |
|---|---:|---|
| `termination.enabled` | `true` | Enables the local model's termination review. |
| `termination.min_confidence` | `0.75` | Confidence floor for a “not finished” verdict to continue the loop. Range `0..=1`. Measured optimum edge of the confidence-floor sweep over the loop-termination corpus: no legitimate ending is vetoed, the single corpus error is one missed stop. |
| `model` | `english` | Checkpoint kind: `english`, `multilingual`, or `typed_decisions`; the TUI displays the last one as `typed-decisions`. |
| `hub.repo` | `Zaluski/laya-onnx` | Model-weight mirror repository. |
| `hub.pinned_repo` | `""` | Repository to which the recorded revision belongs. |
| `hub.pinned_revision` | `""` | Revision pin. Empty resolves and records the first installed revision. |

Use **Decision model** to select the checkpoint used for the termination
checkup. Configure a custom checkpoint in `setup.json`.

```json
{
  "model_decision": {
    "model": { "kind": "custom", "repo": "/path/to/checkpoint", "subfolder": null }
  }
}
```

`MORPHEUS_ONNX_HUB_REPO` overrides `model_decision.hub.repo`. The legacy top-level
name `checkup` is still accepted when reading, but Morpheus writes
`model_decision`. The checkpoint itself is installed from `/download` →
**Models** → **Decision**. An uncached checkpoint disables the audit until it
is installed.

## `voice` (speak builds only)

Speaks the agent's final answer with a local Kokoro model. `/download` →
**Models** → **Text-to-speech** lists the downloadable bundles. The full speaker
and language reference is in [`docs/voice/README.md`](../voice/README.md).

```json
{
  "voice": {
    "auto_generate": true,
    "speaker": 42,
    "speed": 1.0,
    "language": "pt-br",
    "model_dir": "/home/me/.cache/morpheus/models/kokoro-multi-lang-v1_0"
  }
}
```

| Field | Default | What it controls |
|---|---:|---|
| `auto_generate` | `true` | Generates the final answer's audio at the end of every turn. It is never played automatically. `false` generates on demand (Speak / `Ctrl+Y`). Legacy alias: `enabled`. |
| `speaker` | `0` | Speaker id of the loaded bundle. Ids belong to one bundle: `42` is `pf_dora` (Portuguese) in v1.0 and `zf_075` (Chinese) in v1.1. Legacy alias: `voice`. |
| `speed` | `1.0` | Pace: `1.0` natural, larger faster, smaller slower. Invalid values fall back to `1.0`. |
| `language` | `""` | Voice language (`pt-br`, `es`, `ja`, …). Empty derives it from `speaker`; English and Chinese voices then use the bundle lexicon. A non-empty value wins, except when it is the voice's own language and the bundle has a lexicon for it (`en-us`, `en-gb`, `zh`): those always keep the lexicon. Non-lexicon languages are phonemized outside the engine: `ja` by the built-in Japanese frontend, the rest by espeak-ng. |
| `model_dir` | `""` | Kokoro bundle. Empty uses `MORPHEUS_KOKORO_MODEL_DIR` or the cache default, and falls back to another installed bundle. `~` is not expanded. |

English and Chinese voices need only the `speak` feature. Portuguese, Spanish,
French, Hindi, Italian and Japanese need `speak-phonemize` (espeak-ng) and the
v1.0 bundle, the only release that contains those voices. The daemon reads
this section at startup; after a manual edit, stop `morpheus-ttsd` so the next
synthesis reloads it.

## `lsp`, `mcp`, `editor`, and `telemetry`

| Setting | Default | TUI / behavior |
|---|---:|---|
| `lsp` | `true` | `/settings` → Environment → Language servers. `MORPHEUS_LSP=off`, `0`, or `false` disables it for the process. |
| `editor` | `""` | `/settings` → Environment → Editor. Empty auto-detects `nvim`, then `vim`, then `nano`; otherwise use a command such as `nvim -u NONE` or `code -w`. |
| `telemetry` | `true` | `/settings` → General → Telemetry. This is opt-out and applies on the next launch; `MORPHEUS_TELEMETRY=off` disables it. CI is always off. |

### `mcp`

Manage servers in `/settings` → Integrations → MCP servers. A server is
enabled by default; disabling it skips it at startup.

| Field | What it controls |
|---|---|
| `name` | Unique name shown in the TUI and used for runtime state. |
| `enabled` | Whether Morpheus connects to the server at startup. |
| `transport.type` | `stdio` for a local process or `http` for a remote server. |
| `stdio.command`, `args` | Executable and arguments for the local process. |
| `stdio.env`, `cwd` | Environment variables and optional working directory for that process. |
| `http.url` | Remote `http://` or `https://` endpoint. |
| `http.headers` | Static extra request headers; do not put secrets here. |
| `http.api_key_env` | Environment-variable name used for the HTTP API key. |
| `http.timeout_ms` | Per-request timeout in milliseconds; default `30000`. |

```json
{
  "mcp": {
    "servers": [
      {
        "name": "local-tools",
        "enabled": true,
        "transport": {
          "type": "stdio",
          "command": "my-mcp-server",
          "args": ["--stdio"],
          "env": {},
          "cwd": null
        }
      },
      {
        "name": "remote-tools",
        "transport": {
          "type": "http",
          "url": "https://example.com/mcp",
          "headers": {},
          "api_key_env": "MCP_API_KEY",
          "timeout_ms": 30000
        },
        "enabled": true
      }
    ]
  }
}
```

In the TUI, enter a command and arguments for stdio, or an `http(s)://` URL
for HTTP. Timeout is entered in seconds; the file stores milliseconds and
defaults to `30000` (30 seconds). HTTP credentials entered as a literal key
are stored in the OS keyring, not in `setup.json`; entering `$VAR` stores the
environment-variable name in `api_key_env`. Stdio credentials belong in that
server's `transport.env`.

## `whatsapp`

Access control for `morpheus whatsapp`. The default is the most restrictive
policy: only the account's own **self-chat** ("Message yourself") reaches the
agent. No contact and no group is answered until it is listed here.

| Field | Default | What it controls |
|---|---:|---|
| `self_chat` | `true` | Answer messages the account sends to its own chat. |
| `contacts` | `[]` | One-to-one contacts allowed to use the agent. |
| `groups` | `[]` | Groups allowed to use the agent. |

```json
{
  "whatsapp": {
    "self_chat": true,
    "contacts": [{ "id": "5511912345678", "enabled": true }],
    "groups": [
      { "id": "120363012345678901@g.us", "enabled": true },
      { "id": "120363099999999999@g.us", "enabled": false }
    ]
  }
}
```

Each entry is `{ "id": …, "enabled": … }`. An entry with `enabled: false`
stays in the file but grants nothing — disabling is separate from removing.
Older files that list bare strings (the pre-toggle shape) still load, with
every string treated as enabled.

The same three knobs are editable from `/settings` → WhatsApp. Each list
shows its configured entries as toggle rows: Enter flips the entry on or
off, the 🗑 glyph removes it, and the "+ Add" row opens an empty box where
comma-separated entries are appended as enabled items. The box is cleared
after each confirmation, so the list is the only view of what is configured
and a confirm never overwrites it.

Each `contacts` id is a phone number in international format or an account
id (`@lid` included); the `+`, spaces, punctuation and `@server` suffix are
ignored, so `+55 (11) 91234-5678` and `5511912345678` are the same contact.
WhatsApp routes some direct chats by LID instead of phone number; those are
matched against the phone number WhatsApp reports for the chat, so an entry
written as a number still authorizes its LID-routed conversation.
Each `groups` id is the group id (`120363…@g.us`) or just its digits. An
empty list (or one whose entries are all disabled) denies that origin kind
entirely.

A message from an origin that is not allowed is logged and never answered:
the sender gets no reply, and the agent never starts a turn for that chat.

## Environment values that affect setup

These are convenience inputs, not additional JSON fields:

| Variable | Affects |
|---|---|
| `MORPHEUS_CONFIG_DIR` | Directory containing `setup.json`. |
| `MORPHEUS_DATA_DIR` | Session and data storage directory. |
| `MORPHEUS_PROVIDER`, `MORPHEUS_MODEL`, `MORPHEUS_REASONING` | Initial model defaults before a persisted `/models` choice. |
| `MORPHEUS_TOOL_CALL_MODE` | Initial tool-call mode before the persisted setting is applied. |
| `MORPHEUS_LSP` | Runtime LSP override. |
| `MORPHEUS_TELEMETRY` | Runtime telemetry override. |
| `MORPHEUS_ONNX_HUB_REPO` | ONNX mirror override in ONNX builds. |
| `MORPHEUS_KOKORO_MODEL_DIR` | Kokoro bundle used when `voice.model_dir` is empty. |
| `MORPHEUS_TTSD_IDLE_SECS` | Idle seconds before the voice daemon exits (default `300`). |

For normal use, configure through the TUI and let Morpheus write the file. Edit
`setup.json` directly for `skills` filters, custom decision checkpoints,
advanced routing, or automation that must be reproducible.
