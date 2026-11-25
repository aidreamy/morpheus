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
| `humanize` | `false` | Default simulated typing for chats with no `/human` choice. |
| `contacts` | `[]` | One-to-one contacts allowed to use the agent. |
| `groups` | `[]` | Groups allowed to use the agent. |

`self_chat` is independent of both lists: adding or removing a contact or a
group never changes it, so allowing someone new cannot lock the account out of
its own chat.

Each entry is `{ "id": …, "enabled": … }`. Disabling keeps the entry in the file
but grants nothing; removing deletes it. The same knobs are editable in
`/settings` → WhatsApp: `self_chat` and `humanize` are switches flipped in
place, and each list shows toggle rows — Enter flips an entry, the 🗑 glyph
removes it, and the **+ Add** row appends comma-separated entries.

The self-chat is never paced: a typing state in a chat with yourself has
nothing to simulate, so `humanize` and `/human` apply only to replies in other
chats, and `/human` in the self-chat says so instead of saving a choice.

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
| `/agent [ls \| ls all \| off \| reset <name> \| model <name> [<model>] \| <name>[,<name>…]]` | Select external ACP agents, reset one session, read or set a model. |
| `/human [on \| off]` | Turn simulated typing on or off for this chat (off by default; not used in the self-chat). |
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

`/agent ls` shows each selected instance with its model (`model <name>` or
`default model`) and whether it already has an ACP session (`session saved`) or
will start a new one (`new session`). `/agent model <name>` reports the same
pick without changing it.

Mode, selected agents, their ACP sessions, the `/human` setting and the main
agent's context are persisted per chat: reconnecting to WhatsApp resumes each
conversation where it stopped.

## Humanized replies

`/human on` makes the bot show WhatsApp's “typing…” state for about one to 2.5
seconds before each reply, scaled by the reply length. It is a pacing effect
only: the answer, its model and its text do not change, and the wait is bounded
so one `composing` stanza covers it. Commands (`/help`, `/mode`, `/agent`,
`/human`, `/stop`) are never delayed, `@all` shows a single window for the whole
batch, and a failed presence call never blocks or loses a reply. The setting is
per chat: `/human` records a choice for one chat, while the **Humanized
replies** switch in `/settings` → WhatsApp is the default a chat inherits
until it chooses (off by default).

Some clients do not render presence for groups or the self chat; the message is
still delivered normally.

## Notes

- TUI-only commands (themes, spinner, layout) are not available over WhatsApp.
- The agent works in the directory where `morpheus whatsapp` was started, the
  same way the TUI does.
- To revoke access, remove the linked device on the phone, or stop the process;
  to revoke a single chat, remove it from the allowlist.
