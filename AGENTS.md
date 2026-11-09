# AGENTS.md

## Current focus: Flutter

- Product work, new features and UI acceptance go to `Flutter/` unless the user names another
  front end. The AppKit and GPUI front ends stay (not deprecated) and keep working through
  engine changes, but don't grow their features or port new Flutter features to them.

## Version control: jj

Colocated jj repo (`.jj` + `.git`), pushed to `github.com/taooceros/mailstream` (public).

- Use `jj`, not raw `git`: `jj st`, `jj diff`, `jj describe -m "…"`, `jj new`, `jj log`.
- Each change is its own described commit (`jj describe -m "<what and why>"`, then `jj new`),
  made unprompted as soon as it is built and verified, then pushed: `jj git fetch`, rebase onto
  `main@origin` if it moved, `jj bookmark set main -r @-`, `jj git push`. Never leave finished
  work uncommitted in `@`.
- Every agent works in its own ovnfs checkout, never the default checkout and never
  `jj workspace add` (the `../mailstream-*` workspaces take no new work): `ovnfs new --rev main
  --name <unique>` (it prints the path, under `~/checkouts/`), then `jj git init --colocate &&
  jj bookmark track main@origin` (a checkout has a private `.git` and no `.jj`). It commits,
  pushes `main` and delivers from there ([Release delivery](#release-delivery)), and runs
  `ovnfs rm <name>` once the issue is delivered; a woken agent opens a fresh one. Subagents are
  spawned without omp's `isolated` option. Work the user keeps off `main` (a standalone trial)
  stays in its own existing checkout.
- One line: every front end (the AppKit app, `GPUI/`, `Flutter/`) and the engine they share
  (`Sources/MailstreamKit`, `FFI/`) live on `main`, and every checkout works on it; the
  `gpui-ui` and `flutter-ui` bookmarks are history and take no new commits. An engine change
  updates every front end that calls what it changed, in the same change.
- The repo is public: never commit secrets, tokens, OAuth client files or personal mail data.

## Work tracking: Linear

Team `MS` (the Mailstream app: code in this repo, both machines), split into projects by effort;
issue IDs look like `MS-12`. Team `WIN` holds the Windows machine's own chores (its setup and
tools, nothing in this repo); every `WIN` issue goes to the Windows poller. Use omp's Linear tools.
The issue, not the session, is the record other sessions read.

On the Windows machine (`WIN` issues and `MS` issues labelled `windows`) there are no Linear
tools or keychain. Linear
is reached as the OAuth app `windows-omp`: its client id (line 1) and secret (line 2) are in
`~/.config/linear/credentials`, and the access token in `~/.config/linear/token.json`. Call
`https://api.linear.app/graphql` with `Authorization: Bearer <access_token>`. On an
`AUTHENTICATION_ERROR` (tokens last ~30 days) fetch a new one with
`curl https://api.linear.app/oauth/token -d grant_type=client_credentials -d client_id=… -d
client_secret=… -d scope=read,write` into `token.json`; `linear-poll.py` does this itself. Never
print or commit either file. The app can't create labels; ask the user for new ones.

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
  whoever claims one adds them first (the polling session before handing it out, otherwise the
  agent before its start comment).
- File your own issues in Backlog; only the user starts them (moving one to Todo or adding `omp`).
  Problems found outside the current change get their own Backlog issue; don't fix them in passing.

### Working an issue

- Claim before working: every agent, subagents included, works under an issue it has claimed;
  with none for the work yet, create one and claim it. Work that serves another issue is its
  sub-issue: set Linear's parent field (`parentId`) when creating it; naming the parent only in
  the text does not count.
- Start: read the issue and its comments. Skip issues already In Progress unless the user or the
  polling session hands one over. Move it to In Progress, comment
  `[omp @ <checkout>] start (<agent id>): <plan>` (the subagent/roster id, so the issue shows
  who owns it), then add the `claimed` label. Taking over or resuming counts as starting: post a
  new start comment with your own id. Remove `claimed` when you stop (In Review, Todo with a
  question, Backlog when blocked).
- Progress: record decisions, blockers and dead ends as comments; the next session only sees
  what is on the issue.
- Blocked by another issue (it needs that change first, or the user said to wait for it): add a
  "blocked by" relation (file the blocker first if it doesn't exist), comment what it waits for,
  move it to Backlog and stop. Once every blocker is delivered (In Review, Done or Canceled; the
  user's review is not awaited), the poll hands it out again, in Todo or Backlog, and its agent
  carries on from the comments.
- Questions only the user can decide: don't guess. Assign the issue to the user (taooceros), move
  it to Todo, start the comment with `**❓ 需要你回答：**` followed by the question(s), each
  answerable on its own, and stop.
- Finish: reference the issue in the commit (`Fixes MS-12`, or `Refs MS-12` for partial work),
  integrate it into `main` and deliver it ([Release delivery](#release-delivery)). A commit,
  passing tests or a Debug/test app is not completion. Only after the installed copy passes its
  smoke check, move the issue to In Review and comment what changed, the deployed commit, the
  exact `/Applications` targets, the checks run and what remains; the user moves it to Done.
- UI changes (a new or changed view, flow or interaction, in any front end): attach screenshots
  of the running app showing the change, every state it adds included, to the finishing comment.
  Without them the work is not ready for review.
- Parents: when finishing a sub-issue, check its siblings. Once none is left in Backlog, Todo or
  In Progress, finish the parent too: a summary comment listing each sub-issue and what remains,
  then In Review. Nobody else picks up a parent an agent filed.

### Polling session

An omp session runs `RUNNING_FILE=~/.local/state/linear-poll/running python3 scripts/linear-poll.py watch`
as one background bash job with no deadline (`timeout: 0`) and `wait`s on it: the script checks
Linear every minute and returns only once it has claimed something. The session hands the issues
it claims, and the user's comments, to subagents, then starts it again. It keeps that file a JSON
array of its running agent ids, rewritten whenever one starts or finishes; the script re-reads it
each pass. The script's header says what it claims and how the session picks the agent. Whenever
`ovnfs ls` shows no checkout open (between agent batches, before starting the script again), the
session runs `python3 scripts/warm-build.py` in the default checkout ([Building](#building)).
`python3 scripts/linear-omp.py MS-12` opens one issue in a new Tern tab instead. The scripts are
Python (standard library only) so both machines run the same ones; on Windows the command is
`python`, and the poller also needs `HOST_LABEL=windows LINEAR_TOKEN_FILE=~/.config/linear/token.json
LINEAR_CREDENTIALS=~/.config/linear/credentials`.

Cost rules (MS-157): at most three heavy agents (ones that write code) run at once; other
claimed issues wait in the queue of `~/.local/state/linear-poll/session.md`, moved to Backlog
with a comment saying they are queued, and start in order as slots free. Code is written by the
default model; build, install, smoke checks and screenshots that need no code go to a cheaper
agent. A code agent handing delivery over leaves its checkout open and names it; the delivery
agent installs from that checkout (one of them, for several issues at once) and removes it, since
a fresh checkout builds Flutter and Swift from nothing. Agents write their full report on the
issue and yield only two or three lines (result, commit, issue state, anything the user must do).

The session hands over to a fresh one once its own context passes about 150K tokens: every wake
re-reads the whole conversation. It waits until no agent of its own is running (their results
can't reach another session), writes `session.md` (queued issues in order, what waits on the
user, open benchmarks or checks, anything else the next session needs; running agents are
none by then), empties RUNNING_FILE, opens
`tern new tab --cwd ~/projects/mailstream -- omp "Polling session: read AGENTS.md and
~/.local/state/linear-poll/session.md, then start the poller."` and stops (it does not start
the script again). The new session reads `session.md`, starts the queued issues and the script.

The Mac (app `omp`) and the Windows machine (app `windows-omp`) talk only through Linear. New
work for the other machine is an issue (a sub-issue when it serves one) labelled `omp`, plus
`windows` for Windows (or in team `WIN` when it is a chore on that machine, not repo work): its
poller claims it whoever created it. A comment naming a machine's app
reaches that machine alone on any issue, from the user or the other machine; agents write the
name in backticks (`` `@omp` ``), since Linear rejects an app's @-mention of an app.

## Building

- Rust builds (`Engine/`, `FFI/`, `GPUI/`, Turso) run plain cargo: the repo's
  `.cargo/config.toml` turns off the rcache wrapper in `~/.cargo/config.toml`, which handed out
  corrupt artifacts in ovnfs checkouts (MS-131). Don't set `RUSTC_WRAPPER` to bring it back. An
  ovnfs checkout reads the default checkout's ignored build folders (`target/`, `.turso/`) live,
  not as a copy, until it writes a file there; cargo's fingerprints rebuild what differs when the
  checkout opens. A build or delete in the default checkout afterwards shows up in every open
  checkout, and cargo there can then keep an artifact built from the default checkout's source
  (MS-152).
- The default checkout keeps a warm build of `main` (`FFI/target` release, `Engine/target`
  debug for the engine's tests, `.turso/`) that every new checkout starts from: its first release
  `ffi.py` rebuilds only the repo's 2 crates (~25 s) instead of 460 (~3–4 min) (MS-151, MS-159).
  `ffi.py` builds the library and the command line in one cargo run into `FFI/target` and drops
  `SDKROOT`/`CPATH`-style variables: C build scripts rebuild everything above them when those
  differ from the warm build's. Run release engine builds through `ffi.py`, not a second cargo in
  `Engine/` (that compiles the engine and its dependencies again). Never delete the default
  checkout's build folders to free disk; free your own checkout's output instead (delete it
  there, or `ovnfs rm` the checkout once done). Only `python3 scripts/warm-build.py` builds
  there: it fetches `main`, moves the default checkout's empty change onto it and builds, and
  refuses while any checkout of it is open (MS-152). Nothing builds Swift there, so a checkout's
  `.build` is always its own. Don't share one `CARGO_TARGET_DIR` between checkouts: cargo locks
  it for one build at a time and each checkout's source rebuilds the repo's crates over the
  others'.
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

Finished fixes go to `/Applications` as part of the work, as signed release builds, never Debug.
Separately, each push to `main` that touches the Flutter app or its engine publishes a signed
update without approval, which the installed Mailstream Flutter installs by itself
(`Flutter/README.md`); the issue's own install below is still required, so it is verified now.

1. Targets: `flutter` (`Mailstream Flutter.app`) by default. Add `native` (`Mailstream.app`) or
   `gpui` (`Mailstream GPUI.app`) only when the user asks or its installed behavior is directly
   affected; never install an app that isn't installed yet. Tooling-only changes install
   nothing; changes to deployment itself exercise the real path.
2. Push the issue-referencing commit to live `main` without losing other agents' changes, then
   build from your own checkout at that `main`: `jj git fetch && jj new main` there (never an
   area workspace or the default checkout). It already holds your incremental builds (cargo,
   Flutter, Swift); a fresh checkout would rebuild Flutter and Swift from nothing. Open a fresh
   one (`ovnfs new --rev main --name <unique-build-name>`, jj set up as in
   [Version control](#version-control-jj)) only when you have no checkout with the work, and
   take over a code agent's when it hands one to you. There run
   `./scripts/install.sh <targets>`: it refuses a checkout that differs from `main`, checks
   signing and the deploy lock itself and stops only the installed copy. If `main` moves
   meanwhile, `jj new main` again and rebuild from its tip. Never delete another agent's lock or
   kill its processes.
3. Smoke each installed `/Applications/…app` with computer helpers: the running executable, the
   bundle revision, `codesign --verify --deep --strict`, the real window showing loaded data, and
   the affected behavior (no sending mail, nothing destructive). GPUI also: `otool -l` shows no
   workspace rpath and `vmmap <pid>` loads no `libMailstreamKit.dylib` (the engine is linked in). A build, an
   `open` or a process name alone proves nothing. Keep personal content out of screenshots and
   logs.
4. Record the evidence you gathered, or the exact blocker, on the issue; never claim another
   issue's pending checks passed. Then `ovnfs rm` your checkout; leave every other checkout and
   workspace alone.

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
- Disk: mail data (accounts, mailboxes, listings, content, tags, chat) lives in the Turso database
  behind `MailCache` (`MailDatabase`, `Support/SQLite.swift`), in Application Support/Mailstream/mail.db;
  `MailCache` queues its writes. Other JSON files are read and written with `JSONFile`.
- Style: every color, type size, radius, shadow and duration comes from `Design`
  (`UI/Design.swift`). AppKit reads the tokens directly; the pages' CSS uses them as custom
  properties (`var(--muted)`, `var(--size-body)`, `var(--dur-quick)`) from `Design.stylesheet`,
  so native and HTML surfaces match. Add a token rather than a literal; use the nearest step of a
  scale. AppKit boilerplate (labels, sheets, pinning, toolbar buttons) is in `UI/ViewKit.swift`.
