# Fold wsg-core into jjfx and publish to crates.io

Status: claimed

## Problem Statement

`jjfx` cannot be published to crates.io. Its manifest depends on `wsg-core`
through a path-only dependency (`wsg-core = { path = "crates/wsg-core" }`), and
`cargo package` refuses to package a crate whose dependencies carry no version
requirement:

```
error: all dependencies must have a version requirement specified when packaging.
       dependency `wsg-core` does not specify a version
```

The manifest is also missing the metadata crates.io requires (`description`) and
expects (`repository`, `readme`, `keywords`, `categories`, `rust-version`).

Publishing `wsg-core` as a second crate would keep the migration-era name alive
and create a second public version of record for code with no independent
consumers. That code is the only reason the repository is not a single crate.

## Solution

Absorb `crates/wsg-core` into the `jjfx` crate as a private module tree,
`crate::engine`, and delete the crate. `crates/wsg` becomes a thin shim over the
`jjfx` library alone, so the `wsg` binary name survives (ticket 18) without a
second crate. With no path dependencies left, `jjfx` gains the metadata it is
missing and publishes as one crate.

## Commits

1. Absorb `crates/wsg-core` into `src/engine/`: move the 13 modules, declare
   `pub mod engine`, rewrite the 155 `wsg_core::` references in `src/` and the
   120 `crate::` references inside the moved modules, move the 13 integration
   tests and their fixtures into the root `tests/`, move the library's
   dependencies onto the `jjfx` manifest, delete the crate, repoint `crates/wsg`
   at `jjfx::engine`, mark it `publish = false`, and trim the workspace members.
2. Add the crates.io package metadata and verify with `cargo publish --dry-run`.
3. Update the compatibility and conformance docs to the new layout, add a
   CHANGELOG entry, and note the `cargo install jjfx` path in the README.

## Decision Document

- `wsg-core` is not published and does not survive as a crate; the name retires
  with the migration. The `wsg` **binary** name is unaffected (ticket 18).
- The absorbed code lives under `crate::engine`, a private module tree of
  `jjfx`. `engine` is chosen over `core` so it cannot shadow Rust's `core` crate
  in `use core::...` paths.
- Module names inside `engine` keep their existing names (`layout`, `runtime`);
  they no longer collide because they are nested.
- `crates/wsg` is `publish = false`: buildable from the repository, keeps the
  compatibility binary, but is not a crates.io artifact.
- `jjfx` stays lib+bin and publishes both. The version of record stays
  `Cargo.toml`.
- Publishing itself (token, tag, release notes) is an owner action.

## Testing Decisions

The existing suites move with the code and keep running under
`cargo test --workspace`. wsg-core's 13 integration tests become jjfx's `tests/`
and continue to locate fixtures through
`CARGO_MANIFEST_DIR/tests/fixtures`, which still resolves. `cargo publish
--dry-run` is the gate for the packaging change: it validates the manifest and
builds the tarball without publishing.

## Acceptance Criteria

- [ ] `cargo package --no-verify` succeeds with no path-dependency error.
- [ ] `cargo publish --dry-run` completes clean.
- [ ] `crates/wsg-core` no longer exists; `cargo metadata` lists `jjfx` and
      `wsg` only.
- [ ] The `wsg` binary still builds and reports its own version.
- [ ] `mise run check` is green.

## Out of Scope

- Running `cargo publish` (owner action)
- Changing the `wsg` binary name or its command surface
- Publishing `wsg` to crates.io
- The Go repository deprecation (ticket 18)
