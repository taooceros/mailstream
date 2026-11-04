# AGENTS.md

## Version control: jj

This repo is a colocated jj repo (`.jj` + `.git`), pushed to `github.com/taooceros/mailstream` (public).

- Use `jj`, not raw `git`, for commits and history: `jj st`, `jj diff`, `jj describe -m "…"`, `jj new`, `jj log`.
- Finish each change as its own described commit: `jj describe -m "<what and why>"`, then `jj new`.
  Do it unprompted as soon as a change is built and verified, then push; never leave finished work
  uncommitted in `@`.
- Push: `jj bookmark set main -r @-` then `jj git push`.
- The repo is public: never commit secrets, tokens, OAuth client files or personal mail data.

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
- In task groups, bind an awaited value before building a tuple
  (`let hits = try await …; return (key, .success(hits))`): Swift 6.3 at `-O` drops elements of
  `(key, .success(try await …))`.
