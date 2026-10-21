# Tests that depend on jj's behaviour inherit the developer's jj config

Status: resolved
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

- [x] The throwaway-repository helpers isolate the ambient jj config, **or** the
      suite states and enforces the rule that a test depending on jj behaviour
      must construct that state itself rather than inherit it (the two tests in
      `dogfood-2/02` now do this, via `store::track_bookmarks_from_origin`).
- [x] A test that depends on a jj setting the developer may also set cannot be
      satisfied by the developer's own config: running the suite with the user
      config out of the picture (`JJ_CONFIG` pointing at an empty file) gives the
      same result as running it with the developer's config in place.
- [x] `AGENTS.md`'s CI-parity warning covers jj-config inheritance, not only
      dispatch and terminal stubbing.

## Resolution

Shape A, the helpers isolate. A unit test cannot set `JJ_CONFIG` on the `jj`
processes jjfx spawns, so the throwaway-repository helpers give each repository a
jj config it *owns*: `test_support::hermetic_jj_config` renders a test identity
and auto-tracking off (a glob matching no bookmark, because jj parses the value as
a string pattern and rejects an empty one), and `store::write_repo_jj_config`
writes it to the path jj reports for the repository config
(`jj config path --repo`, not the deprecated `.jj/repo/config.toml`, so no
migration warning reaches a test's stderr). `store::test_local_repo` and the three
`temp_repo` helpers (`src/jj.rs`, `src/engine/workspace.rs`, `src/work.rs`) call
it, and `store::track_bookmarks_from_origin` is now the opt-in that flips
auto-tracking on for the tests that assert the tracked-but-absent state.

On the integration side the fixture owns its config through `JJ_CONFIG`, which
*replaces* the developer's user config rather than layering on it.
`tests/support::hermetic_jj_config_file` is the shared way to get one, and
`tests/pool_reuse.rs` - the one binary the sweep caught, because it pointed
`XDG_CONFIG_HOME` at its own home but inherited an ambient `JJ_CONFIG`, which
takes precedence and left every commit without an author - now owns its config
too. The fixtures that already isolated via `XDG_CONFIG_HOME` alone
(`logs`, `wait`, `ticket_list`, `adhoc_ticket`, `local_tickets`,
`capacity_governor`, `resolve`) also `env_remove("JJ_CONFIG")`, so no fixture
inherits an ambient one. `AGENTS.md` gained a "Test hermeticity (CI parity)"
paragraph covering both the dispatch/terminal seam and jj-config inheritance,
with the two-way sweep to run before landing a test change.

The choice is behavioural, not structural: a throwaway repository is hermetic by
default (the next fixture pays no tax), and a fixture that depends on a jj
setting constructs it (`track_bookmarks_from_origin`).

Proved by sweeping every test binary both ways - with the developer's config and
with `JJ_CONFIG` pointed at an empty file, `XDG_CONFIG_HOME` unset and `HOME` a
throwaway. Before: `pool_reuse`'s
`a_pool_smaller_than_the_backlog_serves_every_ticket_without_a_manual_reset`
failed in the isolated run (two Tickets expected, none dispatched - every one
`held back by the Worker Pool`). After: it passes both ways, and no other binary
changed. `tests/direct_dispatch.rs` fails identically in *both* modes when a
binary is run by hand, because the helper it spawns resolves `jj` through a
`mise` shim that is only active under `mise run test`; under `mise run check`
(where the shim resolves) it is green, so it is a harness artifact, not a
config-inheritance offender.

New coverage: `src/store.rs` asserts a throwaway repository's own identity wins
over the developer's and that a fresh bookmark lists once, and that
`track_bookmarks_from_origin` makes it list twice. `mise run check` is green
(fmt + clippy + build + 995 lib tests + every integration binary). No changelog
entry: this is an internal test-hygiene change with no user-visible behaviour.

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
