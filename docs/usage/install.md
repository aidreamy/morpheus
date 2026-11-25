# Installation

Morpheus is shipped as a prebuilt binary, so no Rust toolchain is required.

## Requirements

- A terminal. Linux and Windows are tested; macOS has not been tested yet — if
  something fails, open an issue with as much detail as possible.
- `curl` for the Unix installer (PowerShell uses `Invoke-RestMethod`).
- 64-bit x64, or arm64 on Linux and macOS. Windows builds are x64 only.
- The voice build also needs an audio player, listed under
  [Voice output](#voice-output).

## Install

**Linux and macOS** (also works on Windows through Git Bash or WSL):

```sh
curl -fsSL https://raw.githubusercontent.com/aidreamy/morpheus/main/download.sh | bash
```

**Windows (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/aidreamy/morpheus/main/download.ps1 | iex
```

The `curl | bash` form does not work in PowerShell or CMD, because Windows has
no native `bash`; use the PowerShell command there instead.

Both installers place the binary in a directory and print the command to add it
to `PATH` when that directory is not on it yet. The defaults are
`~/.local/bin` on Unix and `%USERPROFILE%\morpheus` on Windows; set
`MORPHEUS_BIN_DIR` to install somewhere else. The installer also drops
`morpheus-uninstall` next to the binary, and `morpheus-ttsd` in the voice build.

## Build variants

| Variant | Contents | Published for |
| --- | --- | --- |
| `slim` (default) | The agent and its tools. No local RAG and no Home screen; `morpheus` starts straight in a session. | Linux x64/arm64, macOS x64/arm64, Windows x64 |
| `slim-embed` | `slim` plus local retrieval (RAG) through fastembed. | Linux x64, Windows x64 |
| `slim-speak` | The complete voice build: local speech through Kokoro, with the `speak-phonemize` frontend so languages without a bundled lexicon — Brazilian Portuguese included — work. | Linux x64, macOS x64/arm64 |

Select a variant with `MORPHEUS_VARIANT` before running the installer:

```sh
export MORPHEUS_VARIANT=slim-speak
curl -fsSL https://raw.githubusercontent.com/aidreamy/morpheus/main/download.sh | bash
```

On PowerShell, set variables as `$env:MORPHEUS_VARIANT = "slim-speak"` first.

| Variable | Effect |
| --- | --- |
| `MORPHEUS_BIN_DIR` | Install directory. Defaults to `~/.local/bin`, or `%USERPROFILE%\morpheus` on Windows. |
| `MORPHEUS_VARIANT` | `slim`, `slim-embed` or `slim-speak`. Defaults to `slim`. |
| `MORPHEUS_VERSION` | Exact release to install, such as `v0.1.0` or `0.1.0`. Defaults to the latest release. |
| `INSTALL_OS` | Overrides OS detection: `linux`, `darwin` or `win32`. |

## First run

Run `morpheus` in the directory you want to work in. The session starts
immediately; there is no Home screen in the distributed build.

1. `/providers` adds a provider and stores its API key. A provider that is not
   in the built-in list is registered under `/settings` → Integrations →
   Custom providers (name, base URL, `openai-compatible` family).
2. `/models` chooses the provider, model and reasoning effort.
3. Start chatting. Tab switches the agent mode; see
   [Terminal UI](tui.md).

On the first launch a one-time box asks about usage telemetry. It is opt-out:
the answer is stored in `setup.json` and can be changed later in `/settings` →
General, and `MORPHEUS_TELEMETRY=off` wins over both. Telemetry is always off
in CI.

## Voice output

`slim-speak` uses a sibling binary, `morpheus-ttsd`, which Morpheus spawns; keep
both in the same directory. On Linux, playback needs one of `paplay`,
`pw-play`, `aplay`, `play` or `ffplay`; macOS uses the built-in `afplay`.

The speech model is not bundled. Download a Kokoro bundle from the
[sherpa-onnx model releases](https://github.com/k2-fsa/sherpa-onnx/releases/tag/tts-models)
and extract it under `~/.cache/morpheus/models/`, or point
`MORPHEUS_KOKORO_MODEL_DIR` at the extracted directory. Use v1.1 for the default
voices and v1.0 for the Brazilian Portuguese voice.

Voice is configured in `/settings` → Voice (auto-generate, speaker, speech
pace), or in `setup.json` → `voice`. `/voice` toggles automatic generation of
the final answer; a finished answer can also be spoken on demand with its
**Speak** button. Voice output is not available on Windows.

## Updating

```sh
morpheus update                # latest release
morpheus update v0.2.3         # a pinned release
morpheus update --yes          # unattended: scripts and containers
```

The update runs the official installer for the current OS and variant and
replaces the binaries next to the running `morpheus`; start Morpheus again when
it finishes. `MORPHEUS_BIN_DIR` targets another directory. When a newer release
exists, the TUI footer shows the version hint with an update action.

## Uninstalling

`morpheus-uninstall` ships next to the binary:

```sh
morpheus-uninstall --dry-run   # list what would be removed, remove nothing
morpheus-uninstall             # remove everything
```

It asks once, optionally, why you are uninstalling, then removes the Morpheus
binaries, the config, cache and data directories, and the provider credentials
stored in the OS keyring. Use `--dry-run` first when you want to keep the
sessions under the data directory.

## Where the files live

```sh
morpheus paths
```

prints the directories Morpheus reads and writes, one per line:

| Key | Contents |
| --- | --- |
| `data` | Sessions, usage cache and the telemetry queue |
| `config` | `setup.json` and its lock |
| `logs` | `stdout.log` and `laya.log` |
| `sessions` | The session histories of the current directory |
| `binary` | The running executable |

`MORPHEUS_DATA_DIR` and `MORPHEUS_CONFIG_DIR` override the first two.
