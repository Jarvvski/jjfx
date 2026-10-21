# A recorded review pass replays forever, so a new review thread is invisible and the loop reports convergence

Status: needs-triage
Type: bug
Blocked by: none

## Problem

A pass is keyed by `review:<ticket>:<dimension>:<pass>`, and a recorded pass is
replayed rather than re-read. Nothing in the key changes when the Pull Request's
review state changes, so **the first decision a pass makes is permanent**: a
review thread opened afterwards is never seen, and the loop reports a clean
convergence on a Pull Request that has an unresolved thread.

Reproduced on `Jarvvski/jjfx-drive-3#9` (`tally--09-json-array-input`), whose PR
is open with a Worker holding its branch:

```
$ jjfx review worker-348336 --fg          # the PR has no review threads yet
Reviewing worker-348336: Jarvvski/jjfx-drive-3#9 (at most 2 pass(es))
Review of worker-348336: 1 pass(es), 0 finding(s), stopped on convergence (a pass found nothing)

$ cat .jj/pools/default/decisions.json
{"decisions": {"review:tally--09-json-array-input:threads:1":
  {"findings": [], "completeness": "complete"}}}
```

A reviewer then opens a thread on the PR - one genuinely unresolved conversation:

```
$ gh api graphql -f query='query { repository(owner: "Jarvvski", name:
  "jjfx-drive-3") { pullRequest(number: 9) { reviewThreads(first: 20) {
  nodes { isResolved path } } } } }' \
  --jq '[.data.repository.pullRequest.reviewThreads.nodes[] | select(.isResolved==false)] | length'
1
```

and the next review **does not read it**:

```
$ jjfx review worker-348336 --fg
Reviewing worker-348336: Jarvvski/jjfx-drive-3#9 (at most 2 pass(es))
Review of worker-348336: 1 pass(es), 0 finding(s), stopped on convergence (a pass found nothing)
  1 pass(es) replayed from the decision log
```

`convergence (a pass found nothing)` is false. The pass found nothing because it
never ran: it was served from a decision recorded when the PR had no threads, and
the thread is still open on the PR. The Worker is never told, and the operator is
told the review is clean.

Replay is right within one loop - a pass that already ran must not be re-run to
buy the same findings - and the second drive's `harness/09`/`harness/12` built it
for exactly that. What it does not survive is the *next* invocation: a review
requested after new feedback arrives is the case the command exists for, and it
is the one case replay defeats.

## Acceptance Criteria

- A recorded pass must not outlive the state it decided on. A review thread
  opened after a pass was recorded is visible to a later `jjfx review`, and that
  review reads it.
- **`jjfx review` never reports convergence while the Pull Request has an
  unresolved review thread** - the loop's own honesty rule, and the thing this
  bug breaks. If it cannot tell, it must say so rather than claim a clean pass.
- Whatever shape is chosen - a key that includes the review state, a pass that
  is only replayed within a single invocation, or an explicit refresh - the
  bound is unchanged: `max_passes` and the progress rule still stop the loop, and
  a loop that stops on the cap or a stall still escalates.
- A test holds it: a recorded converged pass plus a new unresolved thread must
  not converge on the replay alone.
