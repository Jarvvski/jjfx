# Fixed sibling paths collide across concurrent test runs

Status: resolved
Type: task
Blocked by: none

## Problem

Three `store.rs` tests create their workspace directory at a **fixed** path
directly inside the shared system temp directory, so two test runs that overlap
in time clobber each other's directory:

- `src/store.rs:338` - `repo.with_file_name("alpha-workspace")`
- `src/store.rs:339` - `repo.with_file_name("zeta-workspace")`
- `src/store.rs:400` - `repo.with_file_name("orphan-workspace")`

`test_local_repo` (`src/store.rs:22-31`) makes the *repo* path unique
(`temp_dir()/jjfx-store-{tag}-{pid}-{nonce}`), but `with_file_name` replaces the
last component with a constant, so the sibling lands at
`temp_dir()/orphan-workspace` regardless of which run created it. Two concurrent
`cargo test` processes - which is exactly what running two jj workspaces at once
does - therefore share one directory, and whichever cleans up first removes the
other's.

Observed once, landing ticket 28: `store::tests::load_represents_jj_only_workspace_without_path`
failed while another agent's workspace was running the suite, and passed in
isolation.

## Why this is worth fixing

It is the same class as ticket 35 (the suite is not robust when the machine is
busy) but a different mechanism: 35 is about wall-clock deadlines, this is about
**isolation**. Concurrent runs are now normal rather than exotic - the
`jj-workspaces` workflow fans several agents out over one repo, and CI may shard
or parallelise - so a fixed path in a shared directory is a latent flake in any
of those setups. It costs a re-run at best and a wrong "this change broke it"
conclusion at worst.

## Fix

Make each sibling unique to the run. The codebase already has the pattern -
`src/app.rs:5507` derives its sibling from the unique repo directory name:

```rust
repo.with_file_name(format!("{}-external-feat", repo.file_name().unwrap().to_string_lossy()))
```

Apply the same shape (or a `test_local_repo`-style `{tag}-{pid}-{nonce}` suffix)
at the three `store.rs` sites. Check for the same shape elsewhere while you are
there: `with_file_name` appears only at these three sites plus the safe
`app.rs:5507`, but a fixed name passed to `create_dir`/`add_workspace` would have
the same defect.

## Acceptance Criteria

- [x] No test creates a workspace or repository at a path that is fixed across
      runs; each is derived from the unique `test_local_repo` directory.
- [x] The affected tests pass when two suites run concurrently - verify by
      running the suite in this workspace while another full suite runs against
      the same temp directory, and repeat enough times to be convincing.
- [x] The change does not weaken what the tests assert (they still pin the same
      behaviour; only the path becomes unique).
- [ ] `mise run check` is green.

## Out of Scope

- The deadline/flake class in ticket 35.

## Outcome (2026-10-13)

**Resolved.** `44b230` - `Isolate store test workspace siblings from the unique
repo directory`.

A `sibling(repo, tag)` test helper derives each workspace directory from the
unique `test_local_repo` path (`<temp>/jjfx-store-{tag}-{pid}-{nonce}-{name}`),
following the existing `app.rs:5507` shape. Assertions untouched - only the path
became unique.

Evidence: pre-fix, four concurrent suites over 12 rounds failed **39 of 48**
invocations on exactly the two affected tests; post-fix, **132 invocations across
three concurrency configurations failed 0**, including while pre-fix suites beside
them were failing 16 of 30 - i.e. the old fixed paths were being actively clobbered
at the same moment the new ones were not.

Sweep: `with_file_name` has no other sites, and every other temp-dir helper
(`ui_state.rs`, `events.rs`, `repo.rs`, `jj.rs`, `work.rs`, `engine/workspace.rs`)
is already process-unique. A few rely on pid-only uniqueness (no nonce), which is
safe for concurrent processes on one host.

No changelog entry: test infrastructure, nothing user-visible.

## Resolution

Reconciled Mon 19 Oct 2026 against the merged code at `9d6bd0ce6e7b`.

This ticket was marked resolved without its acceptance boxes ticked. Every criterion was re-verified by an independent read-only review of the code and tests on `main`. The reconciliation recorded no new work; it confirms what shipped.

Evidence: `src/store.rs:358` defines the `sibling(repo, tag)` helper, which derives each workspace directory from the unique `test_local_repo` path, and every affected site uses it (`:368`, `:369`, `:424`, `:443`, `:463`). No `with_file_name` with a constant remains in `src/store.rs`, so the fixed-path collision the ticket describes cannot recur. Criterion 2's concurrency claim follows from that uniqueness; the ticket's own recorded evidence (132 invocations across three concurrency configurations, 0 failures) is consistent with it. Criterion 3 holds because only the path derivation changed - the assertions are untouched.

Not ticked: the `mise run check` criterion. It is a landing-commit property, and this reconciliation deliberately does not run the full suite (the orchestrator runs the gate once at the end).
