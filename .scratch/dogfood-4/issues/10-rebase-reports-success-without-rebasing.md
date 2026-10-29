# `jjfx rebase` reports `Rebased <branch>` while rebasing nothing, on a stale trunk

Status: ready-for-human
Type: bug
Blocked by: none

## Problem

The Dispatch prompt's recovery for a conflicted Pull Request is re-welding by
name:

> a Pull Request that conflicts after a sibling merged and moved trunk under it is
> re-welded by name with `jjfx rebase <your bookmark>`, which **rebases it onto
> trunk and pushes it** and names the Workspace to resolve in when it conflicts.

`jjfx rebase` does not fetch. It rebases onto whatever `main` the repository
already has, so when the sibling's merge exists only on the remote - the normal
case, since a merge happens on the forge - the branch is *already* a child of the
stale trunk, nothing is rewritten, nothing is pushed, and the command reports
success anyway:

```
$ jjfx rebase adam/ledger--02-input-sources
Rebased adam/ledger--02-input-sources

$ jjfx rebase adam/ledger--04-json-output
Rebased adam/ledger--04-json-output

$ gh pr view 2 -R Jarvvski/jjfx-drive-4 --json mergeable,mergeStateStatus
mergeable=CONFLICTING state=DIRTY
```

Nothing was rebased and nothing was pushed - the shared repository's operation log
has no rebase or push op after either command, and both remote branch heads are
byte-for-byte the commits they were before:

```
$ gh api repos/Jarvvski/jjfx-drive-4/commits/adam/ledger--02-input-sources --jq .sha
3d89bc04beb77176354088b49757a598449ba059     # unchanged
```

The same command, once the trunk is fetched, does the right thing and says so
plainly:

```
$ jj git fetch
bookmark: main@origin [updated] tracked

$ jjfx rebase adam/ledger--02-input-sources
Error: rebased branch adam/ledger--02-input-sources onto main, but it has
conflicts to resolve: resolve them in Workspace branch-adam-ledger--02-input-sources
(`jjfx resolve branch-adam-ledger--02-input-sources`), then re-run to push; the
rebase is left in place
```

That second message is everything the first one should have been. The failure is
not that jjfx cannot rebase - it rebases well, and names the Workspace, the command
to resolve in, and the fact that the rebase is left in place. The failure is that
with a stale trunk it cannot tell "there is nothing to do" from "I cannot see what
there is to do", and reports the first.

`jjfx` has no `fetch` verb at all (`jjfx help` lists `sync`, which re-syncs *one*
Workspace), so the repository-wide fetch the recovery depends on is an operator
step the tool neither performs nor mentions.

## Why this matters

This is the documented, prompt-advertised recovery for the most common landing
failure - two Tickets dispatched in parallel that touch the same file, where the
first merge moves trunk under the second. An operator following the prompt gets a
green-looking `Rebased <branch>` and a Pull Request that is still conflicting, with
no reason to look further. It is the same shape as
[`09`](09-a-run-can-report-success-without-delivering.md) and drive 3's
[`07`](../../dogfood-3/issues/07-a-refused-push-is-reported-as-nothing-to-push.md):
an operation that did not happen, reported as one that did.

It is *not* a recurrence of drive 3's
[`02`](../../dogfood-3/issues/02-a-delivered-pr-cannot-be-re-welded.md) - that
finding was the absence of a rebase-by-bookmark verb, which now exists and works.
This is the verb lying about what it did.

## Acceptance Criteria

- [x] `jjfx rebase` either fetches the trunk, or detects that the local `main` is
      behind `main@origin` and refuses with a message naming the fetch - never
      reports `Rebased` on a rebase it did not perform. **Done in #199**: it
      fetches first (`crate::jj::fetch`, "what advances `trunk()`"), and a fetch
      that fails refuses with a named error rather than guessing.
- [x] A rebase that rewrites nothing says so, and distinguishes "the branch is
      already on trunk" from "the trunk I can see is stale". **Done in #199**:
      `SystemCommands::push` returns its `PushOutcome` rather than collapsing
      `Pushed` and `Nothing`, so `RebaseOutcome` knows whether the branch was
      rewritten - "already on the fetched trunk; nothing to rebase, nothing to
      push" instead of `Rebased`.
- [ ] The outcome is observable: after a re-weld, `jjfx status` (or the Worker's
      delivery state, as in [`09`](09-a-run-can-report-success-without-delivering.md))
      shows whether the Pull Request is still conflicting, so a no-op cannot pass
      for a recovery.
- [x] A test drives the sequence: a branch whose sibling merged on the remote,
      against a repository whose local trunk has not been fetched, and asserts the
      operator is told rather than misled. **Done in #199**, and falsified: with
      the fetch removed the test fails, leaving `main` on the stale commit.

## Comments

- 2026-10-27: #199 fixes the fetch and the no-op report. The third criterion
  above is untouched: after a re-weld, nothing surfaces whether the Pull Request
  is still conflicting.
- 2026-10-29: **Correction.** This finding's closing note - that `deliver`'s weld
  has "the same staleness", welding onto `trunk()` without fetching - is
  **wrong**, and checking it against the code is what caught it. `deliver` runs
  the shared `run_pipeline`, whose *first* step is `jj.fetch()`; a fetch that
  fails aborts the delivery outright with `fetch failed` (`src/forge.rs`). So
  `deliver` has never had this bug - only `rebase` did, and #199 fixed it. What
  remains here is the observability criterion, which is a decision about where a
  re-weld's conflict state surfaces rather than a stale-trunk bug.
