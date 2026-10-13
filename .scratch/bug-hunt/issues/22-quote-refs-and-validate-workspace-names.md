# Quote refs and tighten workspace-name validation

Status: resolved
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

- [x] A workspace name containing spaces or parentheses does not produce
      `Unknown`.
- [x] No revset is built from an unquoted user-controlled name.
- [x] `mise run check` is green.

## Outcome (2026-10-13)

**Two root causes, both confirmed against jj 0.46:**

1. The unquoted interpolation the ticket describes. `::my work@` is a parse
   error, so the read fails, the chain degrades to `Unknown`, `behind` is 0, and
   `lift` returns `Ok(false)` without lifting anything.
2. A second, independent defect in the *read-back*: `jj workspace list -T 'name
   ++ "\n"'` renders `name` as a **symbol**, so a name that needs quoting comes
   back already quoted (`"my work"`) and never matched the raw name in the
   ws-cache. Even with the revsets fixed, the poller's snapshot keys would still
   miss the TUI rows. Fixed by reading `stringify(name)`.

**Fix:** one helper, `jj::revset_symbol`, double-quotes a name and escapes `"`
and `\` (the only characters that need it inside a jj string), applied to every
revset/diff-argument builder: `jj::lift_src`, `work::{behind, read_chain,
diff_loc}`, `diff::load`, and the bookmark-as-revset read in `forge::changes`.
Plus `stringify(name)` in both `workspace_names` readers.

The ticket offered "tighten the charset **or** guarantee the quoting helper
makes any accepted name safe"; the second branch was taken, so
`validate_workspace_name` is unchanged and `my work` remains a legal name that
now works end to end.

**Left as-is, deliberately:** the comma-joined bookmark split (`work::read_chain`)
and the newline-joined split (`forge::changes`). jj rejects commas and newlines
in bookmark names, so both joins are unambiguous and neither is a defect. The
bookmark-as-revset read was quoted anyway, defensively.

**Tests (6):** `work::workspace_names_with_spaces_and_parens_still_read_their_chain`,
`work::a_space_named_workspace_is_not_unknown_through_the_read_port`,
`jj::workspace_names_returns_the_raw_name_not_the_quoted_symbol`,
`engine::workspace::workspace_names_returns_raw_names_for_names_needing_quoting`,
`jj::revset_symbol_quotes_names_that_are_not_bare_symbols`,
`jj::lift_src_quotes_the_workspace_name`. The three integration tests were run
against partially-fixed code to capture the red state before completing the fix.

**Residual risk:** a name containing a literal newline still cannot round-trip
through the line-oriented `workspace_names` template. The revset quoting handles
it; the read-back does not. Pathological and pre-existing.

## Out of Scope

- The typed jj seam (ticket 21), though the helper may live there.
