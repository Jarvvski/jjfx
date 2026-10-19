# Resolve the Worker's bookmark before the landing verbs mutate anything

Status: resolved
Type: bug
Blocked by: none

## Problem

jjfx holds two names for a Worker's bookmark and they disagree.

- Worker state records the bare Ticket id: `branch_name: ticket.to_lowercase()`
  (`src/engine/pool.rs:685`).
- The dispatch prompt tells the agent to create
  `{prefix}/{ticket}-<short-description>` (`src/engine/dispatch_prompt.rs:156`).

So `jjfx rebase` fails on the name jjfx itself asked for:

    $ jjfx rebase worker-12af32
    Error: cannot rebase Worker bookmark: Error: Revision `contact-form--01-form` doesn't exist
    Hint: Did you mean `adam/contact-form--01-form`?

`branch_and_workspace` (`src/engine/worker_actions.rs:792`) hands the raw state
value to every landing verb - `absorb`'s push, `rebase`, `open-pr` - none of
which resolve the real bookmark. `bug-hunt/14` fixed exactly this class of
drift for the dependency-revalidation path (match on the Ticket, not a
namespace); the worker-action verbs were left on the other source of truth.

Two consequences make it worse than a wrong message:

- **A push of a missing bookmark succeeds.** `jj git push -b <name>` prints
  `Warning: No matching bookmarks ... Nothing changed.` and exits 0, so
  `absorb` reports success while pushing nothing.
- **It mutates before it fails.** `rebase` moves the Worker's working copy
  first and only then fails the bookmark lookup, leaving rebased and conflicted
  workspaces behind.

## Acceptance Criteria

- [x] The landing verbs (`absorb`'s push, `rebase`, `open-pr`) resolve the
      Worker's actual bookmark, by the same rule `bug-hunt/14` established.
- [x] A missing bookmark fails loudly and before any mutation - no rebased,
      conflicted workspace left behind.
- [x] A push that matches no bookmark is not reported as success.
- [x] A test drives a Worker whose bookmark carries the configured prefix and
      asserts `rebase` and `open-pr` find it.

## Resolution

The matching rule moved into one place the verbs share. `resolve_ticket_branch`
(`src/engine/orchestration.rs`) now returns a `TicketBranch` outcome -
`Resolved(name)`, `Missing`, or `Ambiguous(names)` - instead of a bare
`Option`, and is the single source of truth for "which bookmark carries this
Ticket's work?"; orchestration's branch repair and the `--all` stacked base
read `.name()`, while `WorkerActions::worker_branch` turns `Missing` and
`Ambiguous` into typed errors. `branch_and_workspace` (used by `rebase`,
`absorb` and `open-pr`) and `pull_request_for` (used by `review`) both resolve
through it, and resolution is a read, so it fails before the Workspace lock is
taken and before any jj mutation.

The push no longer trusts an exit code: `SystemCommands::push` goes through
`crate::jj::push_bookmark`, which classifies the result from the remote-bookmark
state *and* jj's bookmark list. The two zero-exit cases are opposites. A push
that moves a remote bookmark is `Pushed`. A push that moves nothing while the
bookmark exists is `Nothing` - a **no-op success**, because the desired state
already holds - so re-running `rebase` on an up-to-date Worker reports success
and does not roll the rebase back. A push whose name matches no bookmark is
`Failed`, because nothing could have been pushed; `Failed` is what rolls back.

Verified with `mise run check` (fmt + lint + build + test green), and by
`tests/worker_actions.rs`, where a Worker whose state names the bare Ticket
`eng-301` now drives `rebase`/`open-pr` onto the prefixed
`owner/eng-301-action`; a missing and an ambiguous bookmark both fail on the
resolution read with no rebase recorded; re-running `rebase` on an
already-published bookmark succeeds without an `undo`; and an absorb whose
bookmark vanishes before the push reports the failure and undoes the fold.

## References

- `src/engine/worker_actions.rs:792` (`branch_and_workspace`), `:1140` (`push`)
- `src/engine/pool.rs:685`, `src/engine/dispatch_prompt.rs:156`
- `.scratch/bug-hunt/issues/14-single-source-branch-prefix.md`
