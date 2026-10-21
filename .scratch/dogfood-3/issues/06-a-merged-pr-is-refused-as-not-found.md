# A merged Pull Request is refused as "no pull request found"

Status: resolved
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

- [x] A Worker whose Pull Request has merged, or been closed, is refused by name
      with that state - not as a Pull Request that does not exist.
- [x] Only a branch with no Pull Request at all reads as "no pull request found".
- [x] `jjfx help review` or `docs/review-loop.md` says what happens to a Worker
      whose PR has already merged, so the answer is not a surprise.

## Resolution

`pull_request_for` (`src/engine/worker_actions.rs`) now resolves the Worker
bookmark's Pull Request in two steps instead of reading only the open list. The
open lookup (`gh pr list --head <branch>`, state `open`) runs first, so the
common case is unchanged and still one `gh` call; a Pull Request it finds is
reviewed exactly as before. Only when it finds none does a second lookup
(`gh pr list --head <branch> --state all --json state --limit 1`) read the state
of any Pull Request on the branch. The result is a typed
`PullRequestLookup::{Open, NotOpen, Missing}`: `Open` carries the reviewable Pull
Request, `NotOpen` carries the state `gh` reported, and only `Missing` - no Pull
Request in any state - produces `WorkerActionError::PullRequestNotFound`.

The state is a new typed value, `PullRequestState { Open, Merged, Closed }`,
decoded from `gh`'s own `state` vocabulary (`OPEN`/`MERGED`/`CLOSED`, the same
values its `--state` flag accepts) rather than invented, and it `Display`s as
`merged`/`closed`. A non-open Pull Request is refused with the new
`WorkerActionError::PullRequestNotOpen { branch, state }` - a typed variant, not
a formatted string - whose message names the state: `pull request for branch
<branch> is merged; there is nothing left to review`. This is the same rule
`dogfood-2`'s finding 02 applied to a Blocker's bookmark state: name the state
found, not the absence of one.

`jjfx help review` and `docs/review-loop.md` both now say that a Worker whose
Pull Request has already merged or been closed is refused by name, naming that
state, so a landed Worker's answer is not a surprise.

Coverage: `src/engine/worker_actions.rs` unit tests assert `gh`'s three states
decode and that the refusal names `merged`/`closed`; `tests/review_contract.rs`
drives `jjfx review` against a fake `gh` and asserts a merged Pull Request is
refused as `is merged`, a closed one as `is closed`, neither reading as "no pull
request found", that a branch with no Pull Request still reads as "no pull
request found", and that an open Pull Request is still reviewed even when an
earlier Pull Request on the same branch merged. A contract test asserts both the
help page and the review-loop page name the merged and closed states. Verified
with `mise run check` (fmt + clippy + build + test green) and re-run hermetically
(`env -u JJ_CONFIG -u XDG_CONFIG_HOME HOME=$(mktemp -d) JJFX_TEST_SIGNING_OFF=1`
against the built `review_contract` binary).

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
