# The dispatch skill's preconditions and ordering do not match the tool

Status: ready-for-agent
Type: docs
Blocked by: none

## Problem

`skills/jjfx-dispatch/SKILL.md` is the documented way to drive jjfx, and a
first-hand drive found it wrong or silent in four places:

- **Ordering.** "Before dispatching" lists the runtime profile ahead of pool
  capacity, but `jjfx pool profile` fails until the pool exists (see
  `.scratch/dogfood/issues/04-pool-profile-before-the-pool-exists.md`).
- **It never says to pin the binary.** A stale `jjfx 0.54.0` sits first on
  `PATH` on the drive's machine, and the skill's central claim - that
  `jjfx help <command>` prints the parser-derived flag surface - is false
  against it, producing misleading results.
- **The Land section presents `absorb`/`rebase`/`open-pr` as routine.** It
  should say what each requires to be safe: an immutable trunk, a resolvable
  bookmark, a GitHub remote.
- **It does not say the opencode runtime has no lifecycle plugin** installed,
  or what that costs (a Worker waiting on you still reads `busy`).

## Acceptance Criteria

- [ ] The checklist order matches what the commands actually require.
- [ ] It says to pin the binary, and how to check which one is running.
- [ ] Each landing verb names its precondition.
- [ ] The lifecycle-plugin gap is stated, with what it costs.

## References

- `skills/jjfx-dispatch/SKILL.md`
- `.scratch/dogfood/issues/01-absorb-must-not-rewrite-trunk.md`
- `.scratch/dogfood/issues/02-resolve-the-worker-bookmark-before-mutating.md`
- `.scratch/dogfood/issues/03-open-pr-refuses-a-non-github-origin.md`
