# Add Pi interactive lifecycle tracking

Status: ready-for-agent

## Parent

pi-support effort (standalone; no PRD)

## Problem Statement

jjfx can launch an arbitrary interactive command, but its lifecycle state is
currently derived from Claude Code and Codex hook payloads. A workspace running
Pi therefore appears absent or may be misidentified as another provider. Pi
also does not expose the same hook configuration files, event names, or
permission-dialog behavior as those providers.

Interactive lifecycle support needs a Pi-specific event adapter that observes
Pi's documented extension lifecycle and sends only the provider-neutral fields
needed by jjfx. It must be installable without clobbering Pi settings,
extensions, sessions, packages, or project trust decisions, and it must not
pretend that Pi has a `NeedsAttention` signal when it does not.

## Solution

Use the contract and transport selected by issue 01 to ship a jjfx-owned Pi
extension or equivalent Pi integration. Install and report its status through
the existing `jjfx hooks install` / `jjfx hooks status` workflow, preserving
idempotency and unrelated provider configuration. Extend the interactive
lifecycle model with explicit Pi identity and render it distinctly in the
workspace TUI. Keep Pi event collection and extension details outside the
shared `AgentState` transition model.

The adapter should translate the strongest supported Pi lifecycle signals:

- Pi session start -> session present and waiting;
- Pi agent start/active turn -> working;
- Pi agent settled/agent end -> waiting, unless the contract proves a more
  specific terminal state;
- Pi session shutdown -> ended or absent according to the selected transport;
- attention-required -> only if issue 01 identifies a reliable Pi signal.

The implementation must retain a neutral unknown state for malformed,
partial, stale, or unrecognized events.

## Commits

1. Define the Pi event envelope and installation/transport seam selected by
   issue 01, including extension versioning, state directory handling, and
   failure behavior without writing global files in tests.
2. Add `AgentKind::Pi` and explicit provider identification from the Pi event
   envelope. Ensure missing or ambiguous identity remains `Unknown` rather
   than inheriting Claude styling.
3. Implement the jjfx-owned Pi extension or equivalent adapter to emit session,
   agent, turn, and shutdown events with cwd, stable session identity where
   available, and the selected append-only transport.
4. Extend `jjfx hooks install` and `jjfx hooks status` to install, detect, and
   report the Pi integration idempotently while preserving existing Pi
   settings, extension resources, trust data, and Claude/Codex hook entries.
5. Extend lifecycle replay and live updates to fold Pi events without changing
   Claude/Codex transitions or allowing stale/partial records to crash the TUI.
6. Add Pi's label, working animation, paused/ended glyph behavior, and brand
   color using the repository's existing TUI conventions. Keep the visual
   choice documented and avoid using Claude/Codex fallback styling for Pi.
7. Add unit and integration coverage for event mapping, malformed records,
   path/cwd joins, session switching, install/status idempotency, preservation
   of unrelated configuration, replay/live equivalence, and TUI rendering.
8. Update setup and lifecycle documentation, bump the version according to
   repository policy, and add a dated changelog entry.

## Decision Document

- `AgentKind::Pi` is the interactive provider identity; it is deliberately
  separate from worker `AgentRuntime::Pi` even when both refer to Pi.
- The Pi adapter emits the smallest stable envelope that jjfx needs. Pi
  extension-specific payloads are not made part of the Rust lifecycle domain.
- Installation must be idempotent and additive. Existing Pi settings,
  extension paths, package resources, trust decisions, and unrelated hooks are
  preserved exactly unless issue 01 documents a required additive change.
- Pi lifecycle state is based on observed extension events, not on process
  polling, terminal scraping, or guessed session-path substrings.
- `NeedsAttention` is shown for Pi only when a reliable event or extension
  decision is established. Otherwise Pi remains working/waiting/ended without
  a fabricated permission state.
- Unknown or malformed Pi records remain neutral and must not be rendered as
  Claude or Codex.

## Testing Decisions

Run the extension in temporary Pi configuration, session, package, and trust
locations. Use a deterministic event sink or temporary XDG state directory to
assert exact envelopes and append behavior. Test installer merges against
empty, existing, malformed, and unrelated Pi configuration. Test lifecycle
folding through both startup replay and live application. Test TUI helpers at
narrow terminal sizes so missing or partial Pi data cannot panic. Run
`mise run check` after implementation.

## Acceptance Criteria

- [ ] A real Pi session can be identified as Pi through the issue 01-selected
      adapter, with cwd and stable session identity captured where available.
- [ ] Pi session, active-agent, settled, and shutdown events map to the
      documented jjfx lifecycle states; unsupported attention states are
      explicitly documented rather than fabricated.
- [ ] `AgentKind::Pi` never falls back to Claude or Codex when Pi identity is
      missing, ambiguous, malformed, or stale.
- [ ] `jjfx hooks install` installs or enables the Pi integration idempotently,
      and `jjfx hooks status` reports its state accurately.
- [ ] Installation preserves unrelated Pi settings, resources, sessions,
      packages, trust decisions, and existing Claude/Codex hook configuration.
- [ ] Startup replay and live updates produce equivalent Pi state, tolerate
      malformed/partial records, and join events to workspaces by cwd safely.
- [ ] The TUI distinguishes Pi in labels and lifecycle glyphs/animation/color,
      handles narrow terminals, and does not misrepresent absent or ended
      sessions as active.
- [ ] Claude and Codex hook installation, lifecycle folding, and rendering
      remain behaviorally unchanged.
- [ ] Setup/lifecycle documentation, version, changelog, and `mise run check`
      are complete.

## Out of Scope

- Worker `AgentRuntime::Pi`, structured worker logs, ticket discovery, or
  Dispatch behavior covered by issues 02 and 03.
- Modifying Pi itself or installing third-party Pi packages.
- Process-tree polling or terminal scraping as a replacement for lifecycle
  events.
- Adding a generic permission UI to Pi or changing Pi's trust model.
- Live owner acceptance of a real three-provider installation.

## Blocked by

- issues/01-spike-pi-contracts.md
