# Fixed sibling paths collide across concurrent test runs

Status: ready-for-agent
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

- [ ] No test creates a workspace or repository at a path that is fixed across
      runs; each is derived from the unique `test_local_repo` directory.
- [ ] The affected tests pass when two suites run concurrently - verify by
      running the suite in this workspace while another full suite runs against
      the same temp directory, and repeat enough times to be convincing.
- [ ] The change does not weaken what the tests assert (they still pin the same
      behaviour; only the path becomes unique).
- [ ] `mise run check` is green.

## Out of Scope

- The deadline/flake class in ticket 35.
