# Run `jj fix` as a forge step before push

Status: resolved
Type: task
Blocked by: none

## Problem

The forge welds (rebases the chain onto trunk) and pushes, but nothing normalises
formatting across the chain, so a Worker can push commits that a formatter would
have changed. jj has a first-class primitive for exactly this — `jj fix` rewrites
a revset's file content through configured formatters, conflict-free — and jjfx
does not use it (verified: no `jj fix` in `src/`).

## Solution

Run `jj fix` inside the weld step, before push, over the workspace's own mutable
chain — e.g. `jj fix -s 'roots(mutable() & mine() & ::@)'` — honouring the user's
existing `[fix.tools.<id>]` jj config. With no tools configured it is a no-op, so
it is safe to run unconditionally.

`jj fix` only rewrites a file when its tool exits 0; a formatter that fails must
surface as a typed error, never a silent pass.

## Commits

1. Invoke `jj fix` over the workspace's mutable chain inside weld, before push.
2. Classify the outcome: no tools configured (no-op), success, and a formatter
   failure (typed error naming the tool), consistent with the typed-jj-access rule
   from bug-hunt ticket 21.
3. Tests (a repo with a trivial `[fix.tools]`); docs (`docs/configuration.md` or
   the forge docs); changelog.

## Acceptance Criteria

- [ ] After a forge, no commit in the chain has content a configured formatter
      would change.
- [ ] With no `[fix.tools]` configured, the step is a no-op.
- [ ] A failing formatter surfaces as a typed error, not a successful no-op.
- [ ] `mise run check` is green.

## Out of Scope

- Choosing or shipping formatters (the user's jj config owns that).
- `jj fix` outside the forge path.

## References

- `jj fix`: https://docs.jj-vcs.dev/latest/cli-reference/#fix and
  https://docs.jj-vcs.dev/latest/config/#code-formatting-and-other-file-content-transformations
- `src/forge.rs` (weld/push), `src/jj.rs`

## Comments

- 2026-10-15: Filed from the strategy review. jj-native, and it removes per-language
  pre-land glue (supports the "fewer knobs" goal).
- 2026-10-15: Resolved. `jj fix -s <welded chain>` now runs inside the Weld step,
  after the conflict re-check and before push, honouring the user's
  `[fix.tools]`. `jj fix` exits non-zero with a config error when no tools are
  configured, so a `jj config list -T name fix.tools` probe keeps the no-tools
  case a genuine no-op. jj exits zero when a tool fails (it warns and leaves the
  file unfixed), so the forge parses jj's own warning line into a typed error
  naming the tool and stops the push - a deliberate, documented exception to the
  typed-jj-access rule (bug-hunt 21), which jj's zero exit and lack of a
  machine-readable field force.
