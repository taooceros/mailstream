# AGENTS.md

## Version control: jj

This repo is a colocated jj repo (`.jj` + `.git`), pushed to `github.com/taooceros/mailstream` (public).

- Use `jj`, not raw `git`, for commits and history: `jj st`, `jj diff`, `jj describe -m "…"`, `jj new`, `jj log`.
- Finish each change as its own described commit: `jj describe -m "<what and why>"`, then `jj new`.
  Do it unprompted as soon as a change is built and verified, then push; never leave finished work
  uncommitted in `@`.
- Push: `jj bookmark set main -r @-` then `jj git push`.
- The repo is public: never commit secrets, tokens, OAuth client files or personal mail data.

## Work tracking: Linear

Work is tracked in Linear: team `MS`, project `Mailstream`. Issue IDs look like `MS-12`. Use
omp's Linear tools; the issue, not the session, is the record other sessions read.

- Language: write everything on Linear (issue titles, descriptions, comments) in Chinese, plainly
  enough that someone who has not read the code understands what is wrong, what was done and what
  remains. Code identifiers, paths, commands and commit hashes stay as they are.
- Labels: one type (`Bug`, `Feature`, `Improvement`, `Performance`, `Regression`) plus one `Area`
  label. Area names match the jj workspaces (`chat`, `concurrency`, `flutter`, `gpui`, `motion`,
  `ui-shell`) or the layer (`sync`, `cache`, `ffi`); an issue is worked in its area's workspace.
- Claim before working: every agent, subagents included, works under an issue it has claimed.
  With no issue for the work yet, create one first and claim it. Work that serves another issue
  is a sub-issue of it: set Linear's parent field (`parentId`) when creating it, so Linear shows
  it as a sub-issue; naming the parent only in the text does not count. The start comment names
  the agent doing the work (its subagent/roster id), so each issue shows which subagent owns it.
- Start: read the issue and its comments. Skip issues already In Progress unless the user hands
  one over. Move it to In Progress and comment `[omp @ <workspace>] start (<agent id>): <plan>`.
- Progress: record decisions, blockers and dead ends as comments; the next session only sees
  what is on the issue.
- Finish: reference the issue in the commit (`Fixes MS-12`, or `Refs MS-12` for partial work),
  move it to In Review and comment what changed, how it was verified and what remains. The user
  moves it to Done. If the issue has a parent, check its other sub-issues: when none is left in
  Backlog, Todo or In Progress, finish the parent too (a summary comment listing each sub-issue
  and what remains, then In Review). Nobody else picks up a parent an agent filed.
- UI changes (a new or changed view, flow or interaction, in any front end): take screenshots
  of the running app showing the change, every state it adds included, and attach them to the
  finishing comment. Work without them is not ready for review.
- Questions only the user can decide: move the issue to Todo and ask in a comment instead of
  guessing.
- Problems found outside the current change: file a Backlog issue with labels; do not fix them
  in passing.
- An omp session polls `scripts/linear-poll.sh` every minute and hands each issue it claims to a
  subagent working in the area's workspace; when another issue holds that workspace, the issue
  goes to the subagent working related code, or to a private workspace. It claims Todo
  issues the user created, once each, and any issue labelled `omp` (then removes the label). Every
  comment the user posts reaches the subagent working that issue, or starts one. Only the user
  adds `omp`; file your own issues in Backlog. `scripts/linear-omp.sh MS-12` opens one issue in a
  new Tern tab instead (Linear's "Work on issue → Custom script").

## Building

- The app links the Turso database engine (`.turso/lib/libturso_sqlite3.a`). Run `scripts/turso.sh`
  once before `swift build` (it needs rustup; `bundle.sh` runs it too). It builds the pinned version
  with cargo in a few minutes and returns at once after that.
- Check with `swift build` and launch with `scripts/run.sh`: both use the incremental debug build
  (seconds per edit). A release build (`scripts/bundle.sh`, `CONFIG=release`) re-optimizes the
  whole module single-threaded, ~30 s per edit; build it only to check performance or ship.
- Deploying to `/Applications/Mailstream.app` is always a release build: `CONFIG=release
  ./scripts/bundle.sh`, then replace the bundle (the running copy keeps working until relaunched).
- All Swift code is in the `MailstreamKit` target (`Sources/MailstreamKit`); the `Mailstream`
  executable only calls `MailstreamApp.main()`. `MailstreamKit` is also the dynamic library FFI
  front ends load: `scripts/ffi.sh` builds it with the UniFFI crate in `FFI/`. New commands go in
  `CommandSpec`/`ControlService`, then get a typed method in `FFI/src/lib.rs`.
- Test only what you changed: build, then exercise the parts your change touches (the view, flow or command you edited). Do not re-run the whole app checklist or every CLI case after each change; we iterate in small steps.

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
