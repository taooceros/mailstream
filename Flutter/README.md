# Mailstream Flutter UI (experiment)

A Flutter macOS front end for Mailstream, in Material Design 3. The Swift app stays the backend:
accounts, OAuth, sync, push, the cache, threading and AI tagging all run there.
This UI talks to it over the control socket
(`~/Library/Application Support/Mailstream/control.sock`), using the same JSON
commands as the `mailstream` CLI: `accounts.list`, `mailboxes`, `mail.list` (paged by
its `next` cursor), `mail.previews`, `mail.avatars` (sender pictures from the app's avatar
cache), `mail.search`, `mail.show` (with `html: true`), `mail.read`/`star`/`archive`/`trash`,
`mail.tag` and `mail.reply`. Mailstream must be running.

```sh
cd Flutter
flutter run -d macos                    # debug, hot reload
flutter build macos --release           # build/macos/Build/Products/Release/Mailstream Flutter.app
```

The app is not sandboxed, because it has to reach the socket in `~/Library`.

The layout follows Material's large-screen guidance: a navigation drawer from 1200 dp wide
(a navigation rail below, whose menu button opens the drawer), and from 1000 dp the
list-detail layout, list and conversation in two rounded panes side by side. Narrower
windows show one pane; the conversation slides over the list.

| Path | Role |
| --- | --- |
| `lib/backend/control_client.dart` | One JSON request per socket connection; large answers decode on another isolate |
| `lib/backend/models.dart` | Accounts, mailboxes, conversations and messages, as the socket returns them |
| `lib/state/mail_state.dart` | `MailState`: selection, paged lists served from memory for 90 s, previews, search, the reader, and optimistic actions that roll back on failure |
| `lib/ui/design.dart` | `MailTheme` (Material 3 from one seed color), motion, list and pane metrics, stable account/topic/avatar colors |
| `lib/ui/shell.dart` | Adaptive layout, panes, search bar, keys, snackbars |
| `lib/ui/sidebar.dart` | Navigation drawer and rail |
| `lib/ui/conversation_list.dart` | Fixed-height lazy list that loads the next page near its end, Smart Inbox bundles, hover actions |
| `lib/ui/reader.dart` | Reading column, message cards and chat log, HTML bodies, folds, inline reply |
| `lib/ui/slide_layer.dart` | Bundles (and, in one pane, the reader) slide over the list, which stays mounted; trackpad swipe back |

Keys: ↑/↓ or J/K move (beside an open conversation they open the next one), Return or Space
opens, Esc or ⌘[ goes back, E archives, ⌫ trashes, S stars, U toggles read, R replies (⇧R or
A replies to all), ⌘F searches, ⌘R refreshes.

Not here: Google Chat, a compose window, attachments on send, contact photos and settings.
Reply recipients shown in the composer are a preview; the app plans the real ones when it
sends.
