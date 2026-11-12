# Mailstream GPUI front end

A [GPUI](https://gpui.rs) (Zed's GPU UI framework, crate `gpui` 0.2) window for Mailstream, on
macOS and Linux (Wayland or X11). The
engine (accounts, OAuth, sync, push, the Turso cache, threading, AI tagging, Google Chat) runs in
this process: the app links the Rust engine (`../Engine`) through the crate in `../FFI` and calls
its typed async methods (`Mailstream::conversations`, `show`, `tag`, `chat_messages`, `reply`, …).
The engine's change events (`Accounts`, `Mailboxes`, `Conversations`, `Previews`, `Tags`) drive
reloads; nothing polls the mail list. The Mailstream app need not be running; when it is, both
share `mail.db` (see "Front ends in other languages" in the top-level README).

```sh
GPUI/run.sh                 # from the repo root: cargo build, sign (macOS), launch
CONFIG=release GPUI/run.sh  # engine and window optimized: use this to judge smoothness
(cd GPUI && cargo test)
devenv shell -- GPUI/run.sh  # Linux (NixOS): the shell has the libraries GPUI links and loads
```

The engine works on its own threads; only its change events reach the window's main thread.

Always start it through `run.sh`. On macOS the engine reads the sign-ins from the Keychain item
that macOS pins to Mailstream's signature (identifier `dev.mailstream.Mailstream`, the Apple
Development team). `run.sh` signs the binary with `scripts/sign.sh` and refuses to start it unless
`codesign` shows that identifier and a team. A binary signed any other way (`cargo run` leaves an
ad hoc signature) makes macOS ask for the Keychain, and "Always Allow" there re-pins the item to
that one build, so the app and every other host get asked again. Linux keeps the sign-ins in a
file only the user reads (`~/.local/share/Mailstream`), so nothing is signed there.

Linux differs from the Mac in:
- HTML bodies: Blitz, the engine's renderer (`src/blitz_body.rs`, as in Flutter's reader), drawn
  by GPUI, instead of `WKWebView`s. The build turns on the FFI crate's `blitz` feature there, with
  two adjustments for GPUI's dependency graph: fontique takes fontconfig through `dlopen` as
  GPUI's font-kit does, and `vendor/stylo_derive` is Stylo's derive crate 0.22.0 with one error
  type spelled out (`to_css.rs`), which GPUI's `log` features make ambiguous. libc stays below
  0.2.190, which dropped the `ENOATTR` GPUI's tar reader (xattr 0.2) names.
- Shortcuts: ⌘ is Ctrl (`secondary-` bindings), except the Mac's text navigation (⌘←, Emacs keys).
- The window: the compositor draws its frame where it offers to; elsewhere dragging the window
  between the islands or their title rows moves it once the pointer has travelled a few points
  with the button held (a click or tap there never does, MS-206); rows and controls never move it.
  The type is the desktop's sans-serif family (fontconfig). The back swipe takes the scroll as
  the compositor turns it for the person's scrolling direction.
- Engine events reach the window through `Mailstream::subscribe_any_thread`: `subscribe` posts
  to GLib's main loop there, which GPUI doesn't run.

On the Mac `Mailstream GPUI.app` updates itself as Mailstream Flutter does (MS-219; the
Application updates section of `Flutter/README.md` has the details both share). `scripts/gpui-bundle.sh`
bundles the release binary with the Apple Intelligence tagger, Sparkle 2.10.1 (fetched once into
`build/` and checked against its pinned SHA-256) and an Info.plist naming the feed
`https://github.com/taooceros/mailstream/releases/download/updates/appcast-gpui.xml`, the
approved public key and the `prerelease` channel; `src/updater.rs` loads Sparkle at launch and adds
**Check for Updates…** and **Automatically Check for Updates** to the application menu. It checks
hourly, downloads and verifies in the background, and installs on quit. Outside a bundle (`run.sh`,
`cargo run`) there is no updater. Every push to `main` touching GPUI or the engine runs
`.github/workflows/publish-gpui-update.yml`: build, `gpui-bundle.sh` with the next build number
(`scripts/publish-update.py next-version gpui`), sign, `prepare-update.py`, then publish its own
feed and `Mailstream-GPUI-N.zip` archives in the shared `updates` release. A local
`scripts/install.sh gpui` builds number 1, so the next published build replaces it. Like
Flutter's, the bundle carries no OAuth clients or push settings: the engine reads them from
`config.json` in the support directory, which `install.sh` writes.

| Path | Role |
| --- | --- |
| `run.sh` | Builds the app with the engine; on macOS signs it and checks the signature; starts it |
| `src/engine.rs` | The one in-process `Mailstream` engine, started at launch |
| `src/model.rs` | Accounts, mailboxes, conversations and messages converted from the crate's records; display strings derived once per answer, off the main thread |
| `src/state.rs` | `MailApp`: selection, lists served from memory for 90 s and reloaded on change events, paging, search, previews, layers over the list, key bindings |
| `src/state/nav.rs`, `src/state/actions.rs` | Navigation (sources, bundles, the reader, refresh) and optimistic actions (archive, trash, restore, star, read, waiting, tags) that roll back on failure |
| `src/ui/` | The window as islands (`mod.rs`): the sidebar tree and the rail that stands in for it (`sidebar.rs`), the list's title row and the conversation's floating toolbar (`toolbar.rs`), the lazy conversation list of two-line rows with Smart Inbox bundles pinned at its top and its "Waiting on you" group (`list.rs`), the reader's message cards with the reply card (`reader.rs`), the context menu (`menu.rs`) |
| `src/controls.rs` | The shared controls every view composes (Flutter's `controls.dart`, `glass.dart`, `common.dart`): buttons, icon buttons and tooltips, capsules, islands, the title bars that move the window, popovers, tags, pills, marks, list rows and their lifted card, notices, toasts, switches |
| `src/webview.rs` | macOS: HTML bodies in native `WKWebView`s laid over the reader: placed and clipped each frame, sized to their page, page scripts off, links opened in the browser, wheel events handed back to the reader |
| `src/blitz_body.rs` | Linux: HTML bodies drawn by Blitz on the engine's pages thread, shown as tiles near the viewport; links open in the browser, a drag selects and Ctrl+C copies, inline parts go in as `data:` URLs |
| `src/swipe.rs` | Two-finger back swipe over the bundle and reader layers, following the system's swipe-between-pages setting |
| `src/chat.rs` | `ChatPane`: Google Chat spaces, a space's log (bottom-aligned, runs and day dividers, markup and links), sending, earlier pages, threads, New Chat with contact completion; the engine keeps the open space (`chat_open`) and the pane re-reads the list and the space on its `Chat`/`ChatMessages` events |
| `src/compose.rs` | The New Message and Forward windows (From picker, recipient completion, discard confirmation) and the reader's reply card |
| `src/settings.rs` | Accounts (rename, sign in, sign out), AI tagging and re-tagging, engine status |
| `src/avatars.rs` | Contact photos from `mail.avatars`, cached in memory; initials otherwise |
| `src/design.rs` | Flutter's style tokens (`Flutter/lib/ui/design.dart`): both palettes with their elevations, the type, radii, spacing, strokes, motion, list, mark, avatar, tag and notice metrics, avatar/topic/account colors |
| `src/icons.rs`, `src/format.rs` | Material Symbols glyphs; dates and sizes |
| `src/input.rs`, `src/textarea.rs` | Single-line field (IME, selection, clipboard) adapted from gpui's input example, and the multi-line editor for bodies |
| `src/bench.rs` | The frame measurement `scripts/bench.sh gpui` runs (`MAILSTREAM_BENCH=scroll`): the list scrolls itself and every frame's start and render-to-paint time is logged |
| `src/updater.rs` | Mac only: starts Sparkle from the bundle's Frameworks and adds the two update items to the application menu |

Keys (⌘ is Ctrl on Linux): ↑/↓ or J/K move (in the reader they page through conversations),
Return or Space opens, Esc or ⌘[ goes back, E or ⌃⌘A archives, ⌫ trashes, S or ⇧⌘L stars, U or
⇧⌘U toggles read, D or ⇧⌘D toggles waiting on you, R or ⌘R replies, A or ⇧⌘R replies to all, F
or ⇧⌘F forwards, ⌘N writes a new message (a new chat in Chat), / or ⌘F searches, ⇧⌘N refreshes,
⌃⌘S toggles the sidebar, ⌘, opens Settings. In a draft ⌘Return sends and Esc closes (asking first
when something was written). Right-click a conversation for the same commands.

The window follows Flutter's (MS-200): the sidebar sits on the window (a rail of the unified
mailboxes while it is hidden), and the list and the conversation are rounded islands floating on
it. From 1000 pt wide the conversation opens beside the list, which narrows as it slides in;
narrower windows slide it over the list. A bundle slides over the rows within the list's island.
The list stays mounted beneath them, so going back (Esc or a two-finger swipe) reveals the live
list without reloading it. Opening a conversation shows its header at once; bodies arrive after.

Limits:
- Deleting from the Trash for good is left to the app.
