# Resolve the Worker's bookmark before the landing verbs mutate anything

Status: ready-for-agent
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

- [ ] The landing verbs (`absorb`'s push, `rebase`, `open-pr`) resolve the
      Worker's actual bookmark, by the same rule `bug-hunt/14` established.
- [ ] A missing bookmark fails loudly and before any mutation - no rebased,
      conflicted workspace left behind.
- [ ] A push that matches no bookmark is not reported as success.
- [ ] A test drives a Worker whose bookmark carries the configured prefix and
      asserts `rebase` and `open-pr` find it.

## References

- `src/engine/worker_actions.rs:792` (`branch_and_workspace`), `:1140` (`push`)
- `src/engine/pool.rs:685`, `src/engine/dispatch_prompt.rs:156`
- `.scratch/bug-hunt/issues/14-single-source-branch-prefix.md`
