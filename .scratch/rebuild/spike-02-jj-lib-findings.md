# Spike 02 findings: jj-lib matches the installed jj

Type: findings
Status: done
Parent: issues/02-spike-jj-lib.md

## Outcome: clean match - ADR 0007 stands, no fallback needed

- Installed `jj`: **0.43.0** (was Homebrew `/opt/homebrew/bin/jj`).
- `jj-lib` **0.43.0** exists on crates.io (published 2026-07-02, not yanked,
  MSRV 1.89 <= our pinned Rust 1.96.1). jj publishes `jj-lib` and `jj-cli` from
  one workspace at the same version, so the CLI's on-disk store format and the
  library are the same release. **No store-format mismatch; the CLI `-T`
  fallback in ADR 0007 was not needed.**

## Proof: a throwaway binary read this repo's `.jj/` store

A one-file crate (`jj-lib = "0.43.0"`, `pollster = "0.4"`, edition 2024) opened
this repo and read the commit graph + working-copy state without error. Output:

```
opened workspace at /Users/jarvis/Code/personal/jjfx
workspace name: WorkspaceName("default")
loaded repo at head, op id: OperationId("d60f648e…54d2d")
commit-graph heads: 1
  head 279f207d1c107149154dd4df418f554c503ba6aa change 2ad4af000e7f8478e22eba3c8bdbbc75 desc=""
working-copy commit id: Some("279f207d1c107149154dd4df418f554c503ba6aa")
working-copy op id: OperationId("d60f648e…54d2d")
OK: jj-lib 0.43.0 read the store cleanly
```

The head commit `279f207d…` matches the live working copy `@` at spike time,
confirming the read reflects real state.

### The minimal read pattern (0.43.0 API - for issues 05, 09, 11)

```rust
use jj_lib::config::StackedConfig;
use jj_lib::object_id::ObjectId;              // brings `.hex()` into scope
use jj_lib::repo::{Repo, StoreFactories};
use jj_lib::settings::UserSettings;
use jj_lib::workspace::{Workspace, default_working_copy_factories};

let settings = UserSettings::from_config(StackedConfig::with_defaults())?;
let workspace = Workspace::load(
    &settings,
    repo_root,                                // dir containing `.jj/`
    &StoreFactories::default(),
    &default_working_copy_factories(),
)?;
let repo = pollster::block_on(workspace.repo_loader().load_at_head())?;   // async!
let view = repo.view();
let heads = view.heads();                     // &HashSet<CommitId>
let commit = repo.store().get_commit(id)?;    // -> Commit (change_id, description, …)
let wc_id = view.get_wc_commit_id(workspace.workspace_name());
```

API notes that cost build iterations, recorded so the next ticket avoids them:

- `RepoLoader::load_at_head()` / `load_at()` are **`async`** in 0.43 - block on
  them (`pollster`) or drive on the app's tokio runtime. jj-lib work belongs on
  `spawn_blocking` per the engine plan, so wrap the block-on there.
- `UserSettings::from_config(StackedConfig::with_defaults())` is enough to open
  a repo for reading - no user config file required.
- `CommitId`/`ChangeId`/`OperationId` `.hex()` needs `jj_lib::object_id::ObjectId`
  in scope.
- `StoreFactories::default()` includes the git backend (jj-lib default features).

The throwaway crate lives in the session scratchpad, not the repo - it is not
committed.

## Pin

`mise.toml` now pins `jj = "0.43.0"` (mise resolves it via `aqua:jj-vcs/jj`, a
prebuilt binary). `mise which jj` -> the mise-managed 0.43.0; `mise run check`
passes. A future `jj` bump becomes a jjfx change: bump the `jj` pin and `jj-lib`
together, rerun this read, and land only when `mise run check` is green.
