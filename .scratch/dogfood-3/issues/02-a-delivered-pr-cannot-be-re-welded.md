# A delivered PR cannot be updated once trunk moves, and reusing the Worker destroys the only handle

Status: resolved
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

- [x] An already-delivered change can be re-welded onto the moved trunk through
  jjfx - by naming the Worker if it still holds the Ticket, or by naming the
  branch/PR when it does not - and the conflict is reported as a named outcome
  with the workspace an operator must resolve it in.
- [x] Reusing a Worker does not silently orphan its delivered change: either the
  handle survives the reuse, or jjfx says plainly that the change is now
  reachable only by branch name and how to get to it.
- [x] The dispatch prompt, `jjfx help` and the skill agree on what an agent should
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

## Resolution

The durable name of a delivered change is its **bookmark**, not its Worker.
`rebase`, `deliver` and `open-pr` now take `<worker|branch>`: the name resolves
to the change's bookmark *and* the Workspace it is in, and the two spellings are
the same operation on the same thing. `absorb` and `review` stay Worker-only -
`absorb` folds a working copy's own edits, which a change with no Workspace has
none of, and `review` addresses a Pull Request by sending a Run to a Worker,
which a bookmark cannot name.

The engine grows one type, `LandingTarget` (`src/engine/worker_actions.rs`):
the name the operator gave (private `Named::{Worker, Bookmark}`), the Workspace,
its directory, and whether jjfx created it. `WorkerActions::landing_target(name)`
resolves a Worker first, in either spelling, so a Worker is never mistaken for a
bookmark of the same name; everything else is read as a bookmark, and a name
that is neither is refused by naming both things a landing verb accepts. A
Worker is recognised by Pool membership, by an existing `worker_path` directory
(the reading a delivery has always used, so a repository whose Pool state is not
materialised still resolves its Workers), or by a parseable `worker-`-prefixed
name - so a mistyped Worker id reads as a missing Workspace, not as a name that
means nothing.

`branch_of` resolves the bookmark (a Worker's via its Ticket, a bookmark's as
itself) and is a read, so a name that cannot be resolved fails before any
Workspace is touched. That split is what keeps `deliver`'s existing fixture
working: it has no Pool state, so the delivery must not need the Ticket read.
`open_pull_request` takes the name only - the Pull Request is keyed by the
branch and opening it is a browser action, so it must not create a Workspace to
name it.

When **no Workspace holds the bookmark**, jjfx creates one named
`branch-<sanitised bookmark>` (`workspace_name_for_branch`; every character a
Workspace name cannot carry folds to `-`), says so, and leaves it in place. A
Workspace is not optional: the rebase has to rewrite a working copy, `deliver`'s
check runs against it, and a conflict a rebase records is only reachable through
a Workspace's own chain (`jjfx resolve <workspace>`). A name already taken by a
Workspace that does *not* hold the change is refused by name rather than rebased
in. A bookmark that is the mainline is refused too, because re-welding trunk
onto itself is a no-op and a landing verb must never push `main`.

`RebaseConflicted` changed from `{ worker, branch }` to `{ branch, workspace }`
and names `jjfx resolve <workspace>`, and the forge's `stop_on_conflicts` reason
names the Workspace the same way - the conflict a `deliver` hits is the same
conflict, so it must name the same place. `WorkspaceOperation`'s field became
`workspace: String`, and `lock_worker_operation` now delegates to a new
`lock_workspace_operation(repository, workspace)` so the lock is keyed by the
Workspace either way. `BranchNotFound` - the refusal a reused Worker gives -
now says plainly that the change it delivered is reachable by its bookmark and
how to act on it, which is criterion 2's "or jjfx says plainly" branch, on top
of criterion 2's first branch: the handle does survive the reuse.

**Rejected alternatives.** Naming a Pull Request *number* is a bare number in
jjfx's vocabulary already - `4` means the Worker short id `worker-4` - and
`rebase` would take a `gh` dependency it does not have today. Recording a
`delivered_branch` on Worker state is a new persisted field that is wrong the
moment the Worker delivers a second change. Threading a reuse-time notice
through `Dispatch` and `DirectPrepareSuccess` is large plumbing for something
the bookmark makes discoverable anyway. Extending `resolve` to be branch-scoped
instead of creating a Workspace removes the step the operator wants least, but a
Workspace is still required underneath, so it would hide the mechanism rather
than replace it.

Coverage: `tests/reweld.rs` drives the real binary against a repository with a
local bare `origin` and a change no Workspace holds - the state a reused Worker
leaves behind - through the whole round trip (re-weld, name the Workspace,
resolve, re-weld, push), a second re-weld that reuses the Workspace, a
still-trunk run that succeeds, `deliver` by bookmark, a change a Workspace
*does* hold, and the three refusals (unknown name, mainline, missing Worker).
`tests/deliver_contract.rs` asserts the prompt, `jjfx help`, the skill, the
playbook and `docs/configuration.md` agree. `src/engine/worker_actions.rs` unit
tests cover the name fold, the two refusals that carry new text, and the
Workspace name a bookmark folds to. `docs/configuration.md` ("Naming the
change: a Worker or its bookmark") and `docs/conflicts.md` carry the rule; the
dispatch prompt and the skill both name `jjfx rebase <bookmark>` as the re-weld,
so an agent whose PR conflicts after a sibling merged does not reach for a
Worker it no longer holds.
