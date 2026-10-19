# Detect a jj-lib / jj binary version mismatch

Status: resolved
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

- [x] A version mismatch is visible, not silent.
- [x] A matching version is unaffected.
- [ ] `mise run check` is green.

## Out of Scope

- Auto-upgrading `jj` or `jj-lib`.

## Outcome (2026-10-13)

**Resolved.** `bef2b7` - `Detect and surface a jj/jj-lib version mismatch`.

The `jj-lib` target lives in source as `JJ_LIB_TARGET`, beside the existing
version probe, and `jj_lib_target_tracks_the_cargo_pin` reads `Cargo.toml` via
`include_str!` and asserts they agree - so a dependency bump recompiles the test
and fails until the constant moves. No build script, no new dependency, no
runtime manifest parsing. `compat_notice` is the pure comparison;
`jj_lib_compat_notice` is the probing wrapper (probed once per process).

Surfaced TUI-first, since the TUI is the only jj-lib store-read site: a warning
line atop the world graph (so a degraded read reads as degraded), a footer warning
that outranks the key hints, and a `warning:` on stderr for CLI repository
commands. `lib.rs` seeds it before the first load because `runtime::load_graph`
swallows load errors - so the notice is visible even when the read fails outright.
A missing or broken `jj` yields no notice by design (other paths report that
loudly), and the comparison is major/minor, matching the existing colocation probe.

Two landing notes:

- **The red is a compile error, not a behavioural failure.** The change is
  additive, so pre-fix there was no seam for a test to fail against. The agent said
  so plainly rather than dressing it up as a behavioural red.
- **The CLI warning sits in the shared router, so the frozen `wsg` binary emits it
  too.** Checked rather than assumed: `docs/wsg-compatibility-contract.md:153-155`
  reserves stderr for human messages and requires only *failures* to be contextual
  errors with a non-zero status, so a warning is compliant. It can only fire on a
  mismatch, which CI cannot reach because `mise` pins jj to 0.46.

Changelog: `(add)` under `### Added`.

## Resolution

Reconciled Mon 19 Oct 2026 against the merged code at `9d6bd0ce6e7b`.

This ticket was marked resolved without its acceptance boxes ticked. Every criterion was re-verified by an independent read-only review of the code and tests on `main`. The reconciliation recorded no new work; it confirms what shipped.

Evidence: `JJ_LIB_TARGET` is a source constant (`src/engine/workspace.rs:1405`) and `compat_notice` (`:1423`) is the pure major/minor comparison, wrapped by `jj_lib_compat_notice` (`:1417`). The mismatch is surfaced on the graph load (`src/graph.rs:213`) and on stderr for CLI commands (`src/lib.rs:313`). The "matching version is unaffected" half is pinned by `jj_lib_compat_notice_fires_only_on_a_mismatch` (`src/engine/workspace.rs:2027`), which asserts `None` for a match and for an unprobeable `jj`. The follow-on landing note about the (now-deleted) `wsg` compatibility contract is historical and does not affect the criteria.

Not ticked: the `mise run check` criterion. It is a landing-commit property, and this reconciliation deliberately does not run the full suite (the orchestrator runs the gate once at the end).
