# WhatsApp

`morpheus whatsapp` links this machine to a personal WhatsApp account as a
companion device and answers the allowed chats with the agent. No TUI is opened:
the pairing QR code is printed, and after pairing the terminal only reports
connection events while messages are handled in the background. Keep the process
running — it is the connection.

```sh
morpheus whatsapp
```

Scan the QR code with WhatsApp on the phone (Linked devices → Link a device).
The pairing lives in a device store under the Morpheus data directory — the
startup line `Device store: …` prints its path — so later runs reconnect without
a new code. If the stored session goes dead (unlinked from the phone), the
process clears it and prints a fresh QR code instead of exiting.

## Who may use the agent

Access is an allowlist in `setup.json` → `whatsapp`, with the most restrictive
default: only the account's own **self-chat** ("Message yourself") reaches the
agent. No contact and no group is answered until it is listed.

| Field | Default | Meaning |
| --- | --- | --- |
| `self_chat` | `true` | Answer messages the account sends to its own chat. |
| `contacts` | `[]` | One-to-one contacts allowed to use the agent. |
| `groups` | `[]` | Groups allowed to use the agent. |

Each entry is `{ "id": …, "enabled": … }`. Disabling keeps the entry in the file
but grants nothing; removing deletes it. The same three knobs are editable in
`/settings` → WhatsApp, where each list shows toggle rows: Enter flips an entry,
the 🗑 glyph removes it, and the **+ Add** row appends comma-separated entries.

A contact id is a phone number in international format or an account id
(`@lid` included); `+`, spaces, punctuation and the `@server` suffix are
ignored, so `+55 (11) 91234-5678` and `5511912345678` are the same contact.
A group id is the group id (`120363…@g.us`) or just its digits. An empty list —
or one whose entries are all disabled — denies that origin kind entirely.

A message from an origin that is not allowed is logged and never answered: the
sender gets no reply and no turn starts for that chat.

## Commands in a chat

Send these as messages; they are case-insensitive and must name a registered
command, so a path such as `/usr/bin` stays normal text.

| Command | Effect |
| --- | --- |
| `/help` | List the commands available over WhatsApp. |
| `/mode <mode>` | Switch between `build`, `ask`, `yolo`, `command` and `manager`. |
| `/agent [ls \| ls all \| off \| reset <name> \| model <name> <model> \| <name>[,<name>…]]` | Select external ACP agents, reset one session, or set its model. |
| `/stop` | Stop the turns running in this chat. |

Modes mean the same as in the TUI (see [Terminal UI](tui.md#modes)). There is no
approval dialog on WhatsApp, so in `build` and `ask` mode a tool that needs
permission is denied. Use `/mode yolo` to let those tools run.

## Talking to agents

A message without a prefix always goes to the main agent. Address an external
agent explicitly with `@name`, or `@name#2` for its second instance; `@all`
reaches every selected instance. External replies are labeled with the agent's
name, so an unlabeled reply is always the main agent's. An unknown `@name` is
kept as ordinary text.

Mode, selected agents, their ACP sessions and the main agent's context are
persisted per chat: reconnecting to WhatsApp resumes each conversation where it
stopped.

## Notes

- TUI-only commands (themes, spinner, layout) are not available over WhatsApp.
- The agent works in the directory where `morpheus whatsapp` was started, the
  same way the TUI does.
- To revoke access, remove the linked device on the phone, or stop the process;
  to revoke a single chat, remove it from the allowlist.
