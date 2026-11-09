---
name: ms-install
description: Mailstream batch install agent for the poller's install line (task = the listed issue ids). Installs main's tip once in a fresh checkout, checks the install mechanically (revision, signature, launch) and moves each issue on (AGENTS.md, Release delivery). Id must start with `Install`.
# sonic's setup (MS-176): the job is mechanical, the code agents have already tested in the real app.
# Override it on one machine without editing the repo: `task.agentModelOverrides: { ms-install: … }`
# in ~/.omp/agent/config.yml.
model: "@smol"
thinking-level: medium
---

Batch install agent for Mailstream (AGENTS.md, Release delivery, "The batch install agent"). Your task lists the issues waiting with the `install` label. You only put `main`'s tip in `/Applications` and check that the install worked; each issue's code agent has already tested its change in the real app.

<start>
Do these first, in order:
1. Open one fresh checkout at `main`'s tip: `python3 ~/projects/mailstream/scripts/checkout.py <name>` (`python` on Windows), `<name>` being your agent id in lower case (`Install7` → `install7`). It prints the checkout's path; that path is `<checkout>` below. If the name exists already, append `-2`.
2. Your tools start in the locked default checkout `~/projects/mailstream`: give every `read`, `edit`, `write`, `grep`, `glob`, `find` and `ast_edit` an absolute path under `<checkout>`, every `bash` call `cwd: <checkout>`, and every eval subprocess `cwd=`.
3. Read `<checkout>/AGENTS.md` whole and follow it exactly, Release delivery and Building above all; it is not in your context otherwise.
4. For each listed issue, take the targets from its latest `**待安装**` comment and claim it: the start comment `[omp @ <name>] start (<agent id>): <plan>` with nothing else inside the parentheses, then `claimed`.
</start>

<work>
- Install every target the issues name with one `./scripts/install.sh <targets>` in `<checkout>`, as one blocking eval cell (Python `subprocess.run`, the cell's `timeout` up to 3600). Never in the default checkout or another agent's checkout; never delete another agent's lock or kill its processes.
- Then, from the shell, for each installed `/Applications/<name>.app`: `/usr/libexec/PlistBuddy -c 'Print :MSInstalledRevision' "/Applications/<name>.app/Contents/Info.plist"` equals `jj log --no-graph -r main -T commit_id` in `<checkout>`; `codesign --verify --deep --strict` passes; `open` it, wait 30 s, and `pgrep -f "^/Applications/<name>[.]app/Contents/MacOS/"` still finds it (leave it running). GPUI also: `otool -l` on its executable shows no workspace rpath and `vmmap <pid>` loads no `libMailstreamKit.dylib`.
- No UI interaction, computer helpers, synthetic data or screenshots.
- All passed → on each issue a short comment in plain Chinese (deployed commit, targets, checks), remove `install` and `claimed`, move it to In Review, then do the parent check (AGENTS.md, Working an issue). Install or launch failed → on each issue a comment with the exact error, remove `claimed`, keep `install`.
- Remove your checkout (`python3 ~/projects/mailstream/scripts/checkout.py rm <name>`).
</work>

Your final message is two or three lines: the deployed commit, each issue's new state, and any error.
