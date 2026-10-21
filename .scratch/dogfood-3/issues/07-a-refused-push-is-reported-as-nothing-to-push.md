# A push jj refused is reported as a harmless skip, and the delivery fails later at the Pull Request

Status: needs-triage
Type: bug
Blocked by: none

## Problem

`deliver`'s push step can push **nothing** while the pipeline carries on, so the
failure surfaces two steps later as a `gh` error that names none of the real
causes. Reproduced on the third drive's fixture, on a Worker whose chain
contained an empty commit with no description - a commit jj refuses to push:

```
$ jjfx deliver worker-348336
Delivering worker-348336: weld, check, push, Pull Request
  push: nothing to push
  pr: gh pr create (adam/tally--10-version-in-readme): pull request create failed:
      GraphQL: Head sha can't be blank, Base sha can't be blank,
      No commits between main and adam/tally--10-version-in-readme,
      Head ref must be a branch (createPullRequest)
Error: delivery of worker-348336 stopped: pr: gh pr create (...)
```

`push: nothing to push` is printed in the same indented shape as the pipeline's
other *skip* notices, and the delivery continues. The branch never reached the
remote, so `gh pr create` had no head to work with and reported it as a GraphQL
shape error.

jj had said exactly what was wrong, and jjfx dropped it. Run by hand, the forge's
own push revset says:

```
$ jj git push --revisions '::@ ~ trunk() ~ conflicts()'
Warning: Won't push bookmark "adam/tally--10-version-in-readme": commit 5722e547527f has no description
  yvyqxv 5722e5 (empty) (no description set)
Warning: Won't push bookmark main: commit 5722e547527f has no description
  yvyqxv 5722e5 (empty) (no description set)
Nothing changed.
```

The chain, in the Worker's own Workspace:

```
71becd  empty=false  "Show the version flag in the README usage"
5c130b  empty=false  "File a tenth Ticket: show the version flag in the README"
5722e5  empty=true   ""                       <- jj refuses to push this
603e9b9 empty=false  "Accept a whole-file JSON array as input, not only JSONL (#9)"
```

Two distinct problems sit here, and both are worth fixing:

1. **A refused push is not a "nothing to push".** They are different facts: one
   means the change is already on the remote, the other means the remote will not
   take it and the delivery must stop. jjfx reads jj's warning as a no-op and
   reports it as a skip. `dogfood-2`'s finding 05 fixed the *stop* for a genuine
   nothing-to-push; this is the case where "nothing" is not benign.
2. **An empty, undescribed commit survives the weld.** `5722e5` is in the chain
   `deliver` welds and pushes, and it is the reason the push is refused at all. A
   deliverable chain should not carry it: either the weld drops an empty commit
   (it is not a change), or the refusal names it.

Whichever way it is fixed, the operator must not be left reading a GraphQL error
about a blank head sha to discover that a commit in their branch has no
description.

## Acceptance Criteria

- A push that pushed nothing because jj **refused** it stops the delivery and
  names the reason jj gave (the offending commit, and that it has no
  description), rather than reporting a benign skip and continuing.
- A genuine nothing-to-push - the change is already on the remote - still
  behaves as it does today.
- A delivery never reaches the Pull Request step with nothing pushed: the step
  is either skipped with a reason that says the branch is not on the remote, or
  the pipeline has already stopped.
- An empty commit in the welded chain is either dropped by the weld or named in
  the refusal.
