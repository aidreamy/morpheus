# Command line

```text
morpheus                             start the TUI
morpheus -c | --continue             start in the most recently active session here
morpheus -s <id> | --session <id>    start in one specific session here
morpheus update [version] [-y]       install the latest (or a pinned) release
morpheus paths                       print the storage directories
morpheus whatsapp                    answer WhatsApp messages with the agent
morpheus -h | --help [command]       show help, or one command's page
morpheus -v | --version              print the version
morpheus --check-keyring             report API-key storage status
```

`morpheus help <command>` prints the same page as `morpheus <command> --help`,
in the theme of the current configuration.

## Starting a session

| Flag | Meaning |
| --- | --- |
| `-c`, `--continue` | Resume the most recently active session of the current directory. |
| `-s <id>`, `--session <id>` | Resume that specific session. `--session=<id>` is accepted too. |

The two are mutually exclusive. Sessions are stored per directory, so `-c` and
`-s` only see sessions started in the directory you run the command from. The
session store itself lives in the `sessions` entry printed by
`morpheus paths`.

Running `morpheus` without flags starts a new session.

## Commands

### `morpheus update [version] [--yes]`

Installs a release through the official installer for the current OS and
variant, replacing the binaries next to the running `morpheus`. With no version
it targets the latest GitHub release; `version` accepts `v0.2.3` or `0.2.3`.

`--yes` / `-y` skips the confirmation prompt and is required when there is no
terminal, as in scripts and containers. Start Morpheus again after the update
finishes. `MORPHEUS_BIN_DIR` installs into a different directory.

```sh
morpheus update
morpheus update v0.2.3
morpheus update --yes
```

### `morpheus paths`

Prints the directories Morpheus reads and writes, one per line, so other tools
can consume them: `data`, `config`, `logs`, `sessions` and `binary`. See
[Installation](install.md#where-the-files-live).

### `morpheus whatsapp`

Links this machine to a personal WhatsApp account as a companion device and
answers the allowed chats with the agent. No TUI is opened: the pairing QR code
is printed, then the terminal only reports connection events while messages are
handled in the background. See [WhatsApp](whatsapp.md).

### `morpheus help [command]`

Prints the top-level help, or the page of the named command (`update`, `paths`,
`whatsapp`, `help`).

## Other flags

| Flag | Meaning |
| --- | --- |
| `-h`, `--help` | Show help. `morpheus --help update` shows the update page. |
| `-v`, `--version` | Print the version and exit. |
| `--check-keyring` | Report whether API-key storage in the OS keyring is working. |

## Examples

```sh
morpheus                      # new session in this directory
morpheus -c                   # continue where you left off
morpheus -s 1788063765396     # reopen a specific session
morpheus update               # install the latest release
morpheus whatsapp             # chat with the agent from WhatsApp
```
