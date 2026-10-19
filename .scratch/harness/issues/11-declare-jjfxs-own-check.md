# jjfx does not declare its own check, so its own loop cannot verify work

Status: ready-for-agent
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

- [ ] `.jjfx/config.toml` declares this repository's check.
- [ ] A Run dispatched in this repository can reach a verified done state.
- [ ] The check is affordable to run per Run. If `mise run check` (the landing
      gate) is too slow for that, say so and propose what the check should be
      instead - the landing gate and the verification check need not be the
      same command.

## References

- `.scratch/harness/issues/02-verified-done.md`
- `docs/configuration.md` (`[checks]`)
- `.scratch/repo-local-config/`
