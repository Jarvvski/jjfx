# jjfx does not declare its own check, so its own loop cannot verify work

Status: resolved
Type: task
Blocked by: none

## Problem

`harness/02` made a local Ticket resolve only when its Run carries a passing
repository check, and made a repository with **no check configured fail closed**
- it never marks work done on the agent's word alone. That is the right default.

The consequence for this repository is that **jjfx cannot run its own loop**:
there is no `.jjfx/config.toml`, so no `[checks]` is declared, so a Ticket
dispatched in this repository would never reach a verified done state. The tool
does not eat its own dog food, and the first repo where the loop silently
no-ops is the one that builds it.

## Acceptance Criteria

- [x] `.jjfx/config.toml` declares this repository's check.
- [x] A Run dispatched in this repository can reach a verified done state.
- [x] The check is affordable to run per Run. If `mise run check` (the landing
      gate) is too slow for that, say so and propose what the check should be
      instead - the landing gate and the verification check need not be the
      same command.

## Resolution

Declared this repository's own loop in `.jjfx/config.toml`:

```toml
[tickets]
source = "local"

[checks]
command = "mise run test"
timeout_secs = 1800
```

- **The check is `mise run test`, not `mise run check`.** The verification check
  and the landing gate need not be the same command, and they want different
  things. The gate answers "may this land" with fmt, clippy, a release build and
  changelog validation; this check answers the trust-boundary question ticket 02
  draws - "does the change work" - and the full workspace test suite is what
  answers it. It is a strict subset of the gate, so a passing check is a
  necessary condition for landing, and CI re-runs the whole gate on every pull
  request (`.github/workflows/ci.yml`).
- **It is affordable per Run.** Measured on this machine: `mise run test` is
  ~1m17s fully warm and ~1m54s in a freshly created worker-style Workspace - a
  cold target directory, which is what a Run's first check actually sees -
  against ~2m40s for `mise run check`. Dropping the release build and clippy's
  separate compile roughly halves it, and neither adds anything the tests do not
  already say about whether the change works.
- **`timeout_secs` is 1800, not the 600 default.** A cold compile in a fresh
  Workspace measured ~2 minutes on an idle machine, and several Workers
  compiling at once stretch that; the default would kill a legitimate check and
  record it as `CouldNotRun`, a loud failure on a check that was about to pass.
- **The command is the mise task, not bare `cargo test`.** mise owns the
  toolchain pin, and the suite spawns `jj`, which resolves through a mise shim -
  a bare `cargo test --workspace` run without mise's environment fails the
  direct-dispatch tests with `No version is set for shim: jj`. `AGENTS.md` says
  to use the tasks rather than hand-roll cargo, and here that is load-bearing
  rather than style.
- **`[tickets] source = "local"` is declared beside it.** The default source is
  `linear`, and this repository's Tickets are markdown under
  `.scratch/<feature>/issues/` (`docs/agents/issue-tracker.md`), so without the
  selection a dispatched Ticket could not be discovered or written back at all -
  the verified-done loop would still no-op here, just one step earlier.

What the check would fail to catch: formatting drift (`cargo fmt`), clippy lints
(warnings-as-errors), release-profile-only build failures (the tests build
debug), and a changelog `momo status` cannot read. All four are landing
concerns, and CI's `mise run check` catches them on the PR before merge.

Verified:

- **The config is read and valid.** jjfx reads `<repo>/.jjfx/config.toml` from
  the repository root, which is the main checkout rather than a Worker's
  Workspace, so the file was exercised from an isolated repository holding a
  copy of it: `jjfx ticket reconcile` reports `Reconciled local Tickets` where
  the same repository without the `[tickets]` selection reports `Ticket source
  is not local; nothing to reconcile`. An invalid `[checks]` value fails loudly
  and names the file (`validating config .../.jjfx/config.toml` /
  `checks.timeout_secs must be at least 1`), which is the `[checks]` section
  being parsed and validated rather than ignored.
- **The check command runs green in a freshly created jj Workspace** - a cold
  target directory, the state a Run's first check sees - in 1m54s, exit 0.
- **Not verified: a real end-to-end dispatch**, which needs a Worker session
  this change did not spawn. The Run-to-Ticket half is ticket 02's tested code;
  what was missing here was the configuration it reads, and that is what this
  adds.

## References

- `.scratch/harness/issues/02-verified-done.md`
- `docs/configuration.md` (`[checks]`)
- `.scratch/repo-local-config/`
