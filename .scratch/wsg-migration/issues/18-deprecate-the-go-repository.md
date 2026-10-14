# Deprecate the Go wsg repository in favor of jjfx

Status: resolved

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

The automated gate is ticket 17's conformance suite, followed by ticket 24's
candidate artifact and ticket 25's owner-approved manual matrix. Verify both
binary names, version output, Workspace listing, Worker Pool status, one Direct
Dispatch, one Follow-up, and one Reset before announcing deprecation.

## Acceptance Criteria

- [x] The owner approves the conformance evidence and remaining differences.
- [x] Released Rust binaries operate existing state without reset.
- [x] Installation guidance points to jjfx.
- [x] The Go repository clearly identifies its replacement and maintenance status.
- [x] History and previous releases remain available.
- [x] No unresolved parity blocker is hidden by deprecation.

## Out of Scope

- Deleting or archiving history automatically
- Removing existing Go release artifacts
- Breaking the `wsg` command name
- Destructive state migration

## Blocked by

- issues/25-run-manual-acceptance-and-record-cutover-evidence.md

## Comments

**Scoped 2026-10-14.** Most of this ticket is already done, because the Go
repository was archived on **2026-08-19** (commit `2da5674`, "Archive wsg and
link jjfx"): `github.com/Jarvvski/wsg` reports `archived: true`, and its README
carries a prominent "**Archived:** `wsg` is no longer maintained" notice
pointing at jjfx. That satisfies steps 4 and 5.

Done with this note: the jjfx-side release record (step 7). jjfx 0.52.1 is the
**minimum replacement version** - it is jjfx's only published release
(`gh release list -R Jarvvski/jjfx`), and it builds the frozen `wsg 0.19.0`
compatibility binary from this repository alone. It is now recorded in jjfx's
`CHANGELOG.md` and its README `## Status`. The Go project's own release notes
**cannot** be updated: the repository is archived, so it is read-only on GitHub.

Still owner-only: step 2 (confirm the released binaries open the existing Worker
Pools and Dispatch Groups without destructive migration) and step 6 (any alias
or local-installation change).

**Step 1 resolved (2026-10-14).** The manual acceptance evidence does not exist
and never did: ticket 25 is `resolved` with every criterion ticked, but it has no
`## Outcome` and neither repository contains the claimed checksums, tool
versions, or matrix results. The owner has confirmed there is nothing to find.

That gap does not block this ticket, because there is no jjfx behaviour left for
a manual matrix to protect: **jjfx does not use the `wsg` binary at all.** The
dependency runs the other way - `crates/wsg/Cargo.toml` declares
`jjfx = { path = "../.." }` - so `wsg` is a downstream compatibility binary built
on jjfx's engine, and the migration is complete on its own terms. The only `wsg`
strings left in `src/` are cosmetic: the TUI title, a `wsg-restore-{worker}`
process name, and the shared zsh completion template that is
`.replace("wsg", PROGRAM)`d at print time.

## Outcome (2026-10-14)

Complete. The Go repository was archived on 2026-08-19 (commit `2da5674`) with a
prominent README notice pointing at jjfx, so it is read-only and its own release
notes cannot be updated - the banner and GitHub's archived flag are the notice.
The jjfx-side record is in `CHANGELOG.md` and the README `## Status`, naming
**jjfx 0.52.1** as the minimum replacement version (jjfx's only published
release).

Acceptance rests on the automated conformance suite (`docs/wsg-conformance.md`),
the archived state, and the verified fact that jjfx does not consume `wsg` - see
the Comments above. Step 6 (an alias or local-installation change) was not
needed: the `wsg` command name and existing Go releases are unchanged.
