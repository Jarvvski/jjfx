# Quote refs and tighten workspace-name validation

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

Workspace names are interpolated unquoted into revsets:

- `work.rs:405` `({trunk})..{ws}@`
- `work.rs:269` `::({trunk}) ~ ::{ws}@`
- `jj.rs:119-121` `roots(... ::{ws}@)`

`validate_workspace_name` (`engine/workspace.rs:413-425`) accepts any single
`Component::Normal`, including spaces and revset metacharacters. Verified:
`jj log -r '::my work@'` and `'::a(b)@'` are syntax errors. A workspace named
`my work` then reads as `WorkState::Unknown` forever, `behind` reads 0, and
`lift`/`lift_src` silently return `Ok(false)`. Bookmark names as revsets
(`forge.rs:377-381`, `work.rs:459-463` comma-splitting) share the class.

## Fix

- Add one helper that emits refs/workspace names safely (quoted symbols or
  `bookmark("...")`).
- Tighten `validate_workspace_name` to the character set revsets can address, or
  guarantee the quoting helper makes any accepted name safe.
- Fix the comma-joined bookmark split.

## Commits

1. Quoting/escaping helper; apply to all revset builders.
2. Tighten or guarantee validation; add a name-with-space test.
3. Tests: a workspace with spaces/parens still reads its chain.
4. CHANGELOG entry `(fix)`.

## Acceptance Criteria

- [ ] A workspace name containing spaces or parentheses does not produce
      `Unknown`.
- [ ] No revset is built from an unquoted user-controlled name.
- [ ] `mise run check` is green.

## Out of Scope

- The typed jj seam (ticket 21), though the helper may live there.
