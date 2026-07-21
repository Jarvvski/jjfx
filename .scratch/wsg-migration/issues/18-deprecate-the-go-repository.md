# Deprecate the Go wsg repository in favor of jjfx

Status: ready-for-human

## Parent

epics/D-interfaces-and-cutover.md

## Problem Statement

After the Rust wsg binary reaches parity, maintaining two active implementations would split fixes, confuse installation, and eventually break shared-state compatibility. Deprecation must still preserve history, communicate the replacement clearly, and avoid stranding existing users or Worker Pools.

## Solution

After owner review of the parity evidence and replacement release, make jjfx the authoritative home of both jjfx and wsg. Publish migration guidance, switch installation references, issue a final Go release or notice as appropriate, and mark the Go repository maintenance-only without deleting its history.

## Commits

1. Review the conformance report, known differences, live-provider matrix, and release artifacts.
2. Confirm the Rust release opens and operates the owner's existing Worker Pools and Dispatch Groups without destructive migration.
3. Publish jjfx installation and upgrade guidance for both binaries.
4. Add a prominent deprecation notice to the Go repository pointing to jjfx and the replacement release.
5. Clarify that existing Go releases remain available but receive no new feature development.
6. Update aliases or local installation only after explicit owner confirmation.
7. Record the deprecation date and minimum replacement version in both projects' release notes.
8. Close or migrate any remaining active Go implementation work that still applies.

## Decision Document

- The Go repository is deprecated, not deleted.
- Repository history and released binaries remain available.
- jjfx is the source of future wsg development.
- The replacement remains a `wsg` binary for scripting compatibility.
- No pool destruction or state reset is part of migration.
- This ticket requires human approval because it changes project authority and release guidance.

## Testing Decisions

The automated gate is ticket 17's conformance suite. This ticket adds manual installation and existing-state smoke checks using the released artifacts. Verify both binary names, version output, Workspace listing, Worker Pool status, one Direct Dispatch, one Follow-up, and one Reset before announcing deprecation.

## Acceptance Criteria

- [ ] The owner approves the conformance evidence and remaining differences.
- [ ] Released Rust binaries operate existing state without reset.
- [ ] Installation guidance points to jjfx.
- [ ] The Go repository clearly identifies its replacement and maintenance status.
- [ ] History and previous releases remain available.
- [ ] No unresolved parity blocker is hidden by deprecation.

## Out of Scope

- Deleting or archiving history automatically
- Removing existing Go release artifacts
- Breaking the `wsg` command name
- Destructive state migration

## Blocked by

- issues/17-prove-parity-and-release-from-jjfx.md
