# Mailstream

A native macOS mail client (AppKit + WebKit, Swift 6). Its mail engine is
[himalaya](https://github.com/pimalaya/himalaya) 2.2.1, built into the app as a
Rust static library (`Engine/`), so there is no external mail CLI to install.

## Requirements

- macOS 15 or later, Swift 6 toolchain (Command Line Tools is enough, no Xcode project)
- A Rust toolchain (1.89 or later) to build the engine
- Accounts configured in `~/.config/himalaya/config.toml`, the same file the
  himalaya CLI reads
- OAuth tokens for Gmail and Outlook, provided through
  [ortie](https://github.com/pimalaya/ortie) and referenced from that config
- Optional: the `omp` CLI on `PATH` with a working "smol" model role, for AI tagging

Mailstream resolves `PATH` from your login shell, so the token commands in the
config (ortie, `security`) run as they do in a terminal. For omp it picks the
newest version on `PATH`, or `MAILSTREAM_OMP=/path/to/omp`.

## Build and run

```sh
./scripts/bundle.sh                 # release build → build/Mailstream.app (ad-hoc signed)
CONFIG=debug ./scripts/bundle.sh    # debug build
CONFIG=debug ./scripts/run.sh       # bundle, quit the running copy, launch
./scripts/engine.sh && swift build  # compile check only (the engine is a cargo build)
```

`scripts/make-icon.swift` regenerates `Resources/AppIcon.png`.

## Architecture

| Layer | Files | Role |
| --- | --- | --- |
| Engine | `Engine/` (Rust), `Backend/MailEngine.swift` | himalaya 2.2.1's modules as a static library. Runs himalaya command lines in-process, each on its own thread, and keeps every account's connections open between them |
| Backend | `Backend/Himalaya.swift` | Typed calls into the engine with per-account concurrency limits (4 interactive, 2 background) |
| Parsing | `Backend/MessageParser.swift`, `HTMLText.swift` | Converts mail-parser JSON into HTML or text bodies, inline `cid:` parts, attachments and snippets |
| Store | `Store/MailStore.swift`, `Threader.swift`, `ConversationRow.swift`, `MailCache.swift` | Main-actor state, off-main list rebuilds, optimistic flag and move actions, async disk cache in `~/Library/Caches/dev.mailstream.Mailstream` |
| Tagging | `Backend/OmpTagger.swift`, `Model/Tags.swift` | Runs `omp -p --model smol` on batches of conversations and parses the JSON tags |
| UI | `UI/*` | Sidebar plus a full-width content area that shows the list or one conversation, compose window |

Threading groups messages from the Inbox and Sent of each account using
`Message-ID` and `In-Reply-To`. When those headers are missing, it falls back to
normalized subjects within a 45-day window.

The reader blocks remote content by default with a CSP. Each message body is
rendered in a sandboxed `srcdoc` iframe, with page JavaScript disabled.

The main thread never waits on IO or heavy work. Launch paints the last session
(accounts, mailboxes, envelopes, tags) from the cache before himalaya answers.
A list rebuild (load cached pages, thread, format every row's sender, subject,
date and section) runs off the main actor; rebuild requests coalesce and the
newest wins, and a result is dropped if the data changed while it ran. The main
thread only publishes the finished list. Flag changes patch the published list
in place instead of rebuilding it. Previews arrive in one batch per run-loop
turn and reload only the rows whose content changed. Cache reads and writes
(JSON encoding included) run off the main thread.

The reader is one persistent WebKit page, loaded and warmed at launch. Opening
a conversation replaces its content through JavaScript (no navigation), and the
reader slides in once the thread is in the page. Only allowing remote images
reloads the page, because its CSP cannot be loosened in place.

## Layout

The sidebar lists unified mailboxes first (Inbox expands into each account's
inbox, each account marked with its own color bar), then every account's full
folder tree under Accounts.

The list is one quiet line per conversation on fixed columns: an unread dot,
an initials avatar of the person the conversation is with (the latest sender who
isn't you; in Sent, the recipient), the senders, subject and preview, one topic
("● Work"), the date and the account's
color bar. `Re:`/`Fwd:` prefixes are dropped and `[list]` tags are dimmed.
Hovering a row cross-fades the date into an archive / trash / read / star pill.
Clicking a row, or pressing Return or Space, opens the conversation; Esc, ⌘[
or the toolbar back button returns.

The reader is a 720 pt reading column: serif subject, one line of metadata,
messages separated by hairlines, collapsed messages as one-line previews, quoted
history and its "On …, X wrote:" line folded behind `•••`, and a reply pill
docked at the bottom.

Reply and Reply All (pill, per-message buttons, toolbar, menu and list keys)
open an inline composer: the pill unfolds into a card with the recipients, a
text field, and the quoted message hidden behind `•••`. Expanding it shows the
original rendered as HTML, the same way as in the reader. The recipients shown
first are a local guess (Reply-To or From, or the recipients of your own
message). himalaya's answer replaces them, and Send waits for it. ⌘↩ sends. The
card's buttons switch between Reply and Reply All (keeping the text), open the
reply in a window (for attachments, Bcc, or editing recipients or the quote), or
discard it. Esc in the card blurs it, and a second Esc goes back to the list.
The draft is kept per conversation until you send or discard it, but only in
memory, so it is lost on quit. An empty reply is dropped on Esc.

Inline replies are sent as `multipart/alternative`. The plain part and all
headers (threading, recipients, signature) come byte for byte from a himalaya
dry run. The HTML part quotes the original HTML in a Gmail-style
`gmail_quote` blockquote, without inline (cid) images. If the dry run is not a
single `text/plain` part, the reply is sent as plain text. Forward and
the reply window are still plain text with an editable `>` quote.

Motion: the reader and bundles open by spreading out from the middle of the list and close by
settling back toward it, collapsed messages expand in place, archived or trashed rows
slide out while the rest close the gap, and a status capsule rises only while
mail is being fetched or tagged. macOS "Reduce motion" disables the page animations.
Every animation is an explicit Core Animation animation that asks for 120 Hz
(ProMotion) and starts from what is on screen, so reversing one mid-flight
turns it around instead of jumping. The reader turns off WebKit's 60 fps cap.
List rows lay themselves out by hand (no Auto Layout) to keep scrolling cheap.

## Smart Inbox and AI tags

In inboxes (and All Inboxes), each conversation is tagged by the omp "smol"
model with one category plus up to two topic labels:

- **People**: written by a person. These stay in the main list, grouped by date.
- **Notifications**, **Newsletters** and **Invitations**: three tiles above the
  list showing their unread counts and top senders. Click a tile to browse it.
- **To respond**: set when a person asked you something and the latest message
  is not yours. These conversations come first, under "Waiting on you".

Tagging sends each conversation's sender, subject, preview snippet and a few
flags (whether you are in To, whether you replied) to the model provider
configured in omp. It runs in batches of 20, one omp process at a time, for up
to 150 untagged conversations per pass.

Tags are keyed by the latest message, so a new reply re-tags the conversation.
They are cached in `tags-v1.json` in the cache directory. Turn tagging off, or
re-tag everything, from the View menu. Without tags, the list is plain and dated.

## Avatars

Each list row and reader message shows the sender's picture when one is found,
and their first letter otherwise. Click a picture to enlarge it: a popover
shows it at 512 px, fetched again from the same source, with the name and
address. Sources, best first:

1. **Contact photos** from Google (People API: saved contacts, "other contacts"
   and a Workspace directory) or Outlook (Graph contacts). They come from ortie
   accounts whose names end in `-contacts`, e.g. `gmail-personal-contacts` with
   scopes `contacts.readonly`, `contacts.other.readonly` (plus
   `directory.readonly` on Workspace), or `outlook-personal-contacts` with
   `Contacts.Read`. Keeping them separate from the mail tokens means a failed
   grant never touches mail. The provider is read from `endpoints.authorization`.
2. **BIMI logo** of the sender's domain (DNS `default._bimi.<domain>`).
3. **Gravatar**, requested with the SHA-256 of the address and `d=404`.
4. **Site icon** of the domain, for automated mail only (tagged non-people, or a
   `noreply`-style address). Icons smaller than 32 px are skipped.

Free-mail domains (gmail.com, outlook.com, …) never get a domain logo. Results,
including misses, are cached as 96 px PNGs in
`~/Library/Caches/dev.mailstream.Mailstream/avatars` (hits 14 days, misses 3 days).
A change in the contact directory, rebuilt daily, retires cached misses.
Google's People API scopes need your own Google Cloud OAuth client (Desktop
app, published). Thunderbird's public client is refused for them.

## Shortcuts

| Action | Menu | List key |
| --- | --- | --- |
| Open conversation or bundle | | Return / Space |
| Back to list | ⌘[ | Esc |
| New message | ⌘N | |
| Reply / Reply All / Forward | ⌘R / ⇧⌘R / ⇧⌘F | r / a / f |
| Archive | ⌃⌘A | e |
| Move to Trash | ⌘⌫ | ⌫ |
| Toggle read | ⇧⌘U | u |
| Star | ⇧⌘L | s |
| Get mail | ⇧⌘N | |
| Search | ⌘F | |
| Send (compose window / inline reply) | ⇧⌘D / ⌘↩ | |

## Limitations

- OAuth runs outside the app (ortie plus keychain). In-app sign-in is not implemented yet.
- There is no push (IMAP IDLE or Graph subscriptions). The inbox refreshes every 90 s and unread counts every 5 min.
- Mail requests still go through himalaya's command interface, one command per call: listing a page re-runs its search, and nothing streams.
- Opening a conversation marks its unread messages as read.
