# Mailstream

A native macOS mail client (AppKit + WebKit, Swift 6) for Gmail and Outlook. It
talks to the providers directly: the Gmail API, Microsoft Graph and the Google
Chat API, over HTTPS from the app. New mail arrives by push through a small
Cloudflare Worker relay (`Push/`).

## Requirements

- macOS 26 or later, Swift 6.2 or later (Command Line Tools is enough, no Xcode project)
- A Google Cloud OAuth client and a Microsoft Entra app registration of your own
  (see First-time setup); their ids and secrets are entered in the app at sign-in
- For push: a Cloudflare account (`wrangler`) and the Google Cloud CLI (`gcloud`)
- Optional: the `omp` CLI on `PATH` with a working "smol" model role (or the model set
  in `aiTagging.model`), for AI tagging

Accounts are added in the app: Mailstream ▸ Settings… (⌘,) ▸ Add Google Account… or
Add Microsoft Account… first asks for the OAuth client ID and secret, then opens the
provider's sign-in in your browser and returns to the app. Google needs a "Desktop
app" client and its secret; Microsoft needs redirect URI `http://localhost:49152`,
and the secret may be empty for a public client. The fields are prefilled with the
client last used for that provider, else the one the build carries in its
`Info.plist`. Refresh tokens, with the client each was issued to, are kept in one
macOS Keychain item for all accounts (service `dev.mailstream.Mailstream.oauth`,
account `refresh-tokens`), so macOS asks about access at most once; the account
list (address, label, granted scopes) in
`~/Library/Application Support/Mailstream/accounts.json`.
With no account, Settings opens at launch. Labels can be renamed inline there.

Mailstream resolves `PATH` from your login shell to find omp and picks the newest
version on it, unless `aiTagging.ompPath` names one (see Settings and agents).

## First-time setup

Run `scripts/setup.sh` (needs `gcloud` and `npx`). It skips whatever is already
done, so it can be re-run safely, and asks only for what it cannot do itself:

1. **Google Cloud:** it signs in to `gcloud` if needed, asks for a project id (new
   or existing), enables the Gmail, Pub/Sub, Chat and People APIs, and creates the
   `gmail-push` topic that Gmail publishes to. Then it opens the console twice:
   - name the Google Chat app;
   - create an OAuth client of type **Desktop app**, then publish it so its tokens
     do not expire after a week. Paste the client id and secret back.
2. **Microsoft:** it opens Entra app registrations. Register an app for "accounts in
   any organizational directory and personal Microsoft accounts" with Web redirect
   URI `http://localhost:49152`. Add the delegated Graph permissions `User.Read`,
   `Mail.ReadWrite`, `Mail.Send` and `Contacts.Read`, plus a client secret. Paste
   the client id and secret back.
3. **Relay secrets:** it generates `push-token`, `graph-client-state` and
   `pubsub-token`.
4. **Push relay:** it signs in to Cloudflare if needed and runs `Push/deploy.sh`.
   That deploys the relay, sets its secrets, stores its URL as `push-url` and points
   the `mailstream-push` Pub/Sub subscription at it. Run `Push/deploy.sh` again after
   changing `Push/worker.js`.

Everything is kept in the login keychain, service `dev.mailstream.build`.
`scripts/bundle.sh` copies the items into the app's `Info.plist` (`MSGoogleClientID`,
`MSGoogleClientSecret`, `MSMicrosoftClientID`, `MSMicrosoftClientSecret`, `MSPushURL`,
`MSPushToken`, `MSGraphClientState`, `MSGmailTopic`). A missing item is left out with
a warning; the OAuth clients are then entered at sign-in, and without the push items
the app polls instead.

## Settings and agents

Mailstream is meant to be driven by AI agents as well as by hand. Everything the app
can be told goes through one command, `mailstream`, which prints JSON:

```sh
ln -s /Applications/Mailstream.app/Contents/MacOS/Mailstream ~/.local/bin/mailstream
mailstream help                     # overview and every command
mailstream commands                 # every command and argument, as JSON
mailstream status                   # accounts, sync, push, AI tagging
mailstream config list              # every setting with value, default, description
mailstream config set sync.pollSeconds 120
mailstream mail list --mailbox inbox --unread --limit 10
mailstream mail search "invoice" --account you@gmail.com
mailstream mail show '<conversation id>'
mailstream mail archive '<id>' '<id>'
mailstream mail tag '<id>' --category newsletter
mailstream mail reply '<id>' --body - --yes < reply.txt
```

Settings live in `~/Library/Application Support/Mailstream/settings.json`: a flat
object of dotted keys holding only what differs from the defaults (`mailstream config
schema` prints its JSON Schema). `mailstream config …` edits the file without the app;
the app watches the file and applies any change at once, whether it came from the
CLI, an editor or the View menu. Unknown keys and invalid values are ignored and
listed under `settingsIssues` in `mailstream status`.

| Setting | Default | |
| --- | --- | --- |
| `aiTagging.enabled` | `true` | Smart Inbox tagging (also View ▸ Auto-Tag with AI) |
| `aiTagging.model` | `"smol"` | omp model role or id |
| `aiTagging.ompPath` | `""` | omp to run; empty picks the newest on `PATH` |
| `aiTagging.batchSize` | `20` | conversations per omp run |
| `aiTagging.passLimit` | `150` | conversations one pass looks at |
| `aiTagging.backoffSeconds` | `300` | pause after a failed run |
| `remoteContent.trustedSenders` | `[]` | senders whose remote content always loads ("Always Load" adds here) |
| `sync.pollSeconds` | `90` | polling while push is down |
| `sync.pushPollSeconds` | `900` | safety-net polling while push is up |
| `chat.pollSeconds` | `20` | Google Chat polling |

Every other command goes to the running app over a Unix socket,
`~/Library/Application Support/Mailstream/control.sock` (mode 0600; only your own
processes are answered), and starts the app in the background if it is not
running. The protocol is one JSON line each way:
`{"command": "mail.list", "args": {"unread": true}}` →
`{"ok": true, "result": …}` or `{"ok": false, "error": {"code", "message"}}`.

Conversations are addressed by the ids `mail list`, `mail search` and `mail show`
return; the app remembers them while it runs, with the mailboxes they were listed
from, which is where `mail archive` moves them out of. Listing fetches from the
providers without touching what the window shows. Actions answer once the provider
confirmed them and report each id under `done` or `failed`. Sending (`mail send`,
`mail reply`), signing an account out and deleting from the trash do nothing without
`--yes`; the error says what would happen. `accounts add google` opens the browser
sign-in and waits for the person to finish it.

Failures print `{"error": {"code", "message"}}` on stderr and exit 1, 2 for usage
errors, 3 when the app cannot be reached.

## Build and run

```sh
./scripts/bundle.sh                 # release build → build/Mailstream.app
CONFIG=debug ./scripts/bundle.sh    # debug build
CONFIG=debug ./scripts/run.sh       # bundle, quit the running copy, launch
swift build                         # compile check only
```

The bundle is signed with an "Apple Development" certificate when there is one, else
ad hoc. Nothing else is needed. Signed ad hoc, each new build is a new app to macOS,
which asks once whether it may read the saved sign-ins. A free certificate stops that
prompt: add any Apple ID in Xcode ▸ Settings ▸ Accounts, then Manage Certificates ▸ +
▸ Apple Development. `scripts/make-icon.swift` regenerates `Resources/AppIcon.png`.

GitHub Actions (`.github/workflows/build.yml`) builds every push and pull request on
macOS 26 with Xcode 26.4 (Swift 6.3), with warnings as errors, and keeps the app as a
disk image (`Mailstream.dmg`) in the run's artifacts. Each build of `main` replaces the `nightly` prerelease. CI has no
OAuth clients, push settings or signing identity, so that app is signed ad hoc, polls
for mail and asks for your OAuth client at sign-in. Dragged to Applications, it needs
`xattr -dr com.apple.quarantine /Applications/Mailstream.app` before the first launch.

## Architecture

| Layer | Files | Role |
| --- | --- | --- |
| Accounts | `Backend/Accounts.swift`, `OAuth.swift`, `Tokens.swift`, `AppConfig.swift`, `UI/AccountsWindowController.swift` | Signed-in accounts, browser sign-in (authorization code with PKCE, loopback redirect), refresh tokens in the Keychain, build-time defaults from `Info.plist`, the Settings window |
| Control | `Control/Settings.swift`, `Commands.swift`, `CLI.swift`, `ControlSocket.swift`, `ControlService.swift`, `MailStore+Control.swift` | settings.json with live reload (an observable `Settings`), the `mailstream` command table, the CLI, the control socket (a `NetworkListener` on a Unix-domain path) and the app side of each command |
| Backend | `Backend/API.swift`, `MailBackend.swift`, `GmailBackend.swift`, `MIMEMessage.swift`, `GraphBackend.swift`, `MessageAssembly.swift`, `HTMLText.swift`, `AsyncSemaphore.swift`, `QuotaBudget.swift` | `API` sends authorized requests per account (token refresh on 401, backoff on 429 and 5xx; a rate-limit refusal pauses the whole account's quota budget; Gmail runs 6 interactive / 3 background requests at a time within a 3,000-units-a-minute budget, Graph 3 / 1 for Outlook's limit of four per mailbox). Gmail and Graph implement one `MailBackend` protocol: mailboxes with roles, cursor-paged envelopes, search, message content (bodies, inline `cid:` parts and attachments written to the cache), flags, moves, sending |
| Chat | `Backend/Chat.swift`, `ChatAPI.swift`, `Store/ChatStore.swift`, `UI/ChatPage.swift`, `UI/ChatReader.swift` | Google Chat REST API for Google accounts granted Chat access; main-actor spaces and message state with a disk cache, rendered into the reader's page |
| Push | `Backend/PushService.swift`, `Push/` | One WebSocket to the relay; registers Gmail watches and Graph subscriptions for every account and asks the store to refresh the account a notification names |
| Store | `Store/MailStore.swift`, `ContentStore.swift`, `TagStore.swift`, `StoreProblems.swift`, `CoalescedTask.swift`, `Replies.swift`, `Threader.swift`, `ConversationRow.swift`, `MailCache.swift` | `MailStore`: accounts, mailboxes, listings, selection, search and optimistic flag and move actions. It owns `ContentStore` (bodies and previews) and `TagStore` (AI tags). `StoreProblems` is the one error channel. Also reply planning and the async disk cache in `~/Library/Caches/dev.mailstream.Mailstream` |
| Model | `Model/*` | Plain values: messages, conversations, tags, `Subject` (reply and forward prefixes, list tags) |
| Support | `Support/*` | Small shared helpers: chunking, base64url, ISO 8601 dates |
| Tagging | `Backend/OmpTagger.swift`, `Store/TagStore.swift` | Runs `omp -p --model <aiTagging.model>` on batches of conversations and parses the JSON tags |
| UI | `UI/*` | Sidebar plus a full-width content area that shows the list or one conversation; `WebShell` hosts the one web view that the mail reader and `ChatReader` render into; compose window |

`Push/` is the relay, a Cloudflare Worker with one Durable Object: Gmail (through
the Pub/Sub push subscription) and Graph (change notifications) POST to it, and
it forwards each notification's ids to the app's socket. It stores nothing.

Gmail labels are mailboxes, plus a virtual All Mail (archiving removes the
Inbox label). Outlook folders are mailboxes, addressed with immutable message
ids so a message keeps its id when it moves.

Threading groups messages from the Inbox and Sent of each account by the
provider's thread (Gmail thread, Outlook conversation) and by `Message-ID` and
`In-Reply-To`. When those are missing, it falls back to normalized subjects
within a 45-day window.

The reader loads remote content only from known senders: your own addresses,
people you have written to, and senders you chose "Always Load" for. Junk and
other mail block it with a strict CSP inside each message frame until you click
"Load Images". Each message body is rendered in a sandboxed `srcdoc` iframe,
with page JavaScript disabled.

The main thread never waits on IO or heavy work. Launch paints the last session
(accounts, mailboxes, envelopes, tags) from the cache before any provider
answers. A list rebuild (load cached pages, thread, format every row's sender,
subject, date and section) runs off the main actor; rebuild requests coalesce
and the newest wins, and a result is dropped if the data changed while it ran.
The main thread only publishes the finished list. Flag changes patch the
published list in place instead of rebuilding it. Previews arrive in one batch
per run-loop turn and reload only the rows whose content changed. Cache reads
and writes (JSON encoding included) run off the main thread.

Concurrency is Swift's own, with no GCD, locks, timers or Combine:
- The target defaults to main-actor isolation (`defaultIsolation(MainActor.self)`, Swift 6 language mode).
- Model and backend types are `nonisolated` or actors.
- Off-main work runs in `@concurrent` functions.
- Stores and `Settings` are `@Observable`. Each view controller and store follows what it depends on through
  `Observations` in a named task it owns and cancels.
- Repeated work coalesces through `CoalescedTask`, which keeps one pass running and at most one waiting.
- Polling, renewal and backoff are `Task.sleep` loops.
- Cache writes go through one ordered stream.
- The control socket is a `NetworkListener` on a Unix-domain path.
- Two callback bridges remain, each with no async API: the DNS-SD bridge (a dispatch queue) for BIMI lookups
  and the settings.json watch (a `DispatchSource` feeding an `AsyncStream`).

The reader is one persistent WebKit page, loaded and warmed at launch. Opening
a conversation replaces its content through JavaScript (no navigation), and the
reader slides in once the thread is in the page. Blocked and allowed remote
content share that page: its CSP allows remote images, styles and fonts, and
each blocked body carries a stricter policy of its own.

Message bodies are fetched, converted to HTML and checked for remote content off
the main thread, then kept in memory. Resting the pointer on a row (or moving
the keyboard highlight to it) for 80 ms loads that conversation, so opening it
renders the bodies with the thread in the first frame of the slide-in.

## Sync

With the relay configured, a Gmail watch and a Graph subscription per account
(each renewed a day before it lapses) report changes within
seconds. A notification refreshes that account's mailboxes in view and its
unread counts, bursts coalescing into one pass. While the socket is up the app
still polls every 15 min as a safety net; while it is down (no network, relay
unreachable, no push configuration) it polls every 90 s, and it catches up as
soon as the socket reconnects (`sync.pushPollSeconds`, `sync.pollSeconds`). Unread
counts also refresh every 5 min.

## Layout

The sidebar lists unified mailboxes first (Inbox expands into each account's
inbox, each account marked with its own color bar), then every account's full
folder tree under Accounts, by its label.

The list is one quiet line per conversation on fixed columns: an unread dot,
an initials avatar of the person the conversation is with (the latest sender who
isn't you; in Sent, the recipient), the senders, subject and preview, one topic
("● Work"), the date and the account's
color bar. `Re:`/`Fwd:` prefixes are dropped and `[list]` tags are dimmed.
Hovering a row cross-fades the date into an archive / trash / read / star pill.
Clicking a row, or pressing Return or Space, opens the conversation; Esc, ⌘[,
the toolbar back button or a mouse's Back button returns. A two-finger swipe
left or right on the trackpad drags the reader off that side and goes back once
past a third of its width or flicked (when "Swipe between pages" uses two fingers).

The reader is a 720 pt reading column: serif subject, one line of metadata,
messages separated by hairlines, collapsed messages as one-line previews, quoted
history and its "On …, X wrote:" line folded behind `•••`, and a reply pill
docked at the bottom.

Conversations between people (anything not tagged as a service category) with
more than one message read as a chat log instead, like Slack without bubbles:
every message open, a divider per day, and each message under its sender's
avatar, name and time. A sender's next message within an hour on the same day
drops the avatar and name and shows its time on hover. Reply and Forward float
over a message on hover. Signatures (Gmail, Outlook, Apple Mail and Thunderbird
markup, a `-- ` line or a "Sent from my iPhone" footer) fold behind `•••` along
with the quoted history, and so do the blank lines around them. The thread opens
on the first unread message, else the latest, and stays there while bodies
above it load, until you scroll, click or type. Service mail keeps the cards
above.

Reply and Reply All (pill, per-message buttons, toolbar, menu and list keys)
open an inline composer: the pill unfolds into a card with the recipients, a
text field, and the quoted message hidden behind `•••`. Expanding it shows the
original rendered as HTML, the same way as in the reader. Recipients follow the
usual rules: Reply-To, else From, or the recipients of your own message; Reply
All copies everyone else on To and Cc except your own addresses. They show at
once from what is loaded; once the original's body is in, the full plan (with
its Cc and `Re:` subject) replaces them, and Send waits for it. ⌘↩ sends. The
card's buttons switch between Reply and Reply All (keeping the text), open the
reply in a window (for attachments, Bcc, or editing recipients or the quote), or
discard it. Esc in the card blurs it, and a second Esc goes back to the list.
The draft is kept per conversation until you send or discard it, but only in
memory, so it is lost on quit. An empty reply is dropped on Esc.

Inline replies are sent as `multipart/alternative`: the plain part is the text
above an "On …, X wrote:" line and the `> `-quoted original, and the HTML part
quotes the original HTML in a Gmail-style `gmail_quote` blockquote, without
inline (cid) images. Forward and the reply window send plain text with an
editable quote; a forward carries the original's text under a "Forwarded
message" header, not its attachments. Replies and forwards carry `In-Reply-To`
and `References` and stay in the provider's thread.

Motion: the reader and bundles open by spreading out from the middle of the list and close by
settling back toward it, collapsed messages expand in place, archived or trashed rows
slide out while the rest close the gap, and a status capsule rises only while
mail is being fetched or tagged. macOS "Reduce motion" disables the page animations.
Every animation is an explicit Core Animation animation that asks for 120 Hz
(ProMotion) and starts from what is on screen, so reversing one mid-flight
turns it around instead of jumping. The reader turns off WebKit's 60 fps cap.
List rows lay themselves out by hand (no Auto Layout) to keep scrolling cheap.

## Smart Inbox and AI tags

In inboxes (and All Inboxes), each conversation is tagged by the omp model in
`aiTagging.model` ("smol") with one category plus up to two topic labels:

- **People**: written by a person. These stay in the main list, grouped by date.
- **Notifications**, **Newsletters** and **Invitations**: three tiles above the
  list showing their unread counts and top senders. Click a tile to browse it.
- **To respond**: set when a person asked you something and the latest message
  is not yours. These conversations come first, under "Waiting on you".

Tagging sends each conversation's sender, subject, preview snippet and a few
flags (whether you are in To, whether you replied) to the model provider
configured in omp. It runs in batches of 20, one omp process at a time, for up
to 150 untagged conversations per pass (`aiTagging.batchSize`, `aiTagging.passLimit`).

Tags are keyed by the latest message, so a new reply re-tags the conversation.
They are cached in `tags-v1.json` in the cache directory. Turn tagging off, or
re-tag everything, from the View menu or with `mailstream config set
aiTagging.enabled false` and `mailstream mail retag`; `mailstream mail tag` corrects
one conversation's tag. Without tags, the list is plain and dated.

## Avatars

Each list row and reader message shows the sender's picture when one is found,
and their first letter otherwise. Click a picture to enlarge it: a popover
shows it at 512 px, fetched again from the same source, with the name and
address. Sources, best first:

1. **Contact photos** from the signed-in accounts: Google (People API: saved
   contacts, "other contacts" and, on Workspace, the domain directory) and
   Outlook (Graph contacts).
2. **BIMI logo** of the sender's domain (DNS `default._bimi.<domain>`).
3. **Gravatar**, requested with the SHA-256 of the address and `d=404`.
4. **Site icon** of the domain, for automated mail only (tagged non-people, or a
   `noreply`-style address). Icons smaller than 32 px are skipped.

Free-mail domains (gmail.com, outlook.com, …) never get a domain logo. Results,
including misses, are cached as 96 px PNGs in
`~/Library/Caches/dev.mailstream.Mailstream/avatars` (hits 14 days, misses 3 days).
A change in the contact directory, rebuilt daily, retires cached misses.

## Google Chat

Every Google account whose sign-in granted the Chat scopes (`chat.spaces.readonly`,
`chat.messages.readonly`, `chat.messages.create`, `chat.memberships.readonly`,
`chat.users.readstate`) is also a chat account, in its mail account's color. The
API only works for Google Workspace accounts, not personal gmail.com ones, and
only when the OAuth client's project has the Chat API enabled and configured.

The sidebar shows a Chat item (with one child per chat account when there are
several) after the unified mailboxes. Its badge counts spaces with unread
messages. The list shows spaces newest first, in the same rows as mail: DMs and
bots under the other person's picture, groups and spaces under the latest
sender's. Opening a space shows its messages in the reader as a chat log, with
the first unread message marked, and marks the space read. Earlier messages load
from the top of the log.

The composer sits at the bottom of the space: Return sends, Shift-Return starts
a new line, and hovering a message offers Reply in thread. A message that fails
to send stays in the log with Retry and Delete. Spaces refresh every 20 s, and
an open space marks new messages read as they arrive. Search filters the loaded
spaces by title and latest message. The last 150 messages of the 20 most
recently opened spaces are cached in `chat-v2/` in the cache directory.

## Shortcuts

| Action | Menu | List key |
| --- | --- | --- |
| Open conversation or bundle | | Return / Space |
| Back to list | ⌘[ | Esc, two-finger swipe left or right |
| New message | ⌘N | |
| Reply / Reply All / Forward | ⌘R / ⇧⌘R / ⇧⌘F | r / a / f |
| Archive | ⌃⌘A | e |
| Move to Trash | ⌘⌫ | ⌫ |
| Toggle read | ⇧⌘U | u |
| Star | ⇧⌘L | s |
| Get mail | ⇧⌘N | |
| Search | ⌘F | |
| Send (compose window / inline reply) | ⇧⌘D / ⌘↩ | |
| Settings | ⌘, | |

## Limitations

- Only Gmail and Microsoft (Outlook, Microsoft 365) accounts; there is no IMAP.
- Opening a conversation marks its unread messages as read.
- Chat cannot send attachments, and searching chat only filters the spaces already listed.
