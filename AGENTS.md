# AGENTS.md

## Current focus: GPUI

- Product work, new features and UI acceptance go to `GPUI/` unless the user names another
  front end (MS-240; until then it was Flutter). The AppKit and Flutter front ends stay (not
  deprecated) and keep working through engine changes, but don't grow their features or port
  new GPUI features to them. Issues the user files for Flutter (by name, its `flutter` Area or
  a Flutter project) are still Flutter work, and so is Windows, where Flutter is the only front
  end.

## Version control: jj

Colocated jj repo (`.jj` + `.git`), pushed to `github.com/taooceros/mailstream` (public).

- Use `jj`, not raw `git`: `jj st`, `jj diff`, `jj describe -m "…"`, `jj new`, `jj log`.
- Each issue's work lives on its own bookmark `ms-<n>` (`ms-179`) in the shared repository
  below. Each change is its own described commit (`jj commit -m "<what and why>"`, then `jj
  bookmark set ms-<n> -r @-`; `jj bookmark create` the first time), made unprompted as soon as it
  is built and verified. Never leave finished work uncommitted in `@`; commit it to `ms-<n>`
  before you stop for any reason (question, blocker, hand-over), so the next agent finds it.
  `ms-<n>` stays local: every checkout sees it without a push.
- Landing: once the tests for what you changed pass and you have checked it in the real app,
  the issue's own agent lands it with `python3 scripts/land.py MS-<n>` in its checkout. Under a
  lock shared by every lander it fetches, rebases `ms-<n>` onto `main@origin`, moves `main` to
  it, pushes `main` and deletes `ms-<n>`; it prints the landed commits for the hand-over. When
  it stops on a conflict, resolve it in `ms-<n>`'s commits, rerun the tests it touches (and look
  again in the real app when it touches UI), then run it again. Nobody moves or pushes `main`
  any other way.
- Every agent works in its own ovnfs checkout, opened with `python3
  ~/projects/mailstream/scripts/checkout.py <unique>` and removed with `python3
  ~/projects/mailstream/scripts/checkout.py rm <unique>` once its part is done (landed and handed
  to the install batch or In Review, [Release delivery](#release-delivery)); how to use one is the
  `ovnfs` skill (`.omp/skills/ovnfs/SKILL.md`, its first section for this repo). Never the default
  checkout, never `jj workspace add` yourself (the `../mailstream-*` workspaces take no new work),
  never omp's `isolated` option for subagents. A woken agent opens a fresh one and carries on
  with `jj new ms-<n>`. Work the user keeps off `main` (a standalone trial) stays on its own
  bookmark.
- Shared repository (MS-179): each checkout is a jj workspace of the default checkout's
  repository, so every commit, bookmark and working copy (`<checkout>@` in `jj log`; `jj diff -r
  <checkout>@` shows what that agent is changing now) is visible in every checkout at once, and
  so is the operation log. Hence: never `jj undo` or `jj op restore` (they take back other
  agents' operations too); to take back one of your own, `jj op log`, then `jj op revert <id>`.
  Never rewrite, abandon or move commits and bookmarks of another issue or another checkout's
  working copy. When jj says your working copy is stale (another workspace rewrote a commit under
  it), run `jj workspace update-stale`; your uncommitted changes stay. Use jj, not git, in a
  checkout: its `.git` is ovnfs's capture and goes stale. Checkouts opened before MS-179 keep a jj
  repository of their own until removed; they land with `land.py` too. After upgrading jj, test
  the graft again (`scripts/checkout.py`'s header says how).
- The default checkout is locked (MS-156, `scripts/source_lock.py`): every agent's tools start in
  `~/projects/mailstream`, and a relative path or a missing `cwd` put agents' edits and deletes
  there. Its tracked files and folders refuse writes, new files, deletes and renames, whatever
  the tool; reads, `jj st`, `jj git fetch` and opening checkouts work, and its `.jj` (the
  repository every checkout shares) stays writable. `Permission denied` under
  `~/projects/mailstream` means a path missed your checkout: redo it with the checkout's absolute
  path or `cwd`, and never unlock it. Only `warm-build.py` unlocks it, for its run;
  `linear-poll.py watch` locks it again whenever anything there is writable. A `jj` command that
  rewrites its working copy fails there and leaves it stale; the script's header says how to
  recover.
- One line: every front end (the AppKit app, `GPUI/`, `Flutter/`) and the engine they share
  (`Sources/MailstreamKit`, `FFI/`) live on `main`, and every checkout starts from it and lands
  on it; `ms-<n>` bookmarks live only until their issue lands; the
  `gpui-ui` and `flutter-ui` bookmarks are history and take no new commits. An engine change
  updates every front end that calls what it changed, in the same change.
- The repo is public: never commit secrets, tokens, OAuth client files or personal mail data.

## Work tracking: Linear

Team `MS` (the Mailstream app: code in this repo, every machine), split into projects by effort;
issue IDs look like `MS-12`. Team `WIN` holds the Windows machine's own chores (its setup and
tools, nothing in this repo); every `WIN` issue goes to the Windows poller. Use omp's Linear tools.
The issue, not the session, is the record other sessions read.

Three machines poll: the Mac (Linear app `omp`), Windows (`windows-omp`; `WIN` issues and `MS`
issues labelled `windows`) and Linux (NixOS, `linux-omp`; `MS` issues labelled `linux`).
On Windows and Linux there are no omp Linear tools or keychain. Linear
is reached as the machine's OAuth app: its client id (line 1) and secret (line 2) are in
`~/.config/linear/credentials`, and the access token in `~/.config/linear/token.json`. Call
`https://api.linear.app/graphql` with `Authorization: Bearer <access_token>`. On an
`AUTHENTICATION_ERROR` (tokens last ~30 days) fetch a new one with
`curl https://api.linear.app/oauth/token -d grant_type=client_credentials -d client_id=… -d
client_secret=… -d scope=read,write` into `token.json`; `linear-poll.py` does this itself. Never
print or commit either file. The apps can't create labels; ask the user for new ones.

### Writing

Everything on Linear is plain Chinese that someone who has not read the code understands at
once: conclusion first, then what happened, how it affects the user, what was done and what
remains. Explain the terms the reader needs in a few words; leave out the rest and long logs. No
hedging or repeated disclaimers ("未授权", "不代表", "不算通过"); own mistakes; state real unknowns
as facts and never present the unverified as done. Identifiers, paths, commands and commit
hashes stay as they are.

### Filing

- Labels: one type (`Bug`, `Feature`, `Improvement`, `Performance`, `Regression`) plus one `Area`
  (Linear allows one; for work across areas, the one most of it is in). Areas are the parts of
  the app (`chat`, `concurrency`, `flutter`, `gpui`, `motion`, `ui-shell`) or the layer (`sync`,
  `cache`, `ffi`). Repo tooling (AGENTS.md, scripts, CI, build setup) takes no Area.
- Project: the team project whose description the issue fits. When none fits, create one (team
  `MS`, short Chinese name, one-line scope description, lead taooceros). Sub-issues go in their
  parent's project.
- Label and place issues you file when creating them. Issues the user files often have neither:
  the agent that claims one adds them before its start comment (the polling session leaves this
  to it).
- File your own issues in Backlog; only the user starts them (moving one to Todo or adding `omp`).
  Problems found outside the current change get their own Backlog issue; don't fix them in passing.
- An issue filed on Windows or Linux for that machine to do carries its machine label (`windows`,
  `linux`) from the start, added by the agent that files it.

### Working an issue

- Claim before working: every agent, subagents included, works under an issue it has claimed;
  with none for the work yet, create one and claim it. Work that serves another issue is its
  sub-issue: set Linear's parent field (`parentId`) when creating it; naming the parent only in
  the text does not count.
- Start: read the issue and its comments. Skip issues already In Progress unless the user or the
  polling session hands one over. Move it to In Progress, comment
  `[omp @ <checkout>] start (<agent id>): <plan>` (the subagent/roster id, so the issue shows
  who owns it), then add the `claimed` label. Taking over or resuming counts as starting: post a
  new start comment with your own id. Remove `claimed` when you stop (In Review, handed to the
  install batch, Todo with a question, Backlog when blocked).
- Progress: record decisions, blockers and dead ends as comments; the next session only sees
  what is on the issue.
- Blocked by another issue (it needs that change first, or the user said to wait for it): add a
  "blocked by" relation (file the blocker first if it doesn't exist), comment what it waits for,
  move it to Backlog and stop. Once every blocker is on `main` (labelled `install`, In Review,
  Done or Canceled; the user's review is not awaited), the poll hands it out again, in Todo or
  Backlog, and its agent carries on from the comments.
- Questions only the user can decide: don't guess. Assign the issue to the user (taooceros), move
  it to Todo, start the comment with `**❓ 需要你回答：**` followed by the question(s), each
  answerable on its own, and stop.
- Finish: reference the issue in the commit (`Fixes MS-12`, or `Refs MS-12` for partial work)
  and land it on `main` (`scripts/land.py`, [Version control](#version-control-jj)) once the
  tests for what you changed pass and you have checked it in the
  real app yourself; then hand it to the install batch ([Release delivery](#release-delivery)),
  which only puts `main`'s tip in `/Applications`. A commit or passing tests is not completion:
  the batch install agent moves the issue to In Review once the installed copy passes its
  mechanical checks, with a short note (deployed commit, `/Applications` targets, checks); the
  user moves it to Done. Tooling-only changes install nothing: their agent moves them to In
  Review itself, commenting what changed, the checks run and what remains.
- UI changes (a new or changed view, flow or interaction, in any front end): before handing
  over, the code agent attaches to the issue screenshots of its own build of the real app (a
  recording for motion) showing every state the change adds or alters, light and dark; without
  them the work is not ready to install.
- Parents: whoever moves a sub-issue to In Review checks its siblings. Once none is left in
  Backlog, Todo or In Progress, it finishes the parent too: a summary comment listing each
  sub-issue and what remains, then In Review. Nobody else picks up a parent an agent filed.

### Polling session

An omp session runs `RUNNING_FILE=~/.local/state/linear-poll/running python3 scripts/linear-poll.py watch`
as one background bash job with no deadline (`timeout: 0`) and `wait`s on it: the script checks
Linear every minute and returns only once it has claimed something. Each line it prints carries
what the session needs to hand the work out (an issue's title, state, labels, project, parent,
creator and the start of its description; a comment's issue title and body; the script's header
lists every field), so the session doesn't read issues on Linear itself (MS-174). The session
hands the issues it claims, and the user's comments, to subagents, then starts it again. It keeps
RUNNING_FILE a JSON array of its running agent ids, rewritten whenever one starts or finishes;
the script re-reads it each pass. The script's header says what it claims and how the session
picks the agent. Each time before it starts the script again, the session runs `python3
scripts/warm-build.py` in the default checkout ([Building](#building)).
`python3 scripts/linear-omp.py MS-12` opens one issue in a new Tern tab instead. The scripts are
Python (standard library only) so every machine runs the same ones; on Windows the command is
`python`, and the poller also needs `HOST_LABEL=windows LINEAR_TOKEN_FILE=~/.config/linear/token.json
LINEAR_CREDENTIALS=~/.config/linear/credentials`; on Linux the same with `HOST_LABEL=linux`.

Subagents start from the repo's agent definitions in `.omp/agents`, which carry what every
hand-out repeats: open a checkout of their own named after their agent id, give tools absolute
paths under it (a subagent starts in the session's folder, the locked default checkout, and omp
can't start it elsewhere), read AGENTS.md (subagents don't get it otherwise), read and claim the
issue, report on it, yield two or three lines. The session picks one per task item:

- `ms-issue`: an issue to work (code or tooling), named `MS<number><Topic>` (`MS173BlitzLinear`).
- `ms-discuss`: a question, design or discussion to answer on the issue without changing code.
- `ms-install`: the `install` line, named `Install<n>` (`Install7`), its task the listed
  identifiers; it only installs and runs mechanical checks ([Release delivery](#release-delivery)),
  on the cheap `sonic` setup in its frontmatter (MS-176), and doesn't count against the heavy
  agents below.

The task text is the issue identifier plus at most a line or two the issue doesn't say (a peer
changing the same code, a narrower scope); the agent reads the issue, labels and places it and
claims it. `context`, which the task tool requires, is one line. For example:
`{"context": "Polling session hand-out.", "tasks": [{"agent": "ms-issue", "name":
"MS173BlitzLinear", "task": "MS-173. MS164BlitzFonts is changing the Blitz page code: coordinate
via write agent://MS164BlitzFonts first.", "solutionSpace": "cause open"}]}`. omp reads the
definitions from the session's folder, so a change to them reaches the session once
`warm-build.py` moves the default checkout to `main`.

Cost rules (MS-157): at most three heavy agents (ones that write code) run at once; other
claimed issues wait in the queue of `~/.local/state/linear-poll/session.md`, stay in Todo with a
comment saying they are queued (Backlog holds only what nobody plans to do yet, MS-175), and start
in order as slots free; the poller doesn't hand out a Todo issue again once omp has commented on
it or claimed it. Code is written by the default model, and its agent tests it in the real app
itself; release installs and their mechanical checks go to the batch install agent, one run for
every issue waiting, at most once an hour (`INSTALL_EVERY`). Agents write their full report on
the issue and yield only two or three lines (result, commit, issue state, anything the user
must do).

The session hands over to a fresh one once its own context passes about 150K tokens: every wake
re-reads the whole conversation. It waits until no agent of its own is running (their results
can't reach another session), writes `session.md` (queued issues in order, what waits on the
user, open benchmarks or checks, anything else the next session needs; running agents are
none by then), empties RUNNING_FILE, opens
`tern new tab --cwd ~/projects/mailstream -- omp "Polling session: read AGENTS.md and
~/.local/state/linear-poll/session.md, then start the poller."` and stops (it does not start
the script again). The new session reads `session.md`, starts the queued issues and the script.

The Mac (app `omp`), the Windows machine (app `windows-omp`) and the Linux machine (app
`linux-omp`) talk only through Linear. New work for another machine is an issue (a sub-issue
when it serves one) labelled `omp`, plus `windows` or `linux` (or in team `WIN` when it is a
chore on the Windows machine, not repo work): its poller claims it whoever created it. A comment
naming a machine's app reaches that machine alone on any issue, from the user or another
machine; agents write the name in backticks (`` `@omp` ``), since Linear rejects an app's
@-mention of an app.

## Building

- Rust builds (`Engine/`, `FFI/`, `GPUI/`, Turso) run plain cargo: the repo's
  `.cargo/config.toml` turns off the rcache wrapper in `~/.cargo/config.toml`, which handed out
  corrupt artifacts in ovnfs checkouts (MS-131). Don't set `RUSTC_WRAPPER` to bring it back.
- The default checkout keeps a warm build of `main` (`FFI/target` release, `Engine/target`
  debug for the engine's tests, `.turso/`) that every new checkout starts from and keeps (MS-151,
  MS-152, MS-159): a checkout's first release `ffi.py` rebuilds only the repo's 2 crates (~30 s)
  instead of 460 (~3–4 min). `ffi.py` builds the library and the command line in one cargo run
  into `FFI/target` and drops `SDKROOT`/`CPATH`-style variables: C build scripts rebuild
  everything above them when those differ from the warm build's. Run release engine builds
  through `ffi.py`, not a second cargo in `Engine/` (that compiles the engine and its
  dependencies again). Never delete the default checkout's build folders. Only `python3
  scripts/warm-build.py` builds there: it fetches `main`, moves the default checkout's empty
  change onto it (unlocking its source for the run) and builds.
- Run anything that may take over a minute (a cold cargo build, `ffi.py`, `install.sh`, `flutter
  build`, a Swift release build, `bench.sh`) as one blocking eval cell, not `bash`: Python
  `subprocess.run(…, capture_output=True, text=True)` with the cell's `timeout` set (up to 3600),
  printing the exit code and the output's last lines. A `bash` call moves to the background after
  60 s, and `wait` then hands back a still-running snapshot every ~10 s, each one a full request
  that re-reads the whole conversation: ~860 such wake-ups on 11-07–11-09, ~590 of them on
  builds (MS-159). Don't start a long build and then wait on it with `wait` or `sleep`.
- The app links the Turso database engine (`.turso/lib/libturso_sqlite3.a`). Run `scripts/turso.sh`
  once before `swift build` (it needs rustup; `bundle.sh` runs it too). It builds the pinned
  version with cargo in a few minutes and returns at once after that.
- Check with `swift build` and launch with `scripts/run.sh`: both use the incremental debug build
  (seconds per edit). A release build (`scripts/bundle.sh`, `CONFIG=release`) re-optimizes the
  whole module single-threaded, ~30 s per edit; build it only to check performance or ship.
- All Swift code is in the `MailstreamKit` target (`Sources/MailstreamKit`); the `Mailstream`
  executable only calls `MailstreamApp.main()`. GPUI and Flutter run the Rust engine (`Engine/`)
  through the UniFFI crate in `FFI/`, which `scripts/ffi.py` builds; the Swift engine stays only
  for Mailstream.app until MS-120. New commands go in the Rust engine's command table
  (`Engine/src/commands.rs`) and the Swift `CommandSpec`/`ControlService` while both exist, then
  get a typed method in `FFI/src/lib.rs`; GPUI uses that crate directly, Flutter through the
  Dart binding `ffi.py` generates (`Flutter/README.md`).
- The engine keeps the state every front end shares (the open chat space and its history, as for
  the native reader); front ends re-read it on the engine's change events.
- Test only what you changed: build, then exercise the parts your change touches (the view, flow
  or command you edited). Don't re-run the whole app checklist or every CLI case after each
  change; we iterate in small steps.

## Release delivery

Finished fixes reach `/Applications` as signed release builds, never Debug, in batches (MS-159);
on Linux they reach `~/.local/opt` the same way (MS-224, below).
All testing happens before that (MS-176): each code agent checks its own change in the real app,
and one cheap agent then installs `main`'s tip for every issue waiting and checks only that the
install worked. Separately, each push to `main` that touches the GPUI or Flutter app or the
engine publishes a signed update of that app without approval, which its installed copy installs
by itself (`GPUI/README.md`, `Flutter/README.md`); the batch install still runs for each issue.

The code agent:

1. Tests its change in the real app before landing: a release or debug build from its own
   checkout (never an `/Applications` copy; it doesn't install), over synthetic data (no
   accounts or personal mail, like the fixtures `GPUI/README.md` and `Flutter/README.md` list),
   no sending mail, nothing destructive. It takes every screenshot or recording the change
   needs, UI states in light and dark, and attaches them to the issue. GPUI on the Mac goes
   through `python3 scripts/shots.py` (MS-243; its header has the commands): one run builds the
   app, fills a synthetic home, plays a script of clicks, keys and shots in light and dark and,
   with `--upload MS-<n>`, prints the Markdown for the comment; no screen or computer use, so
   it works with the Mac locked. Other front ends (and recordings) are driven with computer
   helpers (launching the executable with `-AppleInterfaceStyle Dark` shows it dark).
   On Linux it runs the app only in a private Hyprland desktop of its own, `python3
   scripts/vhypr.py start <name> [--dark]` (the script's header has every verb: `run`, `shot`,
   `click`, `drag`, `scroll`, `type`, `key`, `appearance`, `stop`), never on the user's screen,
   and stops it when done. The app there gets an empty data folder of its own. Never start
   another compositor (sway, cage, weston) on the GPU: a headless sway hung it twice on 11-11,
   resetting the GPU and crashing the user's session.
2. Lands its `ms-<n>` bookmark on live `main` with `python3 scripts/land.py MS-<n>` ([Version
   control](#version-control-jj)), which keeps other agents' changes.
3. Picks the targets: `gpui` (`Mailstream GPUI.app`) by default; `flutter` (`Mailstream
   Flutter.app`) or `native` (`Mailstream.app`) only when the user asks or its installed
   behavior is directly affected; never an app that isn't installed yet. On Linux: `gpui`
   (`~/.local/opt/mailstream-gpui`) and `flutter` (`~/.local/opt/mailstream`), each when the
   change reaches it (an engine change reaches both). Tooling-only changes
   install nothing (In Review directly, [Finish](#working-an-issue)). Changes to deployment
   itself (`install.sh`, `install-linux.py`, signing, updates) exercise the real path: their own
   agent installs them as below.
4. Hands over: a comment starting `**待安装**` with what changed, the commit, the targets and
   what it verified, with its screenshots; nothing is left for the installer to test. Then it
   adds the `install` label, removes `claimed`, leaves the issue In Progress and removes its
   checkout.

The batch install agent (one per `install` line of the poller, [Polling session](#polling-session)):

1. Claims each listed issue (start comment, `claimed`) and takes the targets from its latest
   `**待安装**` comment.
2. Opens one fresh checkout at `main`'s tip ([Version control](#version-control-jj)) and runs
   `./scripts/install.sh <every target the issues name>` there once, as one blocking eval cell
   ([Building](#building)): it refuses a checkout that differs from `main`, checks signing and
   the deploy lock itself and stops only the installed copy. On Linux it runs
   `scripts/install-linux.py` (its header: what goes where, the launcher, the nix GC root, the
   desktop entry), which builds in `devenv shell` with cargo targets outside the ovnfs mount
   (MS-201). Never an area workspace or the default checkout; never delete another agent's lock
   or kill its processes.
3. Checks each installed `/Applications/…app` from the shell, nothing more: its Info.plist
   `MSInstalledRevision` is the commit it built; `codesign --verify --deep --strict` passes;
   `open` launches it and its `/Applications/…app/Contents/MacOS/` executable is still running
   30 s later (left running). GPUI also: `otool -l` shows no workspace rpath and `vmmap <pid>`
   loads no `libMailstreamKit.dylib`. On Linux, each installed `~/.local/opt` folder: its
   `revision` file is the commit it built, and its launcher (`mailstream-gpui.sh`,
   `mailstream_ui`) started in a `scripts/vhypr.py` desktop of its own (never the user's screen)
   is still running 30 s later; then it stops that desktop. No UI interaction, synthetic data or
   screenshots.
4. Passed → per issue a short comment (deployed commit, targets, checks), `install` and
   `claimed` removed, In Review, then the parent check. Install or launch failed → a comment
   with the exact error on each issue, `claimed` removed and `install` kept.
5. Removes its checkout; leaves every other checkout and workspace alone.

## Performance: 120 fps

The front ends aim at 120 fps. Measure only with `scripts/bench.sh flutter|gpui [RUNS]` on a
release build (setup, scenario and log format in its header and `scripts/frame-stats.swift`). A
run passes when at most 1% of vsyncs drop and the interval p99 is at most 12.5 ms (1.5 frames).
Report 3 runs with their load; work over 8.3 ms shows where time goes. New scenarios get their own
`MAILSTREAM_BENCH` value and the same log format.

## Swift: concurrency and state

macOS 26, Swift 6.2 language mode; `Package.swift` sets `defaultIsolation(MainActor.self)`,
`NonisolatedNonsendingByDefault` and `ExistentialAny`. CI builds with `-warnings-as-errors`.

- Code is main-actor unless it says otherwise. Value types and backends used off the main actor
  are marked `nonisolated` (`nonisolated struct`, `nonisolated protocol MailBackend`).
- Async functions run on their caller's actor. Work that must leave the main actor (network,
  disk, parsing, threading, matching) is `@concurrent`, e.g. `@concurrent private nonisolated
  static func …` in stores. The main thread never waits on I/O.
- No GCD, locks or `Timer`: use `Task(name:)` loops and actors. The one exception is the DNS
  `DispatchQueue` that `DNSServiceSetDispatchQueue` requires in `Avatars.swift`.
- Stores are `@Observable`; the UI follows their properties rather than notifications. Fields the
  UI does not read are `@ObservationIgnored`.
- List rebuilds go through `rebuild.request()` (`CoalescedTask`): requests coalesce and a pass
  whose inputs changed is dropped.
- Failures go to `problems.report(_:from:context:)` (`StoreProblems`), the one problem channel.
- Write existentials as `any Error`, `any MailBackend`.
- Concurrent per-key work (accounts, mailboxes) goes through `forEachConcurrently`
  (`Store/Concurrency.swift`), which consumes each result on the caller's actor as it completes;
  restartable polling is a `PeriodicTask`. Hand-written task groups must bind an awaited value
  before building a tuple (`let hits = try await …; return (key, .success(hits))`): Swift 6.3 at
  `-O` drops elements of `(key, .success(try await …))`.

## Shared layers

Reuse these instead of writing another copy:

- Network: paging is `API.pages`, chunked batch requests with throttled retries are `API.batched`,
  status classification is `API.retry(status:body:)`; provider roots and OAuth scopes live in
  `Backend/Endpoints.swift`.
- Rust engine network (`Engine/src/api.rs`): every HTTP client comes from `api::client` (accounts
  and OAuth share `api::HTTP`); listings page through `api::pages` with `GooglePage`/`GraphPage`;
  batch requests go through `Api::batch`, which sends again what was throttled or unanswered, with
  `multipart_parts` (Gmail) or `json_parts` (Graph `$batch`) reading the answers; queries and forms
  are `api::form_encoded`. OAuth scopes are the constants in `oauth.rs`.
- Engine tests: engine folders, sign-ins, `call`/`wait_until` and the fake HTTP server are in
  `Engine/src/testkit.rs`.
- Disk: mail data (accounts, mailboxes, listings, content, tags, chat) lives in the Turso database
  behind `MailCache` (`MailDatabase`, `Support/SQLite.swift`), in Application Support/Mailstream/mail.db;
  `MailCache` queues its writes. Other JSON files are read and written with `JSONFile`.
- Style: every color, type size, radius, shadow and duration comes from `Design`
  (`UI/Design.swift`). AppKit reads the tokens directly; the pages' CSS uses them as custom
  properties (`var(--muted)`, `var(--size-body)`, `var(--dur-quick)`) from `Design.stylesheet`,
  so native and HTML surfaces match. Add a token rather than a literal; use the nearest step of a
  scale. AppKit boilerplate (labels, sheets, pinning, toolbar buttons) is in `UI/ViewKit.swift`.
