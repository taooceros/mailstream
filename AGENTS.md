# AGENTS.md

## Current focus: Flutter

- Product work, new features and UI acceptance go to `Flutter/` unless the user names another
  front end; automatic release delivery targets the installed Flutter app.
- The AppKit and GPUI front ends stay; they are not deprecated. Don't grow their features or
  turn Flutter work into a cross-front-end rewrite.
- Engine/FFI changes keep them working: update affected callers and run targeted compatibility
  checks. Porting new Flutter features or a full native/GPUI release and acceptance pass is not
  required.

## Version control: jj

Colocated jj repo (`.jj` + `.git`), pushed to `github.com/taooceros/mailstream` (public).

- Use `jj`, not raw `git`: `jj st`, `jj diff`, `jj describe -m "…"`, `jj new`, `jj log`.
- Each change is its own described commit (`jj describe -m "<what and why>"`, then `jj new`),
  made unprompted as soon as it is built and verified, then pushed: `jj bookmark set main -r @-`,
  then `jj git push`. Never leave finished work uncommitted in `@`.
- One line: every front end (the AppKit app, `GPUI/`, `Flutter/`) and the engine they share
  (`Sources/MailstreamKit`, `FFI/`) live on `main`, and every workspace works on it; the
  `gpui-ui` and `flutter-ui` bookmarks are history and take no new commits. An engine change
  updates every front end that calls what it changed, in the same change.
- The repo is public: never commit secrets, tokens, OAuth client files or personal mail data.

## Work tracking: Linear

Team `MS`, split into projects by effort; issue IDs look like `MS-12`. Use omp's Linear tools.
The issue, not the session, is the record other sessions read.

### Writing

- Write everything on Linear (titles, descriptions, comments) in plain Chinese that someone who
  has not read the code understands at once. Lead with the conclusion, then what happened, how it
  affects the user, what was done and what remains. Explain any technical term the reader needs
  in a few words; leave out terms and long logs they don't.
- No defensive writing: don't hedge, don't repeat disclaimers such as "未授权", "不代表" or
  "不算通过", and own mistakes directly. Keep real unknowns and risks as concrete facts; never
  present something uncertain or unverified as done.
- Code identifiers, paths, commands and commit hashes stay as they are.

### Filing

- Labels: one type (`Bug`, `Feature`, `Improvement`, `Performance`, `Regression`) plus one `Area`
  (Linear allows one; for work across areas, the one most of it is in). Areas match the jj
  workspaces (`chat`, `concurrency`, `flutter`, `gpui`, `motion`, `ui-shell`) or the layer
  (`sync`, `cache`, `ffi`); an issue is worked in its area's workspace. Repo tooling (AGENTS.md,
  scripts, CI, build setup) takes no Area and is worked on `main`.
- Project: the team project whose description the issue fits. When none fits, create one (team
  `MS`, short Chinese name, one-line scope description, lead taooceros). Sub-issues go in their
  parent's project.
- Label and place issues you file when creating them. Issues the user files often have neither:
  whoever claims one adds them first (the polling session before handing it out, so it reaches
  the area's workspace; otherwise the agent, before its start comment).
- File your own issues in Backlog; only the user adds `omp`. Problems found outside the current
  change get their own Backlog issue; don't fix them in passing.

### Working an issue

- Claim before working: every agent, subagents included, works under an issue it has claimed;
  with none for the work yet, create one and claim it. Work that serves another issue is its
  sub-issue: set Linear's parent field (`parentId`) when creating it; naming the parent only in
  the text does not count.
- Start: read the issue and its comments. Skip issues already In Progress unless the user or the
  polling session hands one over. Move it to In Progress, comment
  `[omp @ <workspace>] start (<agent id>): <plan>` (the subagent/roster id, so the issue shows
  who owns it), then add the `claimed` label. Taking over or resuming counts as starting: post a
  new start comment with your own id. Remove `claimed` when you stop (In Review, Todo with a
  question, Backlog when blocked).
- Progress: record decisions, blockers and dead ends as comments; the next session only sees
  what is on the issue.
- Blocked by another issue (it needs that change first, or the user said to wait for it): add a
  "blocked by" relation (file the blocker first if it doesn't exist), comment what it waits for,
  move it to Backlog so the workspace frees up, and stop. Once every blocker is In Review, Done
  or Canceled, the poll hands it out again and its agent carries on from the comments.
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

- An omp session runs `scripts/linear-poll.sh` every minute and hands each issue it claims to a
  subagent in the area's workspace; when another issue holds that workspace, the issue goes to
  the subagent working related code, or to a private workspace. `scripts/linear-omp.sh MS-12`
  opens one issue in a new Tern tab instead (Linear's "Work on issue → Custom script").
- It claims: Todo issues the user created, once each, unless a blocker is still open; issues it
  claimed before, once all their blockers are resolved; any issue labelled `omp` (then removes
  the label). Every comment the user posts reaches the subagent working that issue, or starts
  one.
- An In Progress issue still `claimed` by an agent that is no longer running is reported stale;
  the session wakes that agent or hands the issue to a new one. `claimed` is dropped from issues
  out of In Progress whose agent has stopped.
- Who gets an issue: reuse an agent already started over a new one when it has related context
  and room left. Related, in order: it worked this issue before (resumed, or a user comment on
  it); it worked the parent, a sub-issue, a blocker or a blocked issue; it worked the same files
  in the same workspace. Room left: context under 400K tokens (40% of the 1M window, the `ctx`
  figure the session sees); at or over that, start a new agent, which picks up from the issue's
  comments. An agent still running another issue gets the new one only after it finishes,
  unless it is the same issue. A reused agent claims the issue like any other: its own start
  comment, In Progress, `claimed`, its own workspace.

## Building

- Every Rust build (cargo here, in `FFI/`, `GPUI/` and Turso) goes through rcache
  (`~/projects/rcache`), the copy-on-write rustc cache shared across jj workspaces:
  `~/.cargo/config.toml` sets `build.rustc-wrapper` to it, and `scripts/ffi.sh`/`turso.sh` export
  `RUSTC_WRAPPER=rcache`. Don't set `RUSTC_WRAPPER`/`RCACHE_DISABLE` to bypass it; `rcache stats`
  shows hits.
- The app links the Turso database engine (`.turso/lib/libturso_sqlite3.a`). Run `scripts/turso.sh`
  once before `swift build` (it needs rustup; `bundle.sh` runs it too). It builds the pinned
  version with cargo in a few minutes and returns at once after that.
- Check with `swift build` and launch with `scripts/run.sh`: both use the incremental debug build
  (seconds per edit). A release build (`scripts/bundle.sh`, `CONFIG=release`) re-optimizes the
  whole module single-threaded, ~30 s per edit; build it only to check performance or ship.
- All Swift code is in the `MailstreamKit` target (`Sources/MailstreamKit`); the `Mailstream`
  executable only calls `MailstreamApp.main()`. `MailstreamKit` is also the dynamic library FFI
  front ends load: `scripts/ffi.sh` builds it with the UniFFI crate in `FFI/`. New commands go in
  `CommandSpec`/`ControlService`, then get a typed method in `FFI/src/lib.rs`; GPUI uses that
  crate directly, Flutter through the Dart binding `ffi.sh` generates (`Flutter/README.md`).
- The engine keeps the state every front end shares (the open chat space and its history, as for
  the native reader); front ends re-read it on the engine's change events.
- Test only what you changed: build, then exercise the parts your change touches (the view, flow
  or command you edited). Don't re-run the whole app checklist or every CLI case after each
  change; we iterate in small steps.

## Release delivery

Finished fixes go to `/Applications` as part of the work, without a separate request, always as
signed release builds, never Debug.

1. Targets: the installed Flutter app (`Mailstream Flutter.app`) by default. Add `native`
   (`Mailstream.app`) or `gpui` (`Mailstream GPUI.app`) only when the user asks or its installed
   behavior is directly affected and needs verifying; a shared engine/FFI change alone does not
   call for a three-front-end release. Don't install apps that aren't installed yet.
   Tooling-only changes normally replace no app; changes to deployment itself exercise the real
   path, Flutter first. Never claim another issue's pending checks passed.
2. After targeted checks, describe the issue-referencing commit, `jj new`, integrate it into live
   `main` without losing other agents' changes, and push. Read `skill://mailstream-install-frontend`
   when available. Build in a fresh private workspace:
   `jj workspace add --name <unique-build-name> -r main <unused-path>`. Never move an area's `@`
   or build from the stale default checkout or the old front-end bookmarks. Don't copy `target/`
   or `.build/` from another workspace; keep rcache on.
3. There, run `./scripts/install.sh flutter`, plus `native`/`gpui` per step 1. It refuses stale or
   modified source and targets not installed, builds release with the existing scripts, records
   `MSInstalledRevision` in the bundle, requires the shared Apple Development signing (team
   `K7HG6KZNNP`, identifier `dev.mailstream.Mailstream`), verifies the signature and replaces only
   those bundles. If `main` moves during the build, rebuild from its new tip. It terminates only
   the matching `/Applications` executable; never kill another agent's Debug, `/tmp`, test or
   workspace process (no broad `pkill`/`killall`). `/tmp/mailstream-release-install.lock`
   serializes deployments: coordinate with its owner rather than deleting it or overwriting a
   newer installed release.
4. Smoke the installed copy: launch each exact `/Applications/…app` with computer helpers; check
   the running executable, the bundle revision and `codesign --verify --deep --strict`; check the
   real window shows loaded UI and engine data and exercise the affected behavior, without sending
   mail or destructive actions. For GPUI also check `otool -l` shows no workspace rpath and
   `vmmap <pid>` loads `libMailstreamKit.dylib` from the installed bundle. A build, a successful
   `open` or a process name alone proves nothing. Keep personal content out of public screenshots
   and logs.
5. Record the actual evidence on the issue, not a checklist; if install, signing, launch or smoke
   fails, record the exact blocker instead of success. Only after the smoke checks pass, forget
   and remove your build workspace (with its retained previous bundles); leave every pre-existing
   workspace and artifact alone.

## Performance: 120 fps

The front ends aim at 120 fps. Measure frame rate one way, so numbers compare across front ends
and changes: `scripts/bench.sh flutter|gpui [RUNS]` on a release build.

- Setup: the window (1240×800) is parked on a private 120 Hz virtual display (`scripts/vdisplay.m`;
  the front ends draw at the rate of the display they are on, so a 60 Hz display caps them at
  60). Each run waits for the 1-minute load average to fall below 4 (other workspaces' builds
  skew frame times) and records it.
- Scenario `scroll` (`MAILSTREAM_BENCH=scroll`, log to `MAILSTREAM_BENCH_OUT`): 12 s after launch,
  once the list has rows, the app scrolls its conversation list itself, setting the offset each
  frame from the time: 8 legs of 3 s, down 4000 px and back (less while the list is shorter;
  pages load as in normal use). It logs every frame, then quits.
- Per frame: start (Flutter: `FrameTiming` vsync start; GPUI: the window's render on the display
  link tick) and work, the longest single-thread stage (Flutter: build or raster; GPUI: render
  to the end of paint on the main thread).
- `scripts/frame-stats.swift` reports fps, dropped vsyncs, the interval p50/p95/p99/max and work
  percentiles. A run passes at 120 Hz when at most 1% of vsyncs are dropped and the interval p99
  is at most 12.5 ms (1.5 frames). Report 3 runs with their load; work over 8.3 ms shows where
  time goes.
- Other scenarios get their own `MAILSTREAM_BENCH` value and the same log format.

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
