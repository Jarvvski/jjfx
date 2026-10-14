# Run Pi manual acceptance

Status: wontfix

## Parent

pi-support effort (standalone; no PRD)

## Problem Statement

Fake executables and sanitized fixtures can prove jjfx adapter behavior, but
cannot prove that the released integration works with the owner's real Pi
installation, authentication, configured extensions, trust settings, model,
Linear access, filesystem policy, terminal, and session files. The same is
true for validating that adding Pi did not regress existing Claude or Codex
workflows.

Manual acceptance must exercise the full user path without destroying existing
Worker Pool, Workspace, session, or hook state. It must record enough version,
configuration, command, result, cleanup, and owner-signoff information to make
remaining differences reviewable.

## Solution

Run a non-destructive acceptance matrix against the released jjfx artifact and
the owner's installed Pi, Claude, and Codex tools after issues 02-05 are
resolved. Use isolated or explicitly approved tickets, workspaces, temporary
configuration where possible, and a controlled repository state. Record
sanitized evidence under `.scratch/pi-support/`, including tool versions,
configuration sources, observed results, cleanup, known limitations, and
explicit owner approval.

The matrix must cover both Pi's interactive lifecycle and Worker runtime:

- installation/status and configuration discovery;
- interactive launch, working/waiting/ended lifecycle, session switching, and
  TUI identity;
- safe ticket discovery and capability failures, if enabled;
- fresh Direct Dispatch and completion/failure logs;
- Dispatch Group selection/progression if available;
- Follow-up/send, session resume, interactive mount, and fresh-session fallback;
- Logs, current activity, usage/cost visibility, Reset, process cleanup, and
  workspace restoration; and
- representative Claude and Codex regression paths.

## Commits

1. Review automated tests, compatibility fixtures, release artifacts, known
   differences, and the exact manual matrix before touching a live workspace.
2. Record the installed jjfx, Pi, Claude, Codex, jj, gh, terminal, and any
   discovery-helper versions plus artifact checksums and configuration sources.
3. Verify hook behavior (there is no `jjfx hooks status` command - hooks are
   repository files; see `docs/workspace-hooks.md`) and install behavior, then
   smoke-test Pi interactive launch and lifecycle display without overwriting
   unrelated provider configuration.
4. Run a controlled Pi fresh Dispatch, observe safe permissions and ticket
   inputs, inspect structured activity/result logs, and verify success and
   failure cleanup.
5. Run Pi Follow-up/send, session resume, interactive mount, fresh-session
   fallback, Logs, Reset, process-descendant cleanup, and Workspace restoration
   using non-destructive test state.
6. Exercise Pi ticket discovery and Dispatch Group behavior when the selected
   capability is configured; record explicit unsupported/setup outcomes when
   it is not available.
7. Re-run representative Claude and Codex interactive, Dispatch, log,
   follow-up, mount, and reset scenarios and compare state/cleanup behavior.
8. Record sanitized evidence, accepted differences, unresolved blockers,
   cleanup confirmation, and explicit owner sign-off.

## Decision Document

- Manual acceptance uses released candidate artifacts and records checksums;
  an unreproducible development binary is insufficient evidence.
- No existing pool, workspace, session, hook file, or provider installation is
  reset, deleted, or migrated without owner approval for that operation.
- Real credentials and private prompts must never be copied into scratch
  evidence. Record configuration source and capability outcome, not secrets.
- A missing Pi discovery capability is an explicit accepted difference only if
  the owner signs off; it is not converted into silent Claude/Codex fallback.
- Existing Claude and Codex behavior must be checked after Pi acceptance, even
  when their automated tests remain green.
- A failed cleanup or stale process is a blocking acceptance result until
  resolved and re-tested.

## Testing Decisions

Use a controlled repository/ticket and isolated workspace whenever the feature
allows it. Prefer temporary Pi session/configuration/trust directories and
non-destructive Worker Pool operations. For existing state, perform read-only
smoke tests first and capture a before/after inventory. Use bounded timeouts,
verify descendants are gone after Reset, and confirm the terminal and workspace
are usable after every failure. Manual evidence should identify operator/date,
platform, versions, commands or UI actions, expected result, observed result,
cleanup, and status for every matrix row.

## Acceptance Criteria

- [ ] The owner approves the recorded matrix, evidence, accepted differences,
      and any remaining non-blocking limitations.
- [ ] Released jjfx artifact checksums and versions for jjfx, Pi, Claude,
      Codex, jj, gh, and the terminal are recorded.
- [ ] Pi interactive lifecycle installation/status, launch, identity, working,
      waiting, ended, session-switch, and cleanup behavior pass.
- [ ] Pi fresh worker execution produces the expected safe tool/approval
      behavior, structured activity/result logs, terminal outcome, and state
      finalization.
- [ ] Pi ticket discovery and Direct Dispatch pass, or an unavailable
      capability has a documented owner-approved setup/unsupported result.
- [ ] Pi Follow-up/send, session resume, interactive mount, fresh fallback,
      Logs, Reset, descendant cleanup, and Workspace restoration pass.
- [ ] Dispatch Group behavior passes when enabled, including runtime identity,
      progression, restart, completion, and failure handling.
- [ ] Representative Claude and Codex lifecycle, discovery, Dispatch, logs,
      follow-up, mount, reset, and cleanup paths remain operational.
- [ ] No credentials or private session contents appear in the evidence.
- [ ] Existing Go/wsg or other legacy installations remain unchanged unless
      separately approved by the owner.
- [ ] Evidence records cleanup and all unresolved blockers clearly enough for
      release or follow-up decisions.

## Out of Scope

- Implementing fixes or new provider adapters while running acceptance; file a
  follow-up ticket or return to the blocked implementation ticket instead.
- Packaging a release artifact, changing aliases, pushing changes, or opening
  a pull request.
- Destructive migration, pool teardown, workspace deletion, or provider
  uninstallation without separate owner approval.
- Replacing automated unit, integration, or conformance coverage.

## Blocked by

- issues/02-add-pi-worker-runtime.md
- issues/03-complete-pi-worker-actions-and-release.md
- issues/04-add-pi-ticket-discovery-and-dispatch.md
- issues/05-add-pi-interactive-lifecycle.md
- issues/07-add-pi-direct-dispatch-profile.md
- issues/08-complete-pi-dispatch-integration.md

## Comments

**Step 1 review done 2026-10-14.** The matrix, the candidate-artifact commands,
the version and checksum captures, the reachable CLI surface, and the expected
capability differences are recorded in
[`manual-acceptance.md`](../manual-acceptance.md), which is blank and ready for
the owner's session. That review corrected one thing in this ticket: `jjfx hooks
status` is not a jjfx command (commit 3 above is fixed), so the matrix exercises
hook *behaviour* instead of calling a status command.

Confirmed reachable for the matrix: `mise run install-candidate` (stages both
binaries under a non-PATH prefix and never touches the installed Go `wsg` or the
`qwe` alias), `mise run release` / `release-smoke` for a checksummed bundle,
`jjfx pool profile pi --provider --model`, the `JJFX_PI_LINEAR_HELPER`
discovery protocol, and the fixed Pi tool policy in `docs/pi-workers.md`.

The expected unsupported capabilities - per-tool approval popups, aggregate
budget limits, a filesystem sandbox, and native Linear/MCP - are listed in the
capability matrix in `spike-01-contract-findings.md`, so the session records
them as accepted differences rather than discovering them as failures. The
per-tool approval and budget choices must be *rejected* by jjfx, not silently
weakened, and no capability gap may fall back to Claude or Codex.

**Closed 2026-10-14 - `wontfix`.** The owner no longer uses Pi and runs opencode
instead, so the manual acceptance matrix will not be run. Closed rather than
resolved: none of the acceptance criteria above were met, and the prepared matrix
in [`manual-acceptance.md`](../manual-acceptance.md) is superseded.

This is an explicit accepted risk, not a silent one. The Pi capability
differences (per-tool approval, aggregate budget, a filesystem sandbox, native
Linear/MCP) stay unverified against a *real* Pi install; they remain covered only
by the pinned adapter contract in `spike-01-contract-findings.md` and the
automated fixtures under `fixtures/`. Pi support itself stays in the tree with
its automated coverage - closing this ticket is a decision not to spend the manual
session, not a decision to remove the runtime.

Removing the runtime is tracked separately in
[`remove-pi/01`](../remove-pi/issues/01-remove-pi.md), filed 2026-10-14 and
deliberately not scheduled.
