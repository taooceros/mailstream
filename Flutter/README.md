# Mailstream Flutter UI (experiment)

A Flutter macOS front end for Mailstream. The Swift app stays the backend:
accounts, OAuth, sync, push, the cache, threading and AI tagging all run there.
This UI talks to it over the control socket
(`~/Library/Application Support/Mailstream/control.sock`), using the same JSON
commands as the `mailstream` CLI: `accounts.list`, `mailboxes`, `mail.list`,
`mail.search`, `mail.show`, `mail.read`/`star`/`archive`/`trash`, `mail.tag` and
`mail.reply`. Mailstream must be running.

```sh
cd Flutter
flutter run -d macos                    # debug, hot reload
flutter build macos --release           # build/macos/Build/Products/Release/Mailstream Flutter.app
```

The app is not sandboxed, because it has to reach the socket in `~/Library`.

| Path | Role |
| --- | --- |
| `lib/backend/control_client.dart` | One JSON request per socket connection; large answers decode on another isolate |
| `lib/backend/models.dart` | Accounts, mailboxes, conversations and messages, as the socket returns them |
| `lib/state/mail_state.dart` | `MailState`: selection, lists served from memory for 90 s, search, the reader, and optimistic actions that roll back on failure |
| `lib/ui/design.dart` | The native `Design` tokens: colors, type scale, radii, durations, house curve, list metrics |
| `lib/ui/shell.dart` | Window layout, toolbar, keys, toasts |
| `lib/ui/conversation_list.dart` | Fixed-height lazy list, Smart Inbox bundles, hover action pill |
| `lib/ui/reader.dart` | Reading column, message cards and chat log, folds, inline reply |
| `lib/ui/slide_layer.dart` | Reader and bundle slide over the list, which stays mounted; trackpad swipe back |

Keys: ↑/↓ or J/K move, Return or Space opens, Esc or ⌘[ goes back, E archives, ⌫
trashes, S stars, U toggles read, R replies (⇧R or A replies to all), ⌘F searches,
⌘R refreshes.

Limits, set by what the socket offers:

- Lists hold the newest 100 conversations; the protocol has no paging cursor.
- `mail.show` returns text bodies, so mail renders as text. The HTML renderer in
  `reader.dart` is used once `mail.show` accepts `html: true` and returns an
  `html` field.
- No Google Chat, compose window, attachments on send, contact photos or settings.
  Reply recipients shown in the composer are a preview; the app plans the real
  ones when it sends.
