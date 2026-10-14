# Pull-request base for mixed merged and unmerged blockers

Status: ready-for-agent
Type: question
Blocked by: none

## Question

For a dependent with several direct blockers - one **merged** (base `main`) and
another **completed with a real stacked branch** - what should
`pull_request_base` be?

Split out of ticket 28 (item D7). The behaviour is confirmed; no doc decides the
mixed case, so it needs an intent decision before any code moves.

## What the code does today

`dependency_context` (`src/engine/dispatch_group.rs:487-529`) collects **every**
direct blocker's branch, unfiltered (`:496-505`), then returns
`DispatchDependencyContext::new(branches.clone(), description, branches[0].clone())`
(`:524-528`) - the third argument is `pull_request_base`
(`src/engine/direct_dispatch.rs:19-52`).

The only guard is "all branches are `main`" → `None` (`:506-508`). A mix of
`main` plus a real branch passes that guard and silently uses `branches[0]`.
Merged blockers carry `branch = Some("main")` (`:358-360` build path, `:459-461`
`Merged` event), and `blocked_by` is sorted by `TicketId`
(`src/engine/ticket.rs:861-870`, a `BTreeSet`), so "sorts first" means the lowest
ticket id.

The base is consumed verbatim into the PR command: `direct_dispatch.rs:541-547`
runs `gh pr create --base <pull_request_base>`.

**Minimal scenario.** ENG-103 has `blocked_by = ["ENG-101", "ENG-102"]`. ENG-101
is merged → `branch = "main"`; ENG-102 is completed with a real stacked branch →
`branch = "adam/eng-102-api"`. `branches = ["main", "adam/eng-102-api"]`, so
`branches[0]` is `"main"` and the dependent opens its PR against trunk even
though a stacked prerequisite exists. The inconsistency is visible in the same
context: `description()` lists only the non-main branch ("Branch:
adam/eng-102-api …") while the PR command uses `--base main`, so the dependent's
PR would contain ENG-102's unmerged diff.

## Docs

- `.scratch/wsg-migration/issues/12-port-dispatch-group-model.md:45` - "Branch
  base selection follows direct Blockers and preserves current stacked
  behavior."; `:60` acceptance - "Base selection supports Stacked Pull Requests."
- No doc defines mixed merged+unmerged blockers, or says whether `main` should be
  skipped when choosing the base. `docs/wsg-compatibility-contract.md` fixes
  vocabulary only.

## Tests today

- `tests/dispatch_group.rs:653-688`
  `dependency_context_preserves_blocker_branch_order_and_uses_the_first_as_pr_base`
  (two **done** blockers, both real branches → base is the first).
- `tests/dispatch_group.rs:618-650`
  `dependency_context_is_absent_without_blockers_or_when_all_bases_are_main`.

No test covers a **mix**; the mixed case is untested.

## Options

1. **First non-`main` branch.** Skip `main` when selecting the base. Preserves
   stacking for the mixed case; changes current behaviour and the "first blocker"
   contract as written in wsg issue 12.
2. **Keep `branches[0]` (status quo).** Document that a leading merged blocker
   intentionally rebases onto trunk. Dependent PRs may swallow an unmerged
   prerequisite's diff; matches "follows direct Blockers" literally.
3. **Fail closed on a mixed set.** Return `None` (base `main`) only when the set
   is mixed, and flag it. Avoids a silently wrong base but loses the stack hint.

## Acceptance Criteria

- [ ] The chosen rule is recorded in `## Comments`.
- [ ] A test pins the mixed case, which is untested today.
- [ ] If the code changes, `mise run check` is green.

## Comments

**Decision (2026-10-14): option 1 - first non-`main` branch.** When the direct
blockers mix a merged one (`branch = "main"`) with a completed blocker carrying a
real stacked branch, the base is the first blocker whose branch is not `main`.

Rationale: it is the only option that fixes a silently wrong base. `branches[0]`,
sorted by ticket id, let a merged blocker rebase the dependent onto trunk while a
stacked prerequisite existed - so the dependent's PR would contain that
prerequisite's unmerged diff, contradicting the same context's `description()`
which already names the real branch. The "all branches are `main` -> `None`" guard
is unchanged; the "first blocker" wording in wsg issue 12 is amended.
