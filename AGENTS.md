# AGENTS.md

## Current product focus: Flutter

- For now, Flutter is the focus for product work, active feature development and UI acceptance.
  Unless the user specifies another front end, implement and demonstrate the requested behavior
  in `Flutter/`; default automatic release delivery targets the installed Flutter app.
- Keep the native Swift/AppKit and GPUI front ends; they are not deprecated or being removed.
  Do not proactively expand their features or turn Flutter work into a cross-front-end rewrite.
- Keep necessary compatibility when changing the shared backend/FFI: update affected callers
  and run targeted compatibility checks where needed. This does not require duplicating new
  Flutter features or doing a full native/GPUI release and UI acceptance pass for every change.

## Version control: jj

This repo is a colocated jj repo (`.jj` + `.git`), pushed to `github.com/taooceros/mailstream` (public).

- Use `jj`, not raw `git`, for commits and history: `jj st`, `jj diff`, `jj describe -m "…"`, `jj new`, `jj log`.
- Finish each change as its own described commit: `jj describe -m "<what and why>"`, then `jj new`.
  Do it unprompted as soon as a change is built and verified, then push; never leave finished work
  uncommitted in `@`.
- Push: `jj bookmark set main -r @-` then `jj git push`.
- One line: every front end (the AppKit app, `GPUI/`, `Flutter/`) and the engine they share
  live on `main`. Every workspace works on `main`; the old `gpui-ui` and `flutter-ui` bookmarks
  are history and take no new commits. A change to the engine (`Sources/MailstreamKit`, `FFI/`)
  updates every front end that calls what it changed, in the same change.
- The repo is public: never commit secrets, tokens, OAuth client files or personal mail data.

## Work tracking: Linear

Work is tracked in Linear: team `MS`, split into projects by effort. Issue IDs look like
`MS-12`. Use omp's Linear tools; the issue, not the session, is the record other sessions read.

- Language: write everything on Linear (issue titles, descriptions, comments) in Chinese, plainly
  enough that someone who has not read the code understands what is wrong, what was done and what
  remains. Code identifiers, paths, commands and commit hashes stay as they are.
- Labels: every issue carries one type (`Bug`, `Feature`, `Improvement`, `Performance`,
  `Regression`) plus one `Area` label (Linear allows one; for work across areas, the one most of
  it is in). Area names match the jj workspaces (`chat`, `concurrency`, `flutter`, `gpui`,
  `motion`, `ui-shell`) or the layer (`sync`, `cache`, `ffi`); an issue is worked in its area's
  workspace. Issues about the repo's own tooling (AGENTS.md, scripts, CI, build setup) take no
  Area label and are worked on main. Label issues you file when creating them. Issues the user
  files often have none: whoever claims one adds the missing labels first (the polling session
  before handing it out, so it goes to the area's workspace; otherwise the agent, before its
  start comment).
- Projects: every issue belongs to the team's project whose scope (its description) it fits,
  set together with the labels. When none fits, create a new project: team `MS`, a short
  Chinese name, a one-line description of its scope, lead taooceros; then put the issue in it.
  Sub-issues go in their parent's project.
- Claim before working: every agent, subagents included, works under an issue it has claimed.
  With no issue for the work yet, create one first and claim it. Work that serves another issue
  is a sub-issue of it: set Linear's parent field (`parentId`) when creating it, so Linear shows
  it as a sub-issue; naming the parent only in the text does not count. The start comment names
  the agent doing the work (its subagent/roster id), so each issue shows which subagent owns it.
- Start: read the issue and its comments. Skip issues already In Progress unless the user or the
  polling session hands one over. Move it to In Progress, comment
  `[omp @ <workspace>] start (<agent id>): <plan>`, then add the `claimed` label: it says an
  agent is on the issue now, and the start comment says which. Taking over or resuming an issue
  counts as starting it: post a new start comment with your own id. Remove `claimed` when you
  stop (In Review, Todo with a question, Backlog when blocked).
- Progress: record decisions, blockers and dead ends as comments; the next session only sees
  what is on the issue.
- Blocked by another issue (it needs that issue's change first, or the user said to wait for
  it): mark it on Linear with a "blocked by" relation to that issue (file that issue first if
  there is none), comment what it waits for, move it out of In Progress (to Backlog) so the
  workspace frees up, and stop. When every blocker reaches In Review, Done or Canceled, the poll
  hands the issue out again and its agent carries on from the comments.
- Finish: reference the issue in the commit (`Fixes MS-12`, or `Refs MS-12` for partial work).
  After targeted verification, commit and integrate into current `main`, then install affected
  already-installed front ends as signed releases using the delivery procedure below. A commit,
  passing tests, or a Debug/test app alone is not completion. Only after installed-copy smoke
  verification, move the issue to In Review and comment the changes, deployed commit, exact
  `/Applications` targets, checks and remaining limitations; the user moves it to Done. If the
  issue has a parent, check its other sub-issues: when none is left in Backlog, Todo or In
  Progress, finish the parent too (a summary listing each sub-issue and what remains, then In
  Review). Nobody else picks up a parent an agent filed.
- UI changes (a new or changed view, flow or interaction, in any front end): take screenshots
  of the running app showing the change, every state it adds included, and attach them to the
  finishing comment. Work without them is not ready for review.
- Questions only the user can decide: ask instead of guessing, and make the question impossible
  to miss: assign the issue to the user (taooceros), move it to Todo, and start the comment with
  `**❓ 需要你回答：**` followed by the question(s), each answerable on its own. Then stop.
- Problems found outside the current change: file a Backlog issue with labels; do not fix them
  in passing.
- An omp session polls `scripts/linear-poll.sh` every minute and hands each issue it claims to a
  subagent working in the area's workspace; when another issue holds that workspace, the issue
  goes to the subagent working related code, or to a private workspace. It claims Todo issues the
  user created, once each, unless a blocker is still open; issues it claimed before, once all
  their blockers are resolved; and any issue labelled `omp` (then removes the label). An In
  Progress issue still `claimed` by an agent that is no longer running is reported stale, and the
  session wakes that agent or hands the issue to a new one; `claimed` is dropped from issues out
  of In Progress whose agent has stopped. Every comment the user posts reaches the subagent
  working that issue, or starts one. Only the user adds `omp`; file your own issues in Backlog.
  `scripts/linear-omp.sh MS-12` opens one issue in a new Tern tab instead (Linear's "Work on
  issue → Custom script").
- Who gets an issue: the polling session reuses an agent it already started over starting a new
  one, whenever that agent has related context and room left. Related, in this order: it worked
  this issue before (resumed, or a user comment on it); it worked the parent, a sub-issue, a
  blocker or a blocked issue; it worked the same files in the same workspace. Room left: its
  context is under 400K tokens (40% of the 1M window, the `ctx` figure the session sees).
  An agent still running another issue gets the new one only after it finishes, unless it is
  the same issue. At or over 400K, start a new agent instead; it picks up from the issue's
  comments, as after a session restart. A reused agent claims the new issue like any other: its
  own start comment, In Progress, `claimed`, working in its own workspace.

## Building

- Every Rust build (cargo here, in FFI/, GPUI/, and Turso) goes through rcache (`~/projects/rcache`),
  the copy-on-write rustc cache shared across jj workspaces: `~/.cargo/config.toml` sets
  `build.rustc-wrapper` to it, and `scripts/ffi.sh`/`turso.sh` export `RUSTC_WRAPPER=rcache`.
  Don't set `RUSTC_WRAPPER`/`RCACHE_DISABLE` to bypass it; `rcache stats` shows hits.
- The app links the Turso database engine (`.turso/lib/libturso_sqlite3.a`). Run `scripts/turso.sh`
  once before `swift build` (it needs rustup; `bundle.sh` runs it too). It builds the pinned version
  with cargo in a few minutes and returns at once after that.
- Check with `swift build` and launch with `scripts/run.sh`: both use the incremental debug build
  (seconds per edit). A release build (`scripts/bundle.sh`, `CONFIG=release`) re-optimizes the
  whole module single-threaded, ~30 s per edit; build it only to check performance or ship.
- Finished fixes are delivered to `/Applications` automatically as part of the agent's work,
  not only on a separate request. Use the release delivery procedure below; never deploy Debug.
- All Swift code is in the `MailstreamKit` target (`Sources/MailstreamKit`); the `Mailstream`
  executable only calls `MailstreamApp.main()`. `MailstreamKit` is also the dynamic library FFI
  front ends load: `scripts/ffi.sh` builds it with the UniFFI crate in `FFI/`. New commands go in
  `CommandSpec`/`ControlService`, then get a typed method in `FFI/src/lib.rs`; GPUI uses that
  crate directly, Flutter through the Dart binding `ffi.sh` generates (`Flutter/README.md`). The
  engine keeps the state every front end shares (the open chat space and its history, as for
  the native reader); front ends re-read it on the engine's change events.
- Test only what you changed: build, then exercise the parts your change touches (the view, flow or command you edited). Do not re-run the whole app checklist or every CLI case after each change; we iterate in small steps.

### Release delivery after verification

1. Default to the installed Flutter app for automatic delivery. Determine any additional targets
   from the user's request and actual affected code/callers: `flutter` = `Mailstream Flutter.app`,
   `native` = `Mailstream.app`, `gpui` = `Mailstream GPUI.app` in `/Applications`. Shared backend/FFI
   changes must retain necessary native/GPUI compatibility, but do not trigger an unrelated full
   three-front-end release: install another front end only when its installed behavior is directly
   affected and needs verification, or the user requests it. Do not install unrelated/new apps.
   Tooling-only rules changes normally need no app replacement; changes to deployment mechanics
   must exercise the real path, prioritizing Flutter. Do not claim another issue's pending checks
   passed.
2. After targeted tests/smoke checks, describe the issue-referencing commit, `jj new`, integrate
   it into live `main` without losing other agents' changes, and push. Read
   `skill://mailstream-install-frontend` when available. Create a uniquely named private build
   workspace with `jj workspace add --name <unique-build-name> -r main <unused-path>`. Never
   move an area's `@` or build from the stale default checkout/old front-end bookmarks. Do not
   copy `target/` or `.build/` from another workspace; keep rcache enabled.
3. In that fresh workspace run `./scripts/install.sh flutter` by default; add `native` or `gpui`
   only for the additional affected targets identified above. It refuses
   stale/modified source and missing installed targets, builds release using the existing
   scripts, records `MSInstalledRevision` in the bundle, requires shared Apple Development
   signing (team `K7HG6KZNNP`, signing identifier `dev.mailstream.Mailstream`), verifies the
   signature, and replaces only those installed bundles. If main advances during the build,
   rebuild from its new tip. It only terminates the matching `/Applications` executable, never
   another agent's Debug, `/tmp`, test or workspace process; do not use broad `pkill`/`killall`.
   Deployments are serialized by `/tmp/mailstream-release-install.lock`; coordinate with its
   owner rather than deleting another agent's lock or overwriting a newer installed release.
4. Launch each exact `/Applications/...app` with computer helpers. Verify the running executable,
   bundle revision and `codesign --verify --deep --strict`; inspect the real window for loaded
   UI/engine data, and exercise the affected behavior without sending mail or destructive actions.
   For GPUI also check `otool -l` has no workspace rpath and `vmmap <pid>` loads
   `libMailstreamKit.dylib` from the installed bundle. Keep personal content out of public
   screenshots/logs. A build, `open` success or a process name alone is not UI/engine proof.
5. Record actual evidence on the issue, not an unperformed checklist. If installation, signing,
   launch or smoke verification fails, record the exact blocker rather than reporting success.
   Only after successful smoke checks, forget and remove your own temporary build workspace
   (including its retained previous bundles); preserve all pre-existing workspaces/artifacts.


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
