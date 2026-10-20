# Tests that depend on jj's behaviour inherit the developer's jj config

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

A change's own guard test failed on CI and passed locally. The test asserted the
premise it depended on - that `jj bookmark list -T 'name ++ "\n"'` lists a
freshly created local bookmark **twice** when a remote is configured - and that
premise holds only because this machine's `~/.config/jj/config.toml` sets:

```toml
[remotes.origin]
auto-track-bookmarks = "glob:*"
```

On a clean-config runner the same bookmark lists **once**, so the guard failed:

```
assertion `left == right` failed: jj must list the fresh bookmark twice for this
test to mean anything: adam/eng-101-foundation
```

Reproduced directly, with `JJ_CONFIG` pointed at an empty file so the user config
is out of the picture:

- `jj git init --no-colocate .`, `jj git remote add origin <path>`, then
  `jj bookmark create adam/x -r @` → `jj bookmark list -T 'name ++ " | " ++ remote'`
  prints the bookmark **once**
- write `.jj/repo/config.toml` with `[remotes.origin]` /
  `auto-track-bookmarks = "glob:*"`, create another bookmark → that one prints
  **twice** (`remote=` and `remote=origin present=false`)

So the double-listing is a *configuration*, not a jj default - and the tests that
create throwaway repositories inherit whatever the developer has configured.

The throwaway-repository helpers (`temp_repo` in `src/jj.rs` and
`src/engine/orchestration.rs`, and the equivalents in `tests/`) pass
`--config signing.behavior=drop` but otherwise read the developer's jj config.
Any setting that changes jj's observable behaviour - a default remote,
auto-tracking, `ui.default-command`, aliases - can therefore make a test pass
here and fail on CI, or the reverse. That is the same failure mode `AGENTS.md`
already warns about for dispatch tests ("must use `support::stub_terminal` and
`run_jjfx_tolerating_a_starved_terminal`, or they pass locally and fail on CI"),
but nothing says it about jj-config inheritance, and no shared helper isolates it.

## Acceptance Criteria

- [ ] The throwaway-repository helpers isolate the ambient jj config, **or** the
      suite states and enforces the rule that a test depending on jj behaviour
      must construct that state itself rather than inherit it (the two tests in
      `dogfood-2/02` now do this, via `store::track_bookmarks_from_origin`).
- [ ] A test that depends on a jj setting the developer may also set cannot be
      satisfied by the developer's own config: running the suite with the user
      config out of the picture (`JJ_CONFIG` pointing at an empty file) gives the
      same result as running it with the developer's config in place.
- [ ] `AGENTS.md`'s CI-parity warning covers jj-config inheritance, not only
      dispatch and terminal stubbing.

## References

- `.scratch/dogfood-2/issues/02-landing-verbs-refuse-a-fresh-bookmark.md` - the
  ticket whose guard exposed this
- `src/jj.rs` (`temp_repo`, `local_bookmark_names`), `src/store.rs`
  (`track_bookmarks_from_origin`), `src/engine/orchestration.rs`
- `AGENTS.md` ("Integration tests that dispatch ...")
- CI run 39450238338 (the failure)

## Notes

Found while working the second dogfood drive's backlog (2026-10-20), by the
orchestrator merging `dogfood-2/02`. The change itself was correct; only the
test's premise was environment-dependent.
