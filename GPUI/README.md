# Mailstream GPUI front end (experiment)

A [GPUI](https://gpui.rs) (Zed's GPU UI framework, crate `gpui` 0.2) window for Mailstream. The Swift
app stays the backend: accounts, OAuth, sync, push, the Turso cache, threading and AI tagging all
run there. This UI talks to it over the control socket
(`~/Library/Application Support/Mailstream/control.sock`) with the same JSON commands as the
`mailstream` CLI: `accounts.list`, `mailboxes`, `mail.list`, `mail.search`, `mail.show`,
`mail.read`/`star`/`archive`/`trash` and `refresh`; `mail.previews` and `mail.list --cursor` paging
are used when the running app offers them. Mailstream must be running.

```sh
cd GPUI
cargo run            # debug build; the first one compiles gpui (~5 min)
cargo test           # subject splitting, the house curve
```

| Path | Role |
| --- | --- |
| `src/control.rs` | One JSON request per socket connection, blocking; always called on the background executor |
| `src/model.rs` | Accounts, mailboxes, conversations and messages as the socket returns them; display strings derived once per answer, off the main thread |
| `src/state.rs` | `MailApp`: selection, lists served from memory for 90 s (polled when stale), paging, search, layers over the list, threads, optimistic actions that roll back on failure, key bindings |
| `src/ui.rs` | Sidebar, toolbar, the lazy conversation list (`uniform_list`, fixed 38 pt rows), Smart Inbox bundles, the reader |
| `src/design.rs` | The native `Design` tokens: both palettes, type scale, radii, durations, house curve, list metrics, avatar/topic colors |
| `src/input.rs` | Single-line text field (IME, selection, clipboard) for search, adapted from gpui's input example |

Keys: ↑/↓ or J/K move (in the reader they page through conversations), Return or Space opens,
Esc or ⌘[ goes back, E archives, ⌫ trashes, S stars, U toggles read, / or ⌘F searches, ⌘R refreshes.

Layers: a bundle and the reader slide over the list, which stays mounted beneath them, so going
back never reloads it. Opening a conversation shows its header at once; bodies arrive after.

Limits, set by what the socket offers or not yet built:
- `mail.show` bodies render as text; HTML mail is shown through its text part.
- No reply or compose, Google Chat, contact photos or settings.
- Deleting from the Trash for good is left to the app (the socket asks for `--yes`).
