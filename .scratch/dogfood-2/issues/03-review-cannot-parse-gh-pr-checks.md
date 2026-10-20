# `jjfx review` cannot read `gh pr checks` on gh 2.102

Status: needs-triage
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

- [ ] `jjfx review` reads the failing checks from a field the installed `gh`
      actually returns (`bucket` and/or `state`), so it runs on a current gh.
- [ ] A gh that returns a shape jjfx does not understand fails with a message
      naming the command and gh's own output, not a raw serde error.
- [ ] A test pins the parse against a captured `gh pr checks --json` body for a
      current gh (and, if the old shape must keep working, both).

## References

- `src/engine/worker_actions.rs:1317` (`failing_checks`)
- `src/engine/worker_actions.rs:759` (`WorkerActions::review`)
