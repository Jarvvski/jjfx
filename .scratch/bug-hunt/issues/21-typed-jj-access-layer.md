# One jj access layer with typed outcomes

Status: resolved
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

- [x] No user-visible jj semantic is derived from English CLI text.
- [~] A jj failure is a typed error, never a silent empty/zero result - met
      everywhere a machine-readable source exists; **one documented exception**
      (`forge::config_value`, see Outcome).
- [x] `mise run check` is green.

## Out of Scope

- Replacing every CLI call with jj-lib; the version-compat question is ticket 24.

## Outcome (2026-10-13)

**Resolved** (with one documented exception). Three commits:

- `33bc4da` - `Give jj one typed access layer and port count, diff-stat, and push onto it`
- `1423599` - `Type the remaining silent jj read failures (behind, conflict check, weld)`
- `30f1953` - `Show a failed diff read as an error, not an empty diff`

### Machine-readable mechanism per semantic

- **count** - `jj log -r <revset> --count`, jj's own integer. (Verified against
  real jj: `jj log --help` documents it as "Print the number of commits instead
  of showing them".)
- **workspace_names** - the existing `-T 'stringify(name) ++ "\n"'` template,
  now typed.
- **diff-stat** - `jj diff --from --to --git` parsed by the existing
  `diff::parse` and summed with `diff::line_totals`. The English `--stat` parser
  (`work::parse_diff_stat`) is **deleted**.
- **push** - `jj log -r 'remote_bookmarks()' -T '...remote/name commit_id...'`
  snapshots taken before and after the push, compared as sorted text. Uses
  `commit_id`, so an amend that moves a bookmark counts. `classify_push` is pure
  and unit-tested over a stub `Run`. The template was run against real jj to
  confirm it is valid syntax - the stub tests cannot catch a bad template.
- **behind / conflict check / weld** - templates and exit status, now typed.
  `behind` and `line_delta` degrade a row to `WorkState::Unknown` rather than
  reporting a plausible `0`/`+0/-0`.

### Verification

The pre-fix behaviour was captured by a throwaway test asserting the *buggy*
behaviour (`count` → `0`, `workspace_names` → `[]` on a non-repo), run green
against the unmodified code, then replaced by
`count_errors_on_a_broken_repository_instead_of_zero` and
`workspace_names_errors_on_a_broken_repository_instead_of_empty`. A grep of the
resulting tree confirms no English sniffing remains; the only `"Nothing
changed."` left is a doc comment plus stub-`Run` test *inputs* (which prove the
classifier ignores the text).

### The one exception: `forge::config_value`

`jj config get <key>` exits **1** for an unset key *and* **1** for a hard error
(verified directly: unset = 1, bad repository = 1, set = 0), so the two cases are
indistinguishable by exit status. The agent declined to wrap the English error
message and reported the gap rather than papering over it; `jj config list -T`
exists but renders values TOML-quoted (`signing.backend="gpg"`) and may not
surface a repository error either, so there is no clean fix. The practical impact
is bounded: a misread makes the signing path behave as "not configured", which
surfaces downstream as a visible push failure, not a silent bad state.

### Nits left, deliberately

- `work::behind` counts lines of a template output where `jj log --count` would
  do; both are machine-readable, so the criterion holds, but `--count` is tidier.
- A successful push whose after-snapshot read fails is reported as a failed
  step (the exit status says the push happened, but nothing moved is
  unknowable) - conservative, never silent.
