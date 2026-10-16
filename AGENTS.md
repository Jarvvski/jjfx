# AGENTS.md - jjfx project memory

## Landing a change (jj, NOT git)

The owner uses **Jujutsu exclusively** in a colocated repo. **Never invoke `git`** - it can corrupt jj's operation log. jj has no staging area; the working copy is always a commit.

1. Make **one focused change** per commit.
2. Run `mise run check` (fmt + lint + build + test); land only when it passes. **Docs-only changes - markdown, symlinks, or agent-config edits that touch no code - skip this gate.** (jj fires no git hooks, so fmt is a task, not a pre-commit hook.)
3. For user-visible changes, bump the version + add a dated `CHANGELOG.md` entry in the *same* commit.
4. `jj describe -m "<imperative one-liner>"`.

**Every change reaches `main` through a Pull Request - never push `main` directly.** `main` is protected: agents publish work on a bookmark and open a PR against it.

- `jj bookmark create <name> -r @` (or `jj bookmark set <name> --to @` to move an existing bookmark).
- `jj git push --bookmark <name>`.
- `gh -R Jarvvski/jjfx pr create --base main ...`.

This applies even when the owner invokes `$land` exactly: landing means opening the PR, and it overrides any generic skill contract that treats `$land` as licence to push straight to `main`. **Agents must not `jj bookmark set main --to @` or `jj git push --bookmark main`** - a direct commit to `main` is the thing this flow replaces.

**Merging.** Agents may merge their own PR once the applicable gate is green - `mise run check` for code, nothing for a docs-only change - and any required review has passed (`gh -R Jarvvski/jjfx pr merge <n>`), then delete the source bookmark with `jj bookmark delete <name>` and `jj git fetch` so local `main` tracks the merged result. Leave the PR open for the owner instead of merging when a gate is unmet - a red check, an unresolved review thread, or a required human decision.

The one exception to the never-push-`main` rule is the `Release` workflow (`.github/workflows/release.yml`),
which the owner dispatches and which pushes its own bump commit and tag using
`GITHUB_TOKEN`. That is owner-configured automation, not an agent push - do not
"fix" it, and do not take it as licence to push `main` yourself.

**Versioning & changelog.** Versioning and the changelog are momo's job (the CLI is pinned in `mise.toml`; see [`docs/releasing.md`](docs/releasing.md)). The version of record is the **latest tag** (`v<x.y.z>`), and it is **release-owned**: only the release workflow moves it (via `momo release`). Do not bump it by hand for an ordinary change.

A user-visible change is recorded with momo, which owns the changelog heading and the section it lands in:

- `momo break "<description>"` - an incompatible change
- `momo add "<description>"` - new functionality, backwards compatible
- `momo fix "<description>"` - a backwards-compatible bug fix

`momo status` derives the next version from the unreleased changes and the latest tag. jjfx is pre-1.0, so the minor position is the breaking slot (Cargo's `^0.52.0` means `>=0.52.0, <0.53.0`): `break` and `add` both raise the minor, `fix` raises the patch - momo's zero-aware rule. **Never bump to 1.0.0 (or any MAJOR) without the owner's explicit approval.** Skip pure internal refactors.

`crates/wsg` is frozen at its current version while the migration-era binary is retired; entries are versioned by `jjfx` alone.

Releases are cut by dispatching `.github/workflows/release.yml` - see
[`docs/releasing.md`](docs/releasing.md).

Remote: `origin` = `Jarvvski/jjfx` (SSH: `git@github.com:Jarvvski/jjfx.git` - SSH avoids the OAuth `workflow`-scope gate on pushing `.github/workflows/`). `gh` auto-detection fails in jj workspaces - always pass `-R Jarvvski/jjfx`.

## Ticket execution scope

When the user asks to pick or implement a ticket, the entire remaining ticket
is the default scope for that agent session.

- Plan and complete every remaining item under `## Commits`.
- Satisfy every acceptance criterion before reporting the ticket complete.
- Treat commit-list items as focused commits and TDD cycles within the same
  session, not as separate session boundaries.
- Do not stop after the first green TDD slice or focused commit.
- For a ticket with multiple focused commits, describe each completed commit
  and start the next with `jj new`, but do not move `main` between them. After
  the whole ticket passes, move `main` to the stack tip and immediately run
  `jj new`.
- Update the ticket status to `resolved` only after `mise run check` passes.
- If the ticket cannot reasonably fit in one session, identify that before
  editing and ask the user to split it. Do not silently implement only the
  first slice.
- After implementation starts, stop early only for a hard blocker,
  contradictory requirements, or a required human decision.

## Toolchain

**mise** owns tasks and the Rust pin (see `mise.toml`). Use the tasks, don't hand-roll cargo:

- `mise run check` - the pre-land gate for code changes: changelog validation + fmt + lint + build + test (docs-only changes skip it).
- `mise run run` / `build` / `test` / `fmt` / `lint` - individual steps.
- `momo status` / `momo break|add|fix` / `momo release` / `momo tag` / `momo notes` - the versioning and changelog tooling (pinned in `mise.toml`).

Rust edition 2024. `Cargo.lock` is committed (this is an app, not a library).

## Rust conventions

**Build loop.** After any substantive change, run `mise run check`; land only when green. Use `cargo check` for fast iteration and run the full test suite only when logic changed. Clippy runs **warnings-as-errors** - fix the root cause, never paper over a lint with a scattered `#[allow(...)]` (if a lint must be disabled, do it locally with a comment saying why). Leave no `dbg!`, stray `println!`, or commented-out code behind.

**Correctness.** Code that compiles is not code that is correct - Rust stops memory bugs, not logic bugs. Verify behavior; don't assume "it compiled so it works." Read compiler and clippy spans in full and apply the suggested fix - they are precise.

**Error handling.** No `.unwrap()` outside tests; use `.expect("why")` only for true invariants, with a message explaining why it cannot fail. Propagate with `?`; return `Result<T, E>` for anything fallible. This is a binary - use `anyhow` + `.context(...)` at the top level; define a `thiserror` enum only when callers need to match on specific error variants. Prefer `Option<T>` over sentinel values.

**Ownership (the #1 agent pitfall).** Do NOT reach for `.clone()`, `Rc<RefCell<_>>`, or `Arc<Mutex<_>>` just to silence the borrow checker - restructure ownership/lifetimes instead. When a closure fights the borrow checker, add `move` and adjust ownership. Never return a reference to a local/temporary; return owned data if lifetimes get hard. A `.clone()` on a non-`Copy` type is a conscious decision, not a reflex.

**Idioms.** Prefer iterators/combinators over manual index loops (`enumerate()`, `if let`/`while let`). Borrow in signatures (`&str`, `&[T]`); return owned only when needed. Implement std traits (`From`/`TryFrom`/`Display`/`Default`) over ad-hoc `to_x`/`make_x` methods; use newtypes to distinguish semantically different values. Match exhaustively; avoid a catch-all `_` arm when a new enum variant should force a compile error.

**Testing.** Unit tests live inline in a `#[cfg(test)] mod tests { use super::*; }` block; integration tests go in `tests/`. Cover the happy path, edges (empty/boundary), and error conditions for new logic.

**Unsafe.** Forbidden crate-wide (`unsafe_code = "forbid"` in `Cargo.toml`) - `unsafe` is a hard compile error, not a lint. Do not remove the forbid to work around a problem; find a safe design instead (if `unsafe` ever becomes genuinely unavoidable, that's an owner decision). TUI note: never let a panic corrupt the terminal - rely on the restore-on-panic hook, don't `unwrap` in the render/event loop.

**Layout & style.** rustfmt defaults; no hand-formatting. No wildcard imports (except a deliberate prelude); group std / external / local and reference intra-crate items via `crate::`. Doc-comment (`///`) public items; omit comments that merely restate the code. Revise files in place - no `main_v2.rs` / `foo_improved.rs` variants.

## Agent skills

### Issue tracker

Issues and PRDs live as local markdown under `.scratch/<feature>/` (no remote tracker). See `docs/agents/issue-tracker.md`.

### Triage labels

Default vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.
