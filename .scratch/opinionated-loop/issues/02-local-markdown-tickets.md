# First-class local `.scratch/` markdown tickets

Status: ready-for-agent
Type: task
Blocked by: 01

## Problem

The owner tracks work as local markdown under `.scratch/<feature>/issues/NN-<slug>.md`
for OSS/personal repos — that is this repository's own convention
(`docs/agents/issue-tracker.md`, `docs/agents/triage-labels.md`). But Dispatch is
Linear-only: there is no ticket source that reads those files, and the only
non-named discovery path requires Pi plus a Linear helper. On every OSS/personal
repo, therefore, the Worker Pool cannot pull work — tickets must be named by hand
against a tracker that is not the one being used.

## Solution

Add a **local markdown** ticket source implementing `TicketQuery`
(`src/engine/ticket.rs:511`), selected as `source = "local"` (ticket 01). It reads
the repository's `.scratch/` tree with no runtime, no Linear, and no Pi.

- **Discovery**: `.scratch/*/issues/*.md` (configurable root/glob). The **ready
  set** is files whose `Status:` is `ready-for-agent` (per
  `docs/agents/triage-labels.md`) and whose every `Blocked by:` entry is
  `resolved`.
- **Identity**: id = `<feature>/<NN>-<slug>` (path-derived, stable); title = the
  first `# ` heading; body = the file content below the metadata lines.
- **Dependencies**: `Blocked by: NN, NN` maps directly onto the existing Dispatch
  Group / Parent orchestration — no new ordering engine.
- **Prompt inlining**: the file body is inlined into the Worker's prompt the same
  way a Linear title+description is today, so a Worker needs no tracker tools.
- **Writeback**: set `Status: claimed` when Dispatch reserves the ticket; set
  `Status: resolved` and append a dated `## Comments` line on completion. This is
  the `/wayfinder` claim protocol, and it matches `bd --claim`.

Parse the existing `Key: value` line convention (as `issue-tracker.md` records),
not YAML front-matter.

## Commits

1. `LocalMarkdownQuery` implementing `TicketQuery`: discover `.scratch/*/issues/*.md`,
   parse `Status:` / `Blocked by:` / `Type:`, and extract id, title, and body.
2. Project `ReadyTickets` and `DependencyGraph` (`TicketQueryRequest`) over the
   local set, resolving `Blocked by:` against `Status:`.
3. Claim/resolve writeback that preserves file formatting and appends a dated
   `## Comments` line; never rewrite unrelated content.
4. Wire `source = "local"` into the shared CLI and TUI dispatch composition and
   inline the body into the dispatch prompt.
5. Docs — `docs/configuration.md` plus a short `docs/local-tickets.md` (or an
   `issue-tracker.md` note) — and changelog.

## Acceptance Criteria

- [ ] `jjfx dispatch --all` with `source = "local"` discovers ready tickets from
      `.scratch/*/issues/*.md`.
- [ ] A ticket with an unresolved `Blocked by:` is not ready; resolving the
      blocker makes it ready, and the dependency graph matches the files.
- [ ] Id, title, and body are read correctly and the body is inlined into the
      Worker prompt.
- [ ] Dispatch sets `Status: claimed`; a completed Run sets `Status: resolved`
      and appends a dated `## Comments` line.
- [ ] A malformed ticket file is surfaced as a `DiscoveryDiagnostic`, never
      silently skipped and never collapsed into an empty ready list (the "no
      plausible zero" rule from bug-hunt ticket 21).
- [ ] Works end-to-end on opencode with no Pi and no Linear configured.
- [ ] `mise run check` is green.

## Out of Scope

- Other trackers (`git-bug`, `bd`, Backlog.md). A generic `source = "cmd"` helper
  (reusing the versioned protocol shape) is a possible follow-up, not this ticket.
- Writeback of anything other than `Status:` and a `## Comments` line.
- The Pi helper removal — `remove-pi`.

## References

- `docs/agents/issue-tracker.md`, `docs/agents/triage-labels.md`
- `src/engine/ticket.rs` (`TicketQuery`, `TicketQueryRequest`, `TicketDiscovery`)
- `.scratch/wayfinder` convention (frontier / claim / resolve)

## Comments

- 2026-10-15: Filed from the strategy review. This is the bet: it closes the
  named dispatch-loop friction and lets jjfx dogfood on its own `.scratch`
  tickets.
