# One jj access layer with typed outcomes

Status: ready-for-agent
Type: task
Blocked by: none

## Improvement

jj semantics are re-derived from CLI text in many places, which is the root of
several bug classes:

- `pushed_something` sniffs `"Nothing changed."` (`forge.rs:36-39`).
- `jj::count` returns 0 on any failure, so `tidy`/`tidyws`/`lift` treat a broken
  revset as "nothing to do" (`jj.rs:238-246`).
- `parse_diff_stat`, revset string building, and bookmark handling all parse
  stdout.

`jj-lib` is already a dependency (used for the graph, ADR-0007), and the crate
has no `unsafe`, so a typed seam is cheap.

## Fix

Introduce one jj access layer that returns typed outcomes
(`pushed`/`conflicted`/`nothing`, structured counts, structured diffs) instead
of scanning stdout, and move the string-sniffing call sites onto it. Migrate
reads to `jj-lib` where it is safe; keep CLI calls where ADR-0007 says to.

## Commits

1. Define the typed seam and port `count`, diff-stat, and push.
2. Port the remaining stdout scanners.
3. Tests: each typed outcome is exercised with a stub `jj`.
4. CHANGELOG entry `(add)` (or `(fix)` if it closes user-visible bugs).

## Acceptance Criteria

- [ ] No user-visible jj semantic is derived from English CLI text.
- [ ] A jj failure is a typed error, never a silent empty/zero result.
- [ ] `mise run check` is green.

## Out of Scope

- Replacing every CLI call with jj-lib; the version-compat question is ticket 24.
