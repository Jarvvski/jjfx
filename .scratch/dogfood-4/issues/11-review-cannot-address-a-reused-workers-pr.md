# `jjfx review` takes only a Worker, so a Pull Request cannot be reviewed once its Worker moves on

Status: ready-for-human
Type: bug
Blocked by: none

## Problem

The Dispatch prompt states the principle plainly:

> Your bookmark, not your Worker, is the durable name of your change: a Worker is
> reused for the next Ticket the moment your Run ends. If your Pull Request
> conflicts after a sibling merged and moved trunk under it, re-weld it by name -
> `jjfx rebase <your bookmark>`.

`deliver` and `rebase` honour it - both take "a Worker's name, or the bookmark of
the change to deliver". **`review` does not.** Its usage is `jjfx review <worker>`
and nothing else, so the bounded review loop is reachable only while the Worker
still holds its branch - which, by the prompt's own description, ends the moment
the next Ticket is dispatched.

By the time a human actually reviews a Pull Request, the Worker has been reused or
reset, and there is no verb left that can address it:

```
$ jjfx review adam/ledger--04-json-output      # the durable name
Error: Worker identifier "worker-adam/ledger--04-json-output" is not a safe filename component

$ jjfx review ledger--08-monthly-sparkline     # the branch, unprefixed
Error: Worker worker-ledger--08-monthly-sparkline not found

$ jjfx review 79bdfa                           # the Worker that delivered the PR
Error: Worker worker-79bdfa has no branch
```

That last one is the trap. `worker-79bdfa` delivered a Pull Request; it has since
been reset to idle, and its state is wiped - `"branch_name": null`, `"ticket":
null` - so the branch that names its Pull Request is unrecoverable from the Worker:

```json
$ .jj/pools/default/worker-79bdfa.json
{"status": "idle", "agent": null, "ticket": null, "branch_name": null, ...}
```

The one Worker for which `review` does resolve its branch - the one that has not
been reset - is handled *well*:

```
$ jjfx review 70cae3
Error: pull request for branch adam/ledger--08-monthly-sparkline is merged; there
is nothing left to review
```

So the machinery works; it is only reachable through a handle the design says is
ephemeral.

The three refusals are also poor in their own right. `review adam/<branch>`
reports a filename-safety violation for an identifier the user never typed
(`worker-adam/ledger--04-json-output`), and `review 79bdfa` says "has no branch"
when the truth is "this Worker's Pull Request is `adam/ledger--07-monthly-summary`"
- the same unhelpful-lookup shape as
[`05`](05-pool-reset-in-the-wrong-pool-says-state-is-missing.md) and
`bug-hunt/48`.

## Why this matters

This is the drive's question 3 - *does the bounded review loop still terminate on
PR review threads?* - and on the released artifact the answer is that it cannot be
asked. Four of this drive's six delivered Pull Requests had their Workers reused
before any review could happen, and none of them can be reviewed now. The loop is
not broken; it is unreachable exactly when it is needed.

It is also a partial fix of drive 3's
[`02`](../../dogfood-3/issues/02-a-delivered-pr-cannot-be-re-welded.md): that
finding established the bookmark as the durable handle and the landing verbs now
accept it. The review verb was left behind.

## Acceptance Criteria

- [ ] `jjfx review` accepts a bookmark, exactly as `deliver` and `rebase` do - the
      durable name of the change, which is what survives the Worker. **Open**: the
      loop's address step launches a Run in the Worker's *own* Workspace
      (`follow_up` reserves a pool Worker and resumes its session), so a change no
      Worker holds has nowhere to be reviewed. Reaching it means either extending
      the Run launcher to an arbitrary Workspace or materialising a pool Worker
      for the change - a design decision, left to the owner.
- [x] A Worker that no longer holds its branch is refused in terms of the Pull
      Request, naming the bookmark to review instead, rather than reporting a
      missing branch. **Done in #200**: the refusal names the change, the
      bookmark, and `jjfx rebase <bookmark>`.
- [x] A non-Worker argument is refused with a message about the argument's form,
      not a filename-safety violation naming an identifier the user never typed.
      **Done in #200**: `normalize_worker` is no longer the first thing `review`
      does, and both refusals are covered by tests.
- [ ] A test covers the sequence: a delivered Pull Request, its Worker reused,
      then reviewed by bookmark. **Open**, and dependent on the first criterion:
      there is no bookmark path to test until one exists.

## Comments

- 2026-10-29: #200 lands the refusals. The first criterion is the substance of
  this finding - the loop is unreachable exactly when a human reviews, which is
  the drive's question 3 answered in the negative - and it is left open because it
  needs a decision about where a review Run runs, not because it is hard to type.
