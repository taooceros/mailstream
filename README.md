# Mailstream

A native macOS mail client (AppKit + WebKit, Swift 6) for Gmail and Outlook. It
talks to the providers directly: the Gmail API, Microsoft Graph and the Google
Chat API, over HTTPS from the app. New mail arrives by push through a small
Cloudflare Worker relay (`Push/`).

## Requirements

- macOS 15 or later, Swift 6 toolchain (Command Line Tools is enough, no Xcode project)
- A Google Cloud OAuth client and a Microsoft Entra app registration of your own
  (see First-time setup); their ids and secrets are build-time configuration
- For push: a Cloudflare account (`wrangler`) and the Google Cloud CLI (`gcloud`)
- Optional: the `omp` CLI on `PATH` with a working "smol" model role, for AI tagging

Accounts are added in the app: Mailstream ▸ Settings… (⌘,) ▸ Add Google Account… or
Add Microsoft Account… opens the provider's sign-in in your browser and returns to
the app. Refresh tokens are kept in one macOS Keychain item for all accounts, so
macOS asks about access at most once; the account list (address,
label, granted scopes) in `~/Library/Application Support/Mailstream/accounts.json`.
With no account, Settings opens at launch. Labels can be renamed inline there.

Mailstream resolves `PATH` from your login shell to find omp. It picks the newest
version on `PATH`, or `MAILSTREAM_OMP=/path/to/omp`.

## First-time setup

1. A signing identity, so the Keychain item the app creates stays readable across
   rebuilds. Best: add any Apple ID in Xcode → Settings → Accounts (a free
   Personal Team is enough) and create an "Apple Development" certificate under
   Manage Certificates; its Team ID lets rebuilds read the item without asking.
   Otherwise `scripts/signing.sh` installs a self-signed "Mailstream Local
   Signing" identity; macOS then asks once after each rebuild.
2. Google Cloud project:
   - enable the Gmail API, Cloud Pub/Sub API, Google Chat API (configured under
     Google Chat API ▸ Configuration: a name, an avatar and a description;
     interactive features can stay off) and People API;
   - create an OAuth client of type Desktop app (published, so tokens do not expire
     after a week);
   - create the Pub/Sub topic `gmail-push` and grant
     `gmail-api-push@system.gserviceaccount.com` the Pub/Sub Publisher role on it.
3. Microsoft Entra app registration: accounts in any organizational directory and
   personal Microsoft accounts, a Web redirect URI `http://localhost:49152`, a
   client secret, and the delegated Graph permissions `User.Read`,
   `Mail.ReadWrite`, `Mail.Send` and `Contacts.Read`.
4. Build secrets go into the login keychain, service `dev.mailstream.build`, one
   generic password per item (`security add-generic-password -U -s
   dev.mailstream.build -a <item> -w <value>`):

   | Item | Value |
   | --- | --- |
   | `google-client-id`, `google-client-secret` | the Google OAuth client |
   | `microsoft-client-id`, `microsoft-client-secret` | the Entra app registration |
   | `gmail-topic` | `projects/<project>/topics/gmail-push` |
   | `push-token` | a random string the app authenticates its socket with |
   | `graph-client-state` | a random string Graph echoes in every notification |
   | `pubsub-token` | a random string Pub/Sub's push endpoint carries |
   | `push-url` | written by `Push/deploy.sh` |

5. `npx wrangler login` and `gcloud auth login` (an account that owns the
   project), then `Push/deploy.sh`: it deploys the relay, sets its secrets, stores
   its URL as `push-url` and creates (or repoints) the `mailstream-push` Pub/Sub
   subscription that delivers Gmail's notifications to it.

`scripts/bundle.sh` copies these items into the app's `Info.plist` (`MSGoogleClientID`,
`MSGoogleClientSecret`, `MSMicrosoftClientID`, `MSMicrosoftClientSecret`, `MSPushURL`,
`MSPushToken`, `MSGraphClientState`, `MSGmailTopic`); a missing item is left out with a
warning. Without the push items the app polls instead.

## Build and run

```sh
./scripts/bundle.sh                 # release build → build/Mailstream.app
CONFIG=debug ./scripts/bundle.sh    # debug build
CONFIG=debug ./scripts/run.sh       # bundle, quit the running copy, launch
swift build                         # compile check only
```

The bundle is signed with "Mailstream Local Signing", else ad hoc with a warning (an
ad-hoc build cannot read the previous build's Keychain item, so sign-ins are lost on
every rebuild). `scripts/make-icon.swift` regenerates `Resources/AppIcon.png`.

GitHub Actions (`.github/workflows/build.yml`) builds every push and pull request on
macOS 26 with Xcode 26.4 (Swift 6.3), with warnings as errors, and keeps the app as a
disk image (`Mailstream.dmg`) in the run's artifacts. Each build of `main` replaces the `nightly` prerelease. CI has no
OAuth clients, push settings or signing identity, so that app is signed ad hoc and
cannot sign in; it is for checking the build, not daily use. Dragged to Applications, it needs
`xattr -dr com.apple.quarantine /Applications/Mailstream.app` before the first launch.

## Architecture

| Layer | Files | Role |
| --- | --- | --- |
| Accounts | `Backend/Accounts.swift`, `OAuth.swift`, `Tokens.swift`, `AppConfig.swift`, `UI/AccountsWindowController.swift` | Signed-in accounts, browser sign-in (authorization code with PKCE, loopback redirect), refresh tokens in the Keychain, build-time configuration from `Info.plist`, the Settings window |
| Backend | `Backend/API.swift`, `MailBackend.swift`, `GmailBackend.swift`, `MIMEMessage.swift`, `GraphBackend.swift`, `MessageAssembly.swift`, `HTMLText.swift` | `API` sends authorized requests per account (token refresh on 401, backoff on 429 and 5xx, 6 interactive / 3 background requests at a time). Gmail and Graph implement one `MailBackend` protocol: mailboxes with roles, cursor-paged envelopes, search, message content (bodies, inline `cid:` parts and attachments written to the cache), flags, moves, sending |
| Chat | `Backend/Chat.swift`, `ChatAPI.swift`, `Store/ChatStore.swift`, `UI/ChatPage.swift`, `UI/ChatReader.swift` | Google Chat REST API for Google accounts granted Chat access; main-actor spaces and message state with a disk cache, rendered into the reader's page |
| Push | `Backend/PushService.swift`, `Push/` | One WebSocket to the relay; registers Gmail watches and Graph subscriptions for every account and asks the store to refresh the account a notification names |
| Store | `Store/MailStore.swift`, `Replies.swift`, `Threader.swift`, `ConversationRow.swift`, `MailCache.swift` | Main-actor state, off-main list rebuilds, optimistic flag and move actions, reply planning, async disk cache in `~/Library/Caches/dev.mailstream.Mailstream` |
| Tagging | `Backend/OmpTagger.swift`, `Model/Tags.swift` | Runs `omp -p --model smol` on batches of conversations and parses the JSON tags |
| UI | `UI/*` | Sidebar plus a full-width content area that shows the list or one conversation, compose window |

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
soon as the socket reconnects. Unread counts also refresh every 5 min.

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
