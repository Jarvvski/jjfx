# `jjfx review` cannot read `gh pr checks` on gh 2.102

Status: resolved
Type: bug
Blocked by: none

## Problem

`WorkerActions::failing_checks` (`src/engine/worker_actions.rs:1317`) runs

    gh -R <repo> pr checks <n> --json name,conclusion

and decodes `conclusion`. gh 2.102.0 does not offer a `conclusion` field:

    $ gh --version
    gh version 2.102.0 (2026-09-30)

    $ gh pr checks 1 -R Jarvvski/jjfx-drive-2 --json
    Specify one or more comma-separated fields for `--json`:
      bucket
      completedAt
      description
      event
      link
      name
      startedAt
      state
      workflow

So `jjfx review` aborts before it starts any Run, for a Worker whose pull
request exists and whose check has actually passed:

    $ jjfx review worker-1be12c --fg
    Error: cannot query pull-request checks: Unknown JSON field: "conclusion"
    Available fields:
      bucket
      completedAt
      description
      event
      link
      name
      startedAt
      state
      workflow

The available equivalent is `bucket` (`pass` / `fail` / `pending` / `skipping` /
`cancel`) or `state` (`SUCCESS` / `FAILURE` / ...). The old field is not present
in this gh, so `review` is unusable on any Worker that has a PR - the one case
it exists for.

## Acceptance Criteria

- [x] `jjfx review` reads the failing checks from a field the installed `gh`
      actually returns (`bucket` and/or `state`), so it runs on a current gh.
- [x] A gh that returns a shape jjfx does not understand fails with a message
      naming the command and gh's own output, not a raw serde error.
- [x] A test pins the parse against a captured `gh pr checks --json` body for a
      current gh (and, if the old shape must keep working, both).

## References

- `src/engine/worker_actions.rs:1317` (`failing_checks`)
- `src/engine/worker_actions.rs:759` (`WorkerActions::review`)

## Triage (2026-10-20)

Triaged from `needs-triage` to `ready-for-agent`. The evidence is a **live
reproduction during the second drive**, not a code reading: the drive hit this
while working a real backlog through jjfx, and the report
(`.scratch/dogfood-2/report.md`) records what happened around it.

Independently **confirmed**: `gh 2.102.0` rejects `--json name,conclusion` -
`Unknown JSON field: "conclusion". Available fields: bucket`.

The acceptance criteria are unchanged - they were already specific and
testable, which is why this needed triage rather than a rewrite.
