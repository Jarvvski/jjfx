# CLAUDE.md - jjfx project memory

## Landing a change (jj, NOT git)

The owner uses **Jujutsu exclusively** in a colocated repo. **Never invoke `git`** - it can corrupt jj's operation log. jj has no staging area; the working copy is always a commit.

1. Make **one focused change** per commit.
2. Run `mise run check` (fmt + lint + build + test); land only when it passes. (jj fires no git hooks, so fmt is a task, not a pre-commit hook.)
3. For user-visible changes, bump the version + add a dated `CHANGELOG.md` entry in the *same* commit.
4. `jj describe -m "<imperative one-liner>"` then `jj bookmark set main --to @` then `jj new`.
5. Invariant: every time `main` moves, the next command is `jj new`, so `@` is always an empty commit one above main.

**Versioning & changelog.** Keep a single **version of record** (e.g. the manifest's version field) and treat it as authoritative. Semver: PATCH for fixes, MINOR otherwise; **never bump to 1.0.0 (or any MAJOR) without the owner's explicit approval - do not auto-bump.** A user-visible change adds a dated `CHANGELOG.md` entry (newest first) in the same commit; skip pure internal refactors. Push with `jj git push --bookmark main` (`--allow-new` the first time a bookmark is pushed).

Remote: `origin` = `Jarvvski/jjfx` (SSH: `git@github.com:Jarvvski/jjfx.git` - SSH avoids the OAuth `workflow`-scope gate on pushing `.github/workflows/`). `gh` auto-detection fails in jj workspaces - always pass `-R Jarvvski/jjfx`.

## Toolchain

**mise** owns tasks and the Rust pin (see `mise.toml`, `rust-toolchain.toml`). Use the tasks, don't hand-roll cargo:

- `mise run check` - the pre-land gate: fmt + lint + build + test.
- `mise run run` / `build` / `test` / `fmt` / `lint` - individual steps.

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
