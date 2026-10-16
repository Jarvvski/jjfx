# Migrate jj *reads* off the CLI onto jj-lib

Status: needs-triage

> Re-triage (2026-10-16, ticket 01): the pin-bump reassessment the note below asked for. The `jj` pin moved **0.43.0 -> 0.46.0** (`mise.toml`; `jj-lib` bumped in lockstep in `Cargo.toml`/`Cargo.lock`), spanning 0.44.0 (2026-08-05), 0.45.0 (2026-09-02), 0.45.1 (2026-09-03) and 0.46.0 (2026-10-07). **The gate is red on every clause, so the ticket stays parked.** Evidence:
>
> - **(a) no stability policy - still false.** jj's own FAQ (docs.jj-vcs.dev) states plainly: "`jj-lib` is not a stable API, so you may have to make changes to your tool when the API changes. The CLI is not stable either, so you may need to make your tool detect the different versions and call the right command." None of these releases shipped a semver/stability guarantee for `jj-lib`.
> - **(b) N releases without a breaking change to the read surface - false; the surface broke in every direction.** The five sites this ticket names are the ones hit:
>   - `jj.rs::workspace_names` (CLI `workspace list`): 0.44 made `jj workspace list` show workspace roots by default and changed `WorkspaceRef.root()` to `Option<FsPath>`; 0.46 stopped omitting unreachable paths and relabelled the template output (`workspace name`, `workspace root`, ...). Two output-format changes in three releases, and the repo's own copy of this read broke on the second - `Harden wsg-core workspace-list parsing for jj 0.46`.
>   - Template reads (`work.rs`): 0.44 changed `WorkspaceRef.root()`/`RepoPath.absolute()` from `String` to `Option<FsPath>`/`FsPath`, and `path` in `jj config list` to `Option<FsPath>`; 0.46 changed the `List.get()`, `.first()` and `.last()` template functions to return `Option<T>` instead of erroring.
>   - Revset evaluation: 0.45's default `immutable_heads()` now includes `untracked_remote_tags()` - a semantic change to the `mutable()`/`immutable()` revsets `tidy`/`lift` depend on.
>   - **`jj-lib` itself:** the *minimal* surface jjfx already reads (open + settings + store factories) broke across 0.43 -> 0.46 - `UserSettings::from_config` -> `from_config_and_home_dir`, `jj_lib::repo::StoreFactories` -> `jj_lib::default_backend_factories::default_backend_factories`, `default_working_copy_factories` moved out of `jj_lib::workspace`, and a new `jj-core` split crate appeared. This is exactly the caveat in the note below - the minimal surface "does not yet prove out the churny surface the five reads would need" - and it churned anyway.
> - **(c) a concrete pain point recurs - false.** No v0.8.1-class scraping / `trunk()`-resolution bug recurred; the drift it came from is now structurally prevented by the shared `trunk` module (ADR 0007 note, 2026-07-09), so the CLI `-T` path remains the correct, more stable contract.
> - **Corroboration in-repo:** the 0.46 bump itself cost follow-up commits on 2026-10-12/13 - `Upgrade jj and jj-lib to 0.46.0 in lockstep`, `Preserve non-colocated workspace creation under jj 0.46`, `Harden wsg-core workspace-list parsing for jj 0.46`, `Fix the Worker undo command for jj 0.46`, `Create colocated workspaces by default (jj 0.46 colocation)` - plus the later `Detect and surface a jj/jj-lib version mismatch`.
>
> **Decision:** gate still red; ticket stays `needs-triage` (the repo's parked state - no separate "parked" status exists). Do **not** start the migration. Reassess at the next `jj` pin bump.

> Re-triage (2026-07-08, ticket 11): `jj-lib` is now in the tree - the commit graph reads the store through it (ADR 0007 note). This ticket's gate assumed no jj-lib dependency yet; that premise no longer holds. The graph uses only a minimal read surface (open + heads/bookmarks/wc + parent walk) and avoids the revset engine, so it does not yet prove out the churny surface the five reads below would need. Reassess the gate against the churn actually observed at the next `jj` pin bump before promoting this back to `ready-for-agent`/`-human`.


## Gate (read this first)

**This ticket only makes sense once `jj-lib` is a stable-enough dependency to
live with.** Today it is not: jj publishes `jj-lib` and `jj-cli` from one
workspace at the same version with **no semver guarantees**, and the library API
churns every release (spike 02 already burned build iterations on 0.43-only
signatures - `RepoLoader::load_at_head()` became `async`, `ObjectId` trait
imports, etc.). Adopting it now means eating an API migration on **every** `jj`
version bump, in lock-step, forever.

Do not start this until at least one of:

- jj-lib ships a documented stability / semver policy (or a "stable" subset), OR
- the read surface we depend on has gone N consecutive `jj` releases without a
  breaking change to the calls listed below, OR
- a concrete pain point (a scraping/`trunk()`-resolution bug like v0.8.1) recurs
  and outweighs the churn cost.

Until then the CLI-`-T` path is the correct, more stable contract. Revisit at
each `jj` pin bump; if the gate is still red, leave this ticket parked.

## Background

Every external command now flows through the capturing `crate::cmd` runner
(landed alongside the forge-TUI-leak fix). That runner is the right seam
*regardless* of this ticket: `gh`, `jj-spr`, `kitten`, and `gpg` will always
shell out. What this ticket removes is the fragility specific to reading **jj**
state through CLI templates and revset scraping - the class of bug behind v0.8.1
(a mis-resolved `trunk()` diff base). ADR 0007 already earmarked reads for
jj-lib "in later epics"; spike 02 proved jj-lib 0.43.0 opens this repo's `.jj/`
store and reads the commit graph + working-copy state cleanly (see
`.scratch/rebuild/spike-02-jj-lib-findings.md` for the working read pattern).

## Scope: reads only

In scope - the pure reads currently scraped from CLI stdout/templates:

- `jj.rs::workspace_names` - `jj workspace list -T ...` -> `view` / workspace names.
- `jj.rs::count` - `jj log -r <revset> ...` -> evaluate revset, count commits.
- `work.rs::jj` reads - work-lifecycle templates, `diff --stat`, `git remote list`.
- `forge.rs::config_get` - `jj config get <key>` -> jj-lib config stack.
- `forge.rs::has_conflict` - `jj log -r "@ & conflicts()"` -> commit conflict state.

Explicitly **out of scope** (stay on the CLI for now - separate future tickets):

- Mutations: `rebase`, `abandon`, `workspace add`/`forget` (transaction /
  operation-log ownership, `--skip-emptied` / `--ignore-immutable` semantics).
- Git ops: `jj git fetch` / `push` (remote auth, refspecs, sign-on-push).
- Non-jj tools: `gh`, `jj-spr`, `kitten`, `gpg` (no jj-lib equivalent).

## Design constraints

- **Version lock-step.** `jj-lib` version must equal the `jj` pin in `mise.toml`;
  a `jj` bump becomes a jjfx change that bumps both together and only lands when
  `mise run check` is green (spike 02's pin note).
- **Revset aliases live in config.** `trunk()`, `mine()`, and the `tidy`/`tidyws`
  aliases resolve via the user's revset-alias config, *not* the library core -
  load that config or the symbols won't resolve. This is the main correctness
  trap; cover it with a test against a repo whose `trunk()` is not `main`.
- **Blocking API on `spawn_blocking`.** `RepoLoader::load_at_head()` is `async` in
  jj-lib; wrap the block-on inside the existing `spawn_blocking` calls, matching
  the engine plan. Never snapshot the working copy from a read path.
- **Keep the `cmd` runner.** This does not delete `crate::cmd`; it only stops
  routing jj *reads* through it. gh/jj-spr/kitten/gpg still use it.

## Acceptance criteria

- [ ] The five read sites above resolve their state via jj-lib, with no CLI
      subprocess and no template/stdout parsing.
- [ ] `trunk()` (including the never-pushed / non-`main` case that caused v0.8.1)
      resolves correctly, proven by a test on a repo where trunk != `main`.
- [ ] `jj-lib` version is pinned equal to the `jj` pin; `mise run check` is green.
- [ ] Mutations and git ops still go through the CLI unchanged; `crate::cmd`
      still serves gh/jj-spr/kitten/gpg.
- [ ] No user-visible behaviour change (this is an internal read-path swap).

## Blocked by

- The **Gate** above (jj-lib stability) - the hard precondition.
- `.scratch/rebuild/issues/02-spike-jj-lib.md` (done - proof + read pattern).
