# Lock down wsg compatibility contracts

Status: resolved

## Parent

epics/A-contract-and-coexistence.md

## Problem Statement

The migration promises that Go and Rust can share an existing Worker Pool, but the effective interface includes exact files, optional JSON fields, locks, timestamps, process semantics, and command output that are not captured in one executable specification. A plausible Rust model could still corrupt or reinterpret Go state.

## Solution

Create language-neutral fixtures and black-box contract tests from representative Go-created state. Specify exact parsing, rendering, omission, nullability, ordering, lock naming, stdout/stderr, and status-transition behavior before Rust performs mutations.

## Commits

1. Inventory every persistent and command-facing compatibility surface used by wsg and jj-wsx.
2. Add golden fixtures for an empty pool and idle Worker.
3. Add fixtures for busy, done, and failed Workers using both Claude Code and Codex fields.
4. Add fixtures for Dispatch Groups in pending, dispatched, retried, done, failed, and merged combinations.
5. Add fixtures for ws-cache ordering, missing paths, whitespace, and malformed lines.
6. Add contract tests that deserialize all fixtures into typed Rust values without data loss.
7. Add round-trip tests that require exact semantic field presence, especially explicit null versus omitted fields.
8. Add a command contract inventory for stdout, stderr, aliases, options, and exit outcomes.
9. Document lock paths, atomic replacement expectations, and cross-process ownership rules as part of the tested contract.

## Decision Document

- Existing wire formats are frozen for the migration.
- Additive fields are allowed only when old consumers ignore them safely.
- Rust readers accept the same tolerated legacy states as Go.
- Rust writers emit state consumable by current Go wsg and jj-wsx.
- Golden fixtures contain data, not copied implementation code.
- CLI compatibility is behavioral; cosmetic ANSI differences are not byte-for-byte requirements unless scripts consume them.

## Testing Decisions

Golden tests exercise public serialization and command contracts. Test empty, missing, null, malformed, and legacy inputs. Keep fixtures small and named by domain scenario. Later tickets must add their implementation to these tests rather than creating separate incompatible fixtures.

## Acceptance Criteria

- [x] Every persisted wsg format has representative provisional fixtures.
- [x] Explicit null and omitted-field behavior is asserted.
- [x] Lock mutation scopes and atomic replacement rules are documented; exact Go filenames remain source-validation work.
- [x] The current Rust foundation command and output surface is inventoried; the full Go command inventory remains source-validation work.
- [x] No Rust mutation is enabled by this ticket.
- [x] `mise run check` is green.

## Out of Scope

- Implementing state mutation
- Redesigning schemas
- Requiring exact ANSI rendering parity

## Comments

2026-07-21 - Added the provisional contract document and golden fixtures in
`crates/wsg-core/tests/fixtures/compatibility/`. The Go wsg/jj-wsx source is not
present in this repository, so exact lock filenames and the complete historical
CLI inventory are explicitly deferred until that source is available. No Rust
mutation is enabled.

## Blocked by

- issues/02-create-shared-rust-foundation.md
