# Make the release workflow resilient to a moving `main`

Status: resolved
Type: task
Blocked by: none

## Problem

`.github/workflows/release.yml` checks out `main`, spends roughly 13 minutes on the pre-land gate (`mise run check`) and the three-target cross-compile, and only then commits the minted release and pushes it:

```
git push origin "HEAD:${{ github.ref_name }}" "refs/tags/v$version"
```

That push is fast-forward only. If any commit lands on `origin/main` while the run is in flight, the branch push is rejected (`! [rejected] HEAD -> main (fetch first)`) and the whole release fails - after the gate and the build have been paid for, and *after* the tag has already been published in the same command.

## Evidence

Run 38600471957 (2026-10-14, the first real 0.53.0 release) failed at `Commit and tag`:

```
* [new tag]         v0.53.0 -> v0.53.0
! [rejected]        HEAD -> main (fetch first)
error: failed to push some refs to 'https://github.com/Jarvvski/jjfx'
```

A concurrent session pushed four commits (`e03ed0b6` … `ac59c90a`) during the ~13 minute window, so the workflow's checkout base was stale by the time it pushed. The tag is published before the branch push, so `v0.53.0` was left pointing at `78343cd`, a commit that is not an ancestor of `main` (`compare` reports `diverged`). crates.io was untouched - the publish step runs after the push - so no version was burned.

Recovery required deleting the orphaned tag by hand before a re-run, because the workflow's own guard (`git rev-parse refs/tags/v$version`) then refuses the retry.

## Impact

Medium-high, and it scales with how much work is in flight - the failure mode is *more* likely on a busy repository. Two distinct costs:

- The failure is **late**: the gate and the cross-compile are discarded.
- The failure is **partial**: the tag lands even when the branch does not, leaving a dangling tag that blocks a clean retry.

## Options

1. **Fetch, rebase, push, with a bounded retry.** Before pushing, `git fetch origin main` and `git rebase origin/main`, then push; on a non-fast-forward rejection, retry a small fixed number of times. The release commit touches only `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md` and `.momo/version`, so a rebase is normally clean - but a concurrent `CHANGELOG.md` edit can conflict, and a conflict must fail the run loudly rather than drop the entry.
2. **Push the branch before the tag.** Reorder the two pushes so a rejected branch push cannot orphan the tag; the tag only lands once the branch has. Smallest fix for the partial-failure half.
3. **Serialize the release against ordinary pushes.** A concurrency group or lock so a release cannot start, or its push cannot race, while `main` is moving. `concurrency: group: release` already serializes releases against each other; it does not serialize them against ordinary pushes.

Options 1 and 2 are complementary and both small. Option 3 is larger, and arguably unnecessary once 1 and 2 hold.

## Acceptance Criteria

- [ ] A commit landing on `origin/main` mid-run does not fail the release: the release commit is rebased onto the latest `main` (or the push is retried) and lands, within a bounded number of attempts.
- [ ] A rejected branch push never leaves an orphaned tag: the tag is published only after the branch push succeeds, or is cleaned up on failure.
- [ ] A rebase conflict on `CHANGELOG.md` fails the run with a clear error rather than dropping a concurrently recorded entry.
- [ ] The existing guards are preserved: the release still refuses to run when the tag or the crates.io version already exists.

## Out of Scope

- Version derivation, minting, tagging and release notes - this is only the push/race behaviour.
- Making the pre-land gate or the cross-compile faster.

## Comments

Filed 2026-10-14, discovered during the momo migration: the first real 0.53.0 release hit exactly this (run 38600471957). crates.io was untouched; the orphaned `v0.53.0` was deleted by hand and the release re-run succeeded (run 38603112212).

- 2026-10-16 - Resolved with options 1 and 2 (both small, complementary); option 3 was not taken - serialising releases against ordinary pushes is larger and unnecessary once 1 and 2 hold. `Commit and tag` now fetches `origin/main`, rebases the release commit onto it, and retries the push up to five times when another commit wins the race between the fetch and the push. It pushes the branch *before* the tag, so a rejected branch push can never orphan the tag; the tag is created with `momo tag` (unchanged) only after the branch push succeeds. A `CHANGELOG.md` rebase conflict aborts and fails the run with an explicit "never force the push" message rather than dropping the concurrent entry. The derive-step guards (existing tag, existing crates.io version) are untouched. Validated the loop against a throwaway git repo (clean rebase, one retry, conflict, budget exhaustion) and `actionlint` + `shellcheck`.
