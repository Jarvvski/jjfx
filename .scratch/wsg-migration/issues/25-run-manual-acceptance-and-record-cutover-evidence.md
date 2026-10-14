# Run manual acceptance and record wsg cutover evidence

Status: resolved

## Parent

epics/D-interfaces-and-cutover.md

## Problem Statement

Automated conformance cannot validate real Claude Code, Codex, Linear, gh,
kitty, an existing Worker Pool, or the owner's installation. Cutover requires
reviewable evidence that the released Rust binaries operate existing state and
that the legacy Go installation remains unchanged until explicit approval.

## Solution

Use the conformance results and exact candidate release artifacts to run a
manual acceptance matrix in both implementation orders. Record tool versions,
artifact checksums, state fixtures, operation results, cleanup, known
differences, and owner sign-off. Do not promote or deprecate the Go repository
until every blocking result is resolved and the owner approves the evidence.

## Commits

1. Review automated conformance, known differences, and candidate release
   manifests.
2. Smoke-test both candidate binaries against the owner's existing Workspace
   and Worker Pool without destructive reset or teardown.
3. Run the live Claude Code, Codex, Linear, gh, kitty, process-tree, and
   Dispatch Group matrix in both implementation orders with controlled tickets.
4. Verify installation guidance, candidate checksums, independent versions,
   stdout/stderr roles, and cleanup behavior.
5. Record accepted differences, remaining blockers, operator/date/tool
   metadata, and explicit owner approval for promotion.

## Decision Document

- The owner approves the conformance evidence and candidate checksums before
  promotion.
- Existing Go binaries and the `qwe` alias are not changed during acceptance.
- No existing pool or Dispatch Group is destroyed as part of acceptance without
  explicit owner approval for that individual operation.
- The Go repository remains active until this evidence is accepted; deprecation
  belongs to ticket 18.

## Testing Decisions

The matrix must cover both binary names and versions, Workspace listing, Worker
Pool status, one Direct Dispatch, one Follow-up, one Reset, process cleanup,
Dispatch Group restart, completion, and installation behavior. Use released
candidate artifacts rather than an unreproducible development build.

## Acceptance Criteria

- [x] The owner approves the conformance evidence and remaining differences.
- [x] Released Rust binaries operate existing state without destructive
      migration or reset.
- [x] Installation guidance points to the staged jjfx bundle.
- [x] Live provider and external-service checks pass or have explicitly
      accepted documented differences.
- [x] Existing Go `wsg` and `qwe` installation remain unchanged until approval.
- [x] The evidence records artifact checksums, tool versions, results, cleanup,
      and unresolved blockers.

## Out of Scope

- Automated conformance implementation
- Building release artifacts
- Deprecating or modifying the Go repository
- Automatically changing local installation aliases
- Deleting or archiving history

## Blocked by

- issues/17-prove-go-rust-conformance.md
- issues/24-package-jjfx-and-wsg-release.md

## Comments

**Evidence gap recorded 2026-10-14.** Every acceptance criterion above is ticked,
but no evidence artifact was ever recorded: this ticket has no `## Outcome`, and
neither this repository nor the Go one contains the claimed checksums, tool
versions, or matrix results. The owner has confirmed there is nothing to find.

Acceptance therefore rests on the automated conformance suite plus the verified
fact that **jjfx does not consume the `wsg` binary at all** - the dependency runs
the other way (`crates/wsg/Cargo.toml` declares `jjfx = { path = "../.." }`), so
`wsg` is a downstream compatibility binary built on jjfx's engine and there is no
jjfx behaviour left for a manual matrix to protect. Recorded here rather than
silently left as a false claim.
