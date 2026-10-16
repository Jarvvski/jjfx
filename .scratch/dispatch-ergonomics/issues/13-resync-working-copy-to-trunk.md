# The working copy goes stale after a fetch, with no way to re-sync it

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

When `main` moves - a fetch, or a PR merging elsewhere - the working copy does not follow it. `@` stays where it was, so `@` and `main` become *siblings* rather than `@` sitting one above `main`. The files on disk are then the old tree, silently.

This is jj's semantics, not a bug in jj itself: there is no "current branch" to follow, and `@` is just a commit. But jjfx owns this workflow, jjfx tells you to fetch after landing a PR, and jjfx's own `AGENTS.md` states an invariant that a fetch quietly breaks:

> `@` is always an empty commit one above `main`.

Nothing detects the violation and there is no command that fixes it.

**It has now cost three separate sessions.** Concretely:

- An agent ran `momo status` against a stale working copy, saw a changelog section missing, and spent time investigating a bug in `momo` that did not exist. The committed tree was correct; the on-disk file was old.
- A second agent noticed after a fetch that its working copy "landed as a *sibling* of the new `main` (still based on the old one), so the on-disk file was stale", and fixed it by hand with `jj rebase -d main`.
- The same thing again after a merge landed: the fix each time is `jj new main` (when `@` is an empty commit, which it usually is) or `jj rebase -d main` (when it carries changes) - and knowing which to reach for is the whole trick.

Note the failure is silent and *plausible*: the tree looks normal, the files are real, and only the comparison against `main` reveals they are behind. Every command that reads a file - tests, `momo status`, a config load - quietly answers about the wrong revision.

## Solution

Give jjfx the two halves: a way to notice, and a way to fix.

- **Detect it.** `jjfx status` (and `where` / `root`) should say when the current workspace's `@` is not above trunk - how far behind it is, and the command to fix it. A warning is enough; this does not need to be an error. It is the same class of check as the existing "Worker whose Workspace is open" reporting.
- **Fix it.** A command that fetches and moves the current workspace onto trunk, e.g. `jjfx sync`. It has to handle both shapes: an empty `@` (move to a fresh commit above trunk) and a `@` carrying changes (rebase onto trunk, reporting conflicts rather than resolving them silently).
- **Name it carefully.** `jjfx refresh` already exists and means something else ("rebuild workspace cache"), so it is taken. `rebase` exists too and is per-Worker. `jjfx sync` is the obvious free name; pick something and document the distinction.

Out of scope: changing jj's behaviour, or making `@` auto-follow (it does not, and fighting that would surprise people who rely on it).

## Acceptance Criteria

- [ ] A workspace whose `@` is behind trunk is reported as such by a state-reading command, naming how far behind and the command that fixes it.
- [ ] A command re-syncs the current workspace onto trunk, covering both an empty `@` and a `@` with changes.
- [ ] Conflicts are surfaced, not silently resolved.
- [ ] The distinction from `jjfx refresh` and `jjfx rebase <worker>` is documented in the help.
- [ ] Tests cover the behind-trunk detection and the re-sync.
- [ ] `mise run check` is green.

## References

- `AGENTS.md` (the "`@` is always an empty commit one above `main`" invariant)
- `src/cli.rs` (`refresh`, `rebase`, `where`, `status`)
- `src/engine/workspace.rs`
- `.scratch/dispatch-ergonomics/map.md`

## Comments

- 2026-10-16: Filed at the owner's request. Hit by three sessions during the `todo-md` test-drive and the batch that followed it.
