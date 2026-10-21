# A merged Pull Request is refused as "no pull request found"

Status: ready-for-agent
Type: bug
Blocked by: none

## Problem

`review_target` resolves the Worker's **open** Pull Request, which is right - a
merged or closed PR has nothing left to review - but when there is none it
reports that no Pull Request exists at all, which is false when one has already
merged. The operator is sent looking for a missing PR instead of being told the
work already landed.

```
$ gh pr view 9 -R Jarvvski/jjfx-drive-3 --json number,state
{"number":9,"state":"MERGED"}

$ jjfx review worker-348336
Error: no pull request found for branch adam/tally--09-json-array-input
```

The Pull Request is right there, and `gh pr view <branch>` finds it. The refusal
should name the state it found - merged, or closed - the way this repository
already names a Blocker's bookmark state (`missing`, `ambiguous`, `trunk`)
instead of a bare "no branch" (`dogfood-2`'s finding 02).

An agent that lands through jjfx and then runs `review` on the Worker is exactly
the case: the PR merged, the Worker has not been reused yet, and the answer it
gets is that no Pull Request exists.

## Acceptance Criteria

- A Worker whose Pull Request has merged, or been closed, is refused by name with
  that state - not as a Pull Request that does not exist.
- Only a branch with no Pull Request at all reads as "no pull request found".
- `jjfx help review` or `docs/review-loop.md` says what happens to a Worker whose
  PR has already merged, so the answer is not a surprise.

## Triage (2026-10-21)

Triaged from `needs-triage` to `ready-for-agent`. The evidence is a **live
reproduction during the third drive**: `Jarvvski/jjfx-drive-3#9` merged,
`gh pr view` still found it, and `jjfx review worker-348336` answered `no pull
request found for branch adam/tally--09-json-array-input`.

Independently **confirmed**: `pull_request_for` returns
`WorkerActionError::PullRequestNotFound { branch }` whenever the open-Pull-Request
lookup comes back empty (`src/engine/worker_actions.rs:882`), which is one error
for a merged PR and for no PR at all.

The acceptance criteria are unchanged. This is a message-honesty fix, the same
rule `dogfood-2`'s finding 02 applied to a Blocker's bookmark state - name the
state found, not the absence of one.
