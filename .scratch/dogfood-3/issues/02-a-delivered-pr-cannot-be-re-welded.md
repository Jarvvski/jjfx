# A delivered PR cannot be updated once trunk moves, and reusing the Worker destroys the only handle

Status: ready-for-agent
Type: bug
Blocked by: none

## Problem

Two Workers delivered from the same trunk in one batch, and the second PR
conflicted as soon as the first merged. That is ordinary parallel-work pain, but
jjfx offers no way out of it, and its own fix for Worker reuse makes it worse.

```
$ jjfx deliver worker-348336      # tally--03, edits cli.py
Delivered worker-348336
$ jjfx deliver worker-a04066      # tally--04, also edits cli.py, same trunk
Delivered worker-a04066

$ gh pr list -R Jarvvski/jjfx-drive-3 --state open
4 adam/tally--04-json-output
3 adam/tally--03-level-filter

$ gh pr ready 3 ... && gh pr merge 3 ... --squash
$ gh pr merge 4 ... --squash
GraphQL: Pull Request has merge conflicts (mergePullRequest)

$ gh pr view 4 -R Jarvvski/jjfx-drive-3 --json mergeable,mergeStateStatus
adam/tally--04-json-output mergeable=CONFLICTING state=DIRTY
```

**Nothing in jjfx can update that branch.** Every landing verb takes a Worker and
nothing else:

```
$ jjfx help | grep -E 'rebase|deliver|review|absorb|open-pr'
  jjfx review <worker>
  jjfx rebase <worker>
  jjfx absorb <worker>
  jjfx open-pr <worker>
  jjfx deliver <worker>
```

And by the time the conflict was known, the Worker that delivered PR 4 had been
**reused for the next Ticket** - which is `dogfood-2`'s finding 07 working as
designed:

```
$ jjfx status | grep -E '^(348336|a04066) '
348336  default  -  busy  tally--05-window-semantics
a04066  default  -  busy  tally--06-blank-lines-crash
```

So `jjfx deliver worker-a04066` now delivers `tally--06`, and there is no
`jjfx` verb that names `adam/tally--04-json-output`. The delivered change is
reachable only through `jj` by hand: fetch, find the branch, make a workspace,
rebase it onto the new trunk, resolve, push.

The two changes are individually correct and individually green - each passed
the repository's check, and CI is green on both PRs. They are only wrong as a
*set*, which is the case `[checks]` was added for (the check runs against the
welded tree) and which the check cannot catch here because the weld happened
before the sibling merged.

## Acceptance Criteria

- An already-delivered change can be re-welded onto the moved trunk through
  jjfx - by naming the Worker if it still holds the Ticket, or by naming the
  branch/PR when it does not - and the conflict is reported as a named outcome
  with the workspace an operator must resolve it in.
- Reusing a Worker does not silently orphan its delivered change: either the
  handle survives the reuse, or jjfx says plainly that the change is now
  reachable only by branch name and how to get to it.
- The dispatch prompt, `jjfx help` and the skill agree on what an agent should
  do when its PR conflicts after a sibling merges.

## Triage (2026-10-21)

Triaged from `needs-triage` to `ready-for-agent`. The evidence is a **live
reproduction during the third drive**: two Workers delivered from one trunk, the
first merged, the second's Pull Request read `CONFLICTING` / `DIRTY`, and the
Worker that had delivered it was already reused for the next Ticket.

Independently **confirmed**: `WorkerActions::pull_request_for`
(`src/engine/worker_actions.rs:871`) resolves a Worker's branch and nothing else,
and every landing verb - `rebase`, `deliver`, `absorb`, `review`, `open-pr` -
takes a `<worker>`. No verb names a branch or a Pull Request, so a delivered
change whose Worker has been reused is reachable only through `jj` by hand.

The acceptance criteria are unchanged. This is the largest of the drive's
findings: *what names a delivered change once its Worker is gone* is a model
decision rather than a patch, and the second criterion (reuse must not silently
orphan a delivered change) is the half that can be settled without it.
