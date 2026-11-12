# Mailstream GPUI front end

A [GPUI](https://gpui.rs) (Zed's GPU UI framework) window for Mailstream, on macOS and Linux
(Wayland or X11). GPUI comes through [gpui-kit](https://gpui-kit.com) 0.9 with its `gpui-fast`
feature (MS-226): GPUI Fast, longbridge's retained-mode GPUI that follows Zed's. `Cargo.toml` names
the kit `gpui`, so the code says `gpui::…`; `secondary!("k")` (in `main.rs`) spells a binding's
⌘/Ctrl, as GPUI Fast's `KeyBinding::new` no longer reads `secondary-`. The
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

GPUI Fast draws a view from the last frame while nothing it read changed: entities, globals, list
and scroll state, the pointer and modifier keys are tracked
([retained mode](https://github.com/longbridge/gpui-fast/blob/main/docs/retained-mode.md)).
Anything else a render reads (a `Cell`, a thread-local, the time) needs `cx.notify()` or
`window.refresh()` when it changes; Blitz's pages refresh the window as their tiles arrive.
`GPUI_VIEW_RETENTION=0` draws every frame from scratch, to tell a stale view from a bug.

The controls are gpui-kit's components where the kit has one (MS-226): buttons and icon buttons
(`controls::button`, `icon_button`), their tooltips (`controls::tooltip`), switches, text fields,
segmented controls (`controls::segmented` / `segment`: the kit's small segmented `TabBar`), tags
(`controls::tag`: the kit's `Tag` without its rim), toasts (`controls::toast`: a kit notification
at the bottom center, pushed by `MailApp::show_toast`, replacing the one shown, gone after the
kit's 5 s) and the row's right-click menu (`ui/menu.rs`: a kit `PopupMenu` built as it opens, its
entries drawn in the app's row; arrows, Return and Escape are the kit's). Notices stay the app's
own: the kit's alert can hold neither a button nor a click. `main.rs` calls `gpui::init` and opens
every window with `gpui::open_window`, whose root hosts the kit's tooltips, menus, popovers and
notifications over the app; `design::sync_kit_theme` puts the palette into the kit's theme
(colors, `radius::CONTROL` corners, the UI font, `text::BODY` as the rem, the segmented track and
labels, the toasts' bottom margin) at launch and on every change of appearance, so the components
keep MS-200's look in both. The app's buttons and switches stay out of the Tab order, as before
the kit.

Text fields are the kit's `InputState` (one line; `masked` for a secret) and `TextareaState`
(written text), drawn by `input::field` / `input::area` without the kit's frame, in the text
style around them (`controls::field_frame` frames the search box and settings). The views
subscribe to the kit's `InputEvent::Change` and `PressEnter`; Esc reaches them as the kit's
`Escape` action, caught with `on_action` on an element around the field. The fields' key context
is `Input`: single-key bindings say `!Input`, and keys a draft takes inside its fields (Tab, ⌘↩)
are bound in `Compose > Input` / `ReplyBox > Input` after the kit's own bindings, which they then
beat. Placeholders take the kit's `muted_foreground` (MS-200's `muted`), and a right click in a
field opens the kit's Cut / Copy / Paste / Select All menu.

Always start it through `run.sh`. On macOS the engine reads the sign-ins from the Keychain item
that macOS pins to Mailstream's signature (identifier `dev.mailstream.Mailstream`, the Apple
Development team). `run.sh` signs the binary with `scripts/sign.sh` and refuses to start it unless
`codesign` shows that identifier and a team. A binary signed any other way (`cargo run` leaves an
ad hoc signature) makes macOS ask for the Keychain, and "Always Allow" there re-pins the item to
that one build, so the app and every other host get asked again. Linux keeps the sign-ins in a
file only the user reads (`~/.local/share/Mailstream`), so nothing is signed there.

On the Mac, `-AppleInterfaceStyle Dark` (or `Light`) after the executable shows the app in that
appearance without changing the system's, for screenshots: AppKit ignores that argument for an
app's appearance, so `main.rs` applies it; a launch without it follows the system.

HTML bodies are drawn by Blitz, the engine's renderer (`src/blitz_body.rs`, as in Flutter's
reader), on every platform: the build turns on the FFI crate's `blitz` feature, with one
adjustment for GPUI's dependency graph: `vendor/stylo_derive` is Stylo's derive crate 0.22.0 with
one error type spelled out (`to_css.rs`), which GPUI's `log` features make ambiguous. On the Mac
the reader's header has a "Renderer: Blitz / WebView (original)" switch (`state::Renderer`, after
Flutter's MS-138/MS-166): every launch starts in Blitz, the switch holds for that run only, the
open messages and quotes stay as they were, and only the chosen renderer holds pages. The
WebView (`src/webview.rs`, native `WKWebView`s) shows the engine's page of the whole mail and
folds its quotes in the page behind the same toggle. Both renderers apply the engine's mail HTML
rules (`FFI/src/mail_html.rs`, shared with Flutter, MS-234): where quoted history starts, the
reader's style sheet, the WebView's content policy, the inline parts a page shows and the links a
click opens (`is_mail_link`).
`MAILSTREAM_READER_RENDERER=webKit` starts in the WebView, for `scripts/bench.sh`.

Linux differs from the Mac in:
- HTML bodies: Blitz only (no WebView switch). fontique takes fontconfig through `dlopen`, which
  GPUI's font-kit then shares.
- Shortcuts: ⌘ is Ctrl (`secondary!` bindings), except the Mac's text navigation (⌘←, Emacs keys).
- The window: the compositor draws its frame where it offers to; elsewhere dragging the window
  between the islands or their title rows moves it once the pointer has travelled a few points
  with the button held (a click or tap there never does, MS-206); rows and controls never move it.
  The type is the desktop's sans-serif family (fontconfig). The back swipe takes the scroll as
  the compositor turns it for the person's scrolling direction.
- The window's material (MS-249): where the compositor blurs translucent windows (Hyprland with
  `decoration:blur` on, KWin) the window asks for blur and paints its warm window color as a wash
  over it, so the sidebar and the gaps between the islands are frosted glass; elsewhere (GNOME,
  X11, Hyprland with blur off) it stays opaque, so the desktop never shows through sharp.
  `src/blur.rs` asks at launch and switches the open window when Hyprland's configuration reloads
  or the compositor's blur capability changes. `scripts/vhypr.py start NAME --blur` shows it.
- Scrolling (Wayland): GPUI applies a wheel notch as one jump of three lines and stops a touchpad
  scroll dead when the fingers lift (on the Mac, macOS sends the trackpad's momentum itself).
  `vendor/gpui-fast-linux` is GPUI Fast's Linux platform 0.1.14 with one Wayland-only patch
  (MS-208, carried over in MS-226: `src/linux/wayland/scroll_motion.rs` and the "Mailstream
  patch" parts of `client.rs`). Wheel notches ease in over about 150 ms, and touchpad scrolls
  arrive as `Started`, `Moved`, `Ended` and then a fling that decelerates as Firefox's flings do
  (0.998 per ms); a click, the pointer leaving or fingers resting on the touchpad stops it. X11
  is unchanged. Moving to a newer GPUI Fast means vendoring its Linux crate again with the patch.
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
| `src/state/invitations.rs` | Meeting invitations in the reader (MS-210): each card asks the account's calendar for the event and the answer (`calendar_invitation`, again on every `Calendar` event) and answers there (`calendar_respond`) |
| `src/ui/` | The window as islands (`mod.rs`): the sidebar tree and the rail that stands in for it (`sidebar.rs`), the list's title row and the conversation's floating toolbar, with a scrim behind it and the subject between its capsules once the conversation scrolls under it (`toolbar.rs`), the lazy conversation list of two-line rows with Smart Inbox bundles pinned at its top and its "Waiting on you" group (`list.rs`), the reader's message cards with the reply card, opening a single mail at its top and a thread at the head of its first unread message, else its last (`reader.rs`) and a meeting invitation's card with Accept / Tentative / Decline (`invitation.rs`), the context menu (`menu.rs`) |
| `src/controls.rs` | The shared controls every view composes (Flutter's `controls.dart`, `glass.dart`, `common.dart`): buttons, icon buttons, tooltips, switches, segmented controls, tags and toasts (gpui-kit's, see above), capsules, islands, the title bars that move the window, popovers, pills, marks, list rows and their lifted card, notices |
| `src/webview.rs` | macOS, the reader's WebView renderer: HTML bodies in native `WKWebView`s laid over the reader: placed and clipped each frame, sized to their page, the engine's page and page script (style sheet, quote fold), page scripts off, links the engine allows opened in the browser, wheel events handed back to the reader |
| `src/blitz_body.rs` | HTML bodies drawn by Blitz on the engine's pages thread (the default renderer everywhere), shown as tiles near the viewport, quoted history as a page of its own; links the engine allows open in the browser, a drag selects and ⌘C (Ctrl+C) copies |
| `src/swipe.rs` | Two-finger back swipe over the bundle and reader layers, following the system's swipe-between-pages setting |
| `src/chat.rs` | `ChatPane`: Google Chat spaces (older pages load near the list's end, `chat_more`), a space's log (bottom-aligned, runs and day dividers, markup and links, uploaded pictures shown in it once `chat_attachment` downloads them, other files and videos as tiles in their kind's color that open in the system's default app), sending, earlier pages, threads, New Chat completing the account's Chat partners (`chat_people`) before contacts; the engine keeps the open space (`chat_open`) and the pane re-reads the list and the space on its `Chat`/`ChatMessages` events. `MAILSTREAM_CHAT_FIXTURE=<file.json>` shows a synthetic chat without an account (the module's `fixture` says what the file holds) |
| `src/calendar.rs`, `src/calendar/` | `CalendarPane` (MS-211): every account's events from `calendar_events` (a week back on, read again on the engine's `Calendar` event), shown instead of the mail list while Calendar is selected. `agenda.rs`: the list island (this week's days on a strip with their booked hours, the meeting about to start with Join, notices for accounts to sign in again or whose fetch failed, the days from today with their events; Going / Maybe / Decline on unanswered invitations, through `calendar_respond`); `detail.rs`: the open event beside it (over it in a narrow window) with its answer, Join, guests and notes, or in the week view in gpui-kit's popover beside its card, closed by a press outside it, Esc or another week (MS-184); `model.rs`: the agenda's days, the Up Next meeting, how a day's timed events sit side by side and the hours they book |
| `src/compose.rs`, `src/compose/` | The drafts (From picker, recipient completion, attachments, discard confirmation) in the compose column (`column.rs`: tabs, docked, swapped, floating or minimized), and the reader's reply card |
| `src/settings.rs` | Settings as Flutter's (`settings.dart`, MS-214): accounts (sign in with your own OAuth client, prefilled with the last one; rename, remove), every setting of settings.json in Flutter's groups through the engine (`settings`, `set_setting`, `reset_setting`; re-read on its `Settings` event, the file's problems on top; the `flutter.*` keys stay out), tag sync per mail account, what auto-read marked read with undo, re-tagging, engine status |
| `src/avatars.rs` | Contact photos from `mail.avatars`, cached in memory; initials otherwise |
| `src/design.rs` | Flutter's style tokens (`Flutter/lib/ui/design.dart`): both palettes with their elevations, the type, radii, spacing, strokes, motion, list, mark, avatar, tag and notice metrics, avatar/topic/account/attachment colors, and what the window behind the islands is (`window_material`, `window_background`, `window_fill`: the Mac's Liquid Glass, or its blur under a wash of the window color before macOS 26; on Linux the compositor's blur under the same wash where it blurs, opaque where it doesn't) |
| `src/blur.rs` | Linux: whether the desktop blurs behind a translucent window (Hyprland's `decoration:blur` over its IPC, followed when its configuration reloads; elsewhere the `ext_background_effect_manager_v1` blur capability or KWin's blur manager), which picks the window's material and switches the open window when it changes (MS-249) |
| `src/glass.rs` | macOS 26: the window's Liquid Glass (`NSGlassEffectView`) under the transparent GPUI view, as Flutter's window, in the palette's warm `glass_tint` for the appearance (MS-242) |
| `src/icons.rs`, `src/format.rs` | Material Symbols glyphs; dates and sizes |
| `src/input.rs` | `field` / `area`: gpui-kit's one-line and multi-line text fields in the surrounding text style (see above) |
| `src/bench.rs` | The frame measurement `scripts/bench.sh gpui` runs (`MAILSTREAM_BENCH=scroll`): the list scrolls itself and every frame's start and render-to-paint time is logged. The window runs in the background, so it opens without GPUI Fast's limit of about 30 fps for a window that isn't key |
| `src/updater.rs` | Mac only: starts Sparkle from the bundle's Frameworks and adds the two update items to the application menu |

Keys (⌘ is Ctrl on Linux): ↑/↓ or J/K move (in the reader they page through conversations),
Return or Space opens, Esc or ⌘[ goes back, E or ⌃⌘A archives, ⌫ trashes, S or ⇧⌘L stars, U or
⇧⌘U toggles read, D or ⇧⌘D toggles waiting on you, R or ⌘R replies, A or ⇧⌘R replies to all, F
or ⇧⌘F forwards, ⌘N writes a new message (a new chat in Chat), ⌘W closes the open conversation (or
Chat's open space or New Chat form, or the calendar's open event), / or ⌘F searches, ⇧⌘N
refreshes, ⌃⌘S toggles the sidebar, ⌘, opens Settings. In the calendar ↑/↓ or J/K open the next or
previous event and ⌘R refreshes it. In a draft ⌘Return sends, ⌘W closes it (asking first when
something was written), Esc closes one with nothing written or hands the keyboard back to the
list and conversation, and ⇧⌘A attaches files; in the reply card ⇧⌘O moves the reply to the
compose column. Right-click a conversation for the same commands.

The window follows Flutter's (MS-200): the sidebar sits on the window (a rail of the unified
mailboxes while it is hidden), and the list and the conversation are rounded islands floating on
it. From 1000 pt wide the conversation opens beside the list, which narrows as it slides in;
narrower windows slide it over the list. A bundle slides over the rows within the list's island.
The list stays mounted beneath them, so going back (Esc or a two-finger swipe) reveals the live
list without reloading it. Opening a conversation shows its header at once; bodies arrive after.

Writing mail happens in a compose column, as in Flutter (MS-61, MS-66), so other mail can be read
beside it: ⌘N, the list's pencil, F (forward) and the reply card's "move to the compose column"
button open it at the window's trailing edge, the sidebar folding to the rail below 1440 pt
(⌃⌘S opens it anyway). Dragging the gap beside it moves that edge alone; the list and the
conversation stay side by side while 700 pt remain for them and slide over each other below that.
Its title bar closes the draft, minimizes it to a capsule in the bottom-right corner, swaps it to
the other side of the conversation (between it and the list), or floats it over the window
(dragged by its title, sized from its top-left corner). Each draft is a tab; they stay in memory,
through mailbox and conversation changes, until sent or discarded. A reply moved there still goes
out with `mail.reply`, keeping its thread; its To, Cc, Bcc and Subject are editable, and fields
left as they opened keep the engine's reply plan.

Limits:
- Deleting from the Trash for good is left to the app.
