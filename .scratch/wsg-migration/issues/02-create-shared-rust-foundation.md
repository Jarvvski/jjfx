# Create the shared Rust foundation and wsg binary skeleton

Status: ready-for-agent

## Parent

epics/A-contract-and-coexistence.md

## Problem Statement

jjfx is currently one binary package whose modules are private to its executable. Workspace Dispatch behavior must be callable by both jjfx and a retained wsg command without importing terminal rendering, duplicating state transitions, or coupling the shared implementation to one frontend.

## Solution

Turn the repository into a Cargo workspace with a shared Workspace Dispatch library and a skeletal wsg CLI package while preserving the existing jjfx package and behavior. Establish a small initial public interface for opening a Repository and reporting that migration capabilities are not yet implemented. Keep CLI parsing, stdout/stderr rendering, and Ratatui outside the shared library.

## Commits

1. Introduce the Cargo workspace while leaving the existing jjfx package build and run behavior unchanged.
2. Add an empty shared library package with crate-wide lint, safety, and documentation conventions matching jjfx.
3. Add a wsg CLI package that supports help and version without Repository access.
4. Add a minimal Repository-opening interface and typed error context to the shared library.
5. Add compile-time and smoke tests proving jjfx and wsg can depend on the shared package without circular dependencies.
6. Update mise tasks so formatting, linting, building, and testing cover every workspace package.

## Decision Document

- The repository produces independent `jjfx` and `wsg` binaries.
- Both binaries depend on one shared Workspace Dispatch library.
- The shared library does not parse command-line arguments or render terminal output.
- The shared library returns typed data and errors; frontends decide presentation.
- Tokio may be used for long-running execution, but simple domain and persistence operations remain usable without a TUI.
- No wsg behavior is claimed until a later parity ticket implements it.

## Testing Decisions

Test the package interfaces as callers use them. Add binary smoke tests for help and version, plus a shared-library test for Repository discovery errors. Avoid testing Cargo layout details beyond successful workspace checks.

## Acceptance Criteria

- [ ] Existing jjfx behavior and tests remain unchanged.
- [ ] One workspace command checks all packages.
- [ ] Both binary names build successfully.
- [ ] The wsg skeleton works outside a jj Repository for help and version.
- [ ] The shared library has no Ratatui or CLI-rendering dependency.
- [ ] `unsafe_code = "forbid"` remains enforced.
- [ ] `mise run check` is green.

## Out of Scope

- Persisted wsg state
- Worker Pool behavior
- Full wsg command compatibility
- jjfx TUI changes

## Blocked by

- issues/01-adopt-dispatch-domain.md
