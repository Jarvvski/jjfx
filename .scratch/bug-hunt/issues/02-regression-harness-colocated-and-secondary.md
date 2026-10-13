# Regression tests: colocated "pushed" and the secondary-workspace default root

Status: resolved
Type: task
Blocked by: none

## Outcome

Both halves resolved without a standalone red-test commit.

### D3 - refuted

`remote_bookmarks()` **already excludes** the colocated `git` remote. Verified
directly against `jj 0.46.0`: in a colocated repo with an `origin` remote and an
unpushed local bookmark,

```
$ jj log -r 'remote_bookmarks()' --no-graph -T 'change_id.short() ++ "\n"'
          # selects nothing
$ jj log -r 'remote_bookmarks(remote=git)' --no-graph -T 'change_id.short() ++ "\n"'
pvonnzkumunl
pozmnnnrkxmn
```

The `work.rs:420-422` comment is accurate, so an unpushed local bookmark reads
`Dirty`, not `Pushed`. The sweep's evidence was a misread of the
`remote_bookmarks` **template keyword** (which also omits `@git`) as the revset
result. No test is needed for correct behaviour; ticket 06 is `wontfix`.

### I5 - fixed

Fixed in ticket 11 with unit tests in `src/repo.rs` and `src/store.rs`.

## Acceptance Criteria

- [x] D3 either has a failing test or is refuted with evidence - **refuted**.
- [x] I5 has a failing test - landed with the fix (ticket 11).
