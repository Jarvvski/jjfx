# Detect a jj-lib / jj binary version mismatch

Status: ready-for-agent
Type: task
Blocked by: none

## Improvement

`Cargo.toml` pins `jj-lib = 0.46.0`, and `graph::load` reads the on-disk store
with it, but only the colocation path probes `jj --version`
(`engine/workspace.rs:1035-1044`). A newer or older `jj` on `PATH` can silently
degrade the graph or misread the store with no visible signal. The README and
CONTRIBUTING both warn that the two are "version-coupled", but nothing enforces
or reports it.

## Fix

At graph load (and any other jj-lib store read), compare the `jj` on `PATH`
against the version `jj-lib` targets and **degrade with a visible notice**
rather than silently producing an empty/partial graph.

## Commits

1. Probe and compare at store-read time; expose the mismatch.
2. Surface it in the TUI/CLI.
3. Test: a mismatched `jj` produces a notice, not an empty graph.
4. CHANGELOG entry `(add)`.

## Acceptance Criteria

- [ ] A version mismatch is visible, not silent.
- [ ] A matching version is unaffected.
- [ ] `mise run check` is green.

## Out of Scope

- Auto-upgrading `jj` or `jj-lib`.
