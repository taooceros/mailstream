# Mailstream GPUI front end (experiment)

A [GPUI](https://gpui.rs) (Zed's GPU UI framework, crate `gpui` 0.2) window for Mailstream. The
engine (accounts, OAuth, sync, push, the Turso cache, threading, AI tagging, Google Chat) runs in
this process: the app links `libMailstreamKit.dylib` through the Rust crate in `../FFI` and calls
its typed async methods (`Mailstream::conversations`, `show`, `tag`, `chat_messages`, `reply`, …).
The engine's change events (`Accounts`, `Mailboxes`, `Conversations`, `Previews`, `Tags`) drive
reloads; nothing polls the mail list. The Mailstream app need not be running; when it is, both
share `mail.db` (see "Front ends in other languages" in the top-level README).

```sh
GPUI/run.sh                 # from the repo root: Turso, libMailstreamKit, cargo build, sign, launch
CONFIG=release GPUI/run.sh  # engine and window optimized: use this to judge smoothness
(cd GPUI && cargo test)
```

The engine's main-actor work (listing a mailbox, encoding answers) runs on the window's main
thread. In the debug engine a `mail.list` takes 5–18 ms there and drops frames while scrolling;
the release engine takes under 8 ms. The release Swift build takes about a minute per edit.

Always start it through `run.sh`. The engine reads the sign-ins from the Keychain item that macOS
pins to Mailstream's signature (identifier `dev.mailstream.Mailstream`, the Apple Development
team). `run.sh` signs the binary with `scripts/sign.sh` and refuses to start it unless
`codesign` shows that identifier and a team. A binary signed any other way (`cargo run` leaves an
ad hoc signature) makes macOS ask for the Keychain, and "Always Allow" there re-pins the item to
that one build, so the app and every other host get asked again.

| Path | Role |
| --- | --- |
| `build.rs` | Adds the rpaths to `libMailstreamKit.dylib`: `.build/<config>` (`DEP_MAILSTREAMKIT_DIR` from the `FFI` crate) for development, `@executable_path/../Frameworks` for an app bundle |
| `run.sh` | Builds the engine and the app, signs it, checks the signature, starts it |
| `src/engine.rs` | The one in-process `Mailstream` engine, started at launch |
| `src/model.rs` | Accounts, mailboxes, conversations and messages converted from the crate's records; display strings derived once per answer, off the main thread |
| `src/state.rs` | `MailApp`: selection, lists served from memory for 90 s and reloaded on change events, paging, search, previews, layers over the list, key bindings |
| `src/state/nav.rs`, `src/state/actions.rs` | Navigation (sources, bundles, the reader, refresh) and optimistic actions (archive, trash, restore, star, read, waiting, tags) that roll back on failure |
| `src/ui/` | Sidebar tree (`sidebar.rs`), toolbar, the lazy conversation list with Smart Inbox bundles (`list.rs`), the reader with its reply card (`reader.rs`), the context menu (`menu.rs`) |
| `src/webview.rs` | HTML bodies in native `WKWebView`s laid over the reader: placed and clipped each frame, sized to their page, page scripts off, links opened in the browser, wheel events handed back to the reader |
| `src/swipe.rs` | Two-finger back swipe over the bundle and reader layers, following the system's swipe-between-pages setting |
| `src/chat.rs` | `ChatPane`: Google Chat spaces, a space's log (bottom-aligned, runs and day dividers, markup and links), sending, earlier pages, threads, New Chat with contact completion; the open space is read again every 5 s |
| `src/compose.rs` | The New Message and Forward windows (From picker, recipient completion, discard confirmation) and the reader's reply card |
| `src/settings.rs` | Accounts (rename, sign in, sign out), AI tagging and re-tagging, engine status |
| `src/avatars.rs` | Contact photos from `mail.avatars`, cached in memory; initials otherwise |
| `src/design.rs` | The native `Design` tokens: both palettes, type scale, radii, durations, house curve, list metrics, avatar/topic colors |
| `src/icons.rs`, `src/format.rs` | Material Symbols glyphs; dates and sizes |
| `src/input.rs`, `src/textarea.rs` | Single-line field (IME, selection, clipboard) adapted from gpui's input example, and the multi-line editor for bodies |
| `src/bench.rs` | The frame measurement `scripts/bench.sh gpui` runs (`MAILSTREAM_BENCH=scroll`): the list scrolls itself and every frame's start and render-to-paint time is logged |

Keys: ↑/↓ or J/K move (in the reader they page through conversations), Return or Space opens,
Esc or ⌘[ goes back, E or ⌃⌘A archives, ⌫ trashes, S or ⇧⌘L stars, U or ⇧⌘U toggles read,
D or ⇧⌘D toggles waiting on you, R or ⌘R replies, A or ⇧⌘R replies to all, F or ⇧⌘F forwards,
⌘N writes a new message (a new chat in Chat), / or ⌘F searches, ⇧⌘N refreshes, ⌃⌘S toggles the
sidebar, ⌘, opens Settings. In a draft ⌘Return sends and Esc closes (asking first when something
was written). Right-click a conversation for the same commands.

Layers: a bundle and the reader slide over the list, which stays mounted beneath them, so going
back (Esc or a two-finger swipe) reveals the live list without reloading it. Opening a
conversation shows its header at once; bodies arrive after.

Limits:
- Deleting from the Trash for good is left to the app.
