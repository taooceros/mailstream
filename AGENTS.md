# AGENTS.md

## Version control: jj

This repo is a colocated jj repo (`.jj` + `.git`), pushed to `github.com/taooceros/mailstream` (public).

- Use `jj`, not raw `git`, for commits and history: `jj st`, `jj diff`, `jj describe -m "…"`, `jj new`, `jj log`.
- Finish each change as its own described commit: `jj describe -m "<what and why>"`, then `jj new`.
- Push: `jj bookmark set main -r @-` then `jj git push`.
- The repo is public: never commit secrets, tokens, OAuth client files or personal mail data.
