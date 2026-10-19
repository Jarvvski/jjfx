# Make the ticket source a configured, runtime-independent choice

Status: resolved
Type: task
Blocked by: none

## Problem

Ticket discovery is coupled to the **agent runtime** when it should be coupled to
the **tracker**.

- `AgentRuntimeQuery` (`src/engine/ticket.rs:202`) selects the discovery strategy
  per `AgentRuntime`: Claude and Codex render prompts, Pi serializes a typed
  request to a dedicated helper named by `PI_DISCOVERY_HELPER_ENV =
  "JJFX_PI_LINEAR_HELPER"` (`:19`).
- `RealWorkspaceDispatch::discover_ready` (`src/workspace_dispatch/adapter.rs:409`)
  builds `TicketDiscovery::new(configured_ticket_query(&repository, runtime))`
  (`:419`), so the ticket source is a function of the runtime.

Consequences:

- On opencode there is no discovery path at all — `dispatch --all` cannot pull
  work, so tickets must be named by hand.
- A non-Linear tracker cannot be added without inventing a runtime-shaped path
  for it.
- `[tickets]` does not exist in `config.toml` (`src/config.rs`); there is no user
  surface to choose a source.

## Solution

Introduce a configured ticket **source** independent of the runtime, behind the
seam that already exists: `TicketQuery` (`src/engine/ticket.rs:511`) is a trait,
`TicketDiscovery<Q>` (`:699`) is generic over it, and `TicketQueryRequest`
(`:424`) is the provider-neutral request.

- Add a `[tickets]` section to `config.toml`: `source = "linear" | "local"` plus
  per-source settings, validated at load like the rest of the config (unknown
  keys rejected).
- Make source selection a config decision: `configured_ticket_query` chooses the
  `TicketQuery` implementation from the configured source, **not** from
  `AgentRuntime`.
- Keep the existing Claude/Codex native Linear discovery as
  `source = "linear"`, unchanged in behaviour.
- Leave the Pi helper path intact (it is `remove-pi`'s to delete); this ticket
  only stops *depending* on it for source selection.

## Commits

1. Add the `[tickets]` config section (source + per-source settings) with
   validation and defaults, and reject an unknown source before the TUI opens.
2. Split `configured_ticket_query` so the source is selected independently of
   `AgentRuntime`; keep `AgentRuntimeQuery` as the `linear` implementation.
3. Route `discover_ready`, `orchestrate`, and `ticket_details` through the
   configured source on the shared CLI and TUI composition paths.
4. Cover source selection with a fake `TicketQuery` and assert Linear behaviour
   is byte-for-byte unchanged for Claude/Codex.
5. Document `[tickets]` in `docs/configuration.md`; changelog.

## Acceptance Criteria

- [x] The ticket source is selected by config, not by `AgentRuntime`.
- [x] `source = "linear"` reproduces today's Claude/Codex discovery, dependency
      graph, and ticket-details behaviour exactly.
- [x] Discovery, dependency graph, and ticket details all go through the selected
      source.
- [x] A missing or unknown source is a clear setup error surfaced before any
      Worker reservation or Pool mutation.
- [x] An opencode Worker can discover tickets through a configured non-runtime
      source.
- [x] `mise run check` is green.

## Out of Scope

- The local-markdown implementation itself — ticket 02.
- Removing the Pi helper and its protocol — `remove-pi`.
- Any second tracker beyond `linear` and `local`.

## References

- `src/engine/ticket.rs:19,202,424,511,699`
- `src/workspace_dispatch/adapter.rs:409,419`
- `src/config.rs`
- `docs/pi-workers.md` (the helper protocol this replaces as the source seam)

## Comments

- 2026-10-15: Filed from the strategy review. This is the foundation for ticket
  02 (the bet). If `remove-pi` lands first, its "keep the shared discovery
  abstraction working" note is already satisfied here.
- 2026-10-16: **Landed** as PR #2 (`Make the ticket source a configured,
  runtime-independent choice`). The source is selected from `[tickets]` config,
  not `AgentRuntime`; `source = "linear"` is byte-for-byte unchanged; a
  non-runtime source is proven to drive discovery, dependency graph, and ticket
  details. The one criterion deferred to 02 — "an opencode Worker can discover
  tickets through a configured non-runtime source" — is satisfied by 02 (PR #6).
  Status reconciled after the batch: PR #2 left this file's status uncommitted,
  so it is committed here.

## Resolution

Reconciled Mon 19 Oct 2026 against the merged code at `9d6bd0ce6e7b`.

This ticket was marked resolved without its acceptance boxes ticked. Every criterion was re-verified by an independent read-only review of the code and tests on `main`. The reconciliation recorded no new work; it confirms what shipped.

Evidence: the source is a config decision - `TicketSource` (`src/config.rs:452`) is `linear | local`, and `configured_ticket_query` (`src/ticket_source.rs:27`) selects the implementation from `TicketsConfig`, taking the runtime only to build the `linear` variant (`:34`). Criterion 4 holds: `source` is required when `[tickets]` is present (`src/config.rs:390-397`), so a missing `source` is a load-time serde error and an unknown value fails the enum parse - both before the TUI opens and before any Pool mutation. Criterion 5 (deferred to ticket 02 by the ticket's own comment) holds on 02's evidence, below.

Caveat, as in the reconciliation of named-pools 01: the "byte-for-byte" clause is established behaviourally - the `linear` path is the unchanged `AgentRuntimeQuery` and the config-selection tests are additive - not by a literal baseline comparison.

Not ticked: the `mise run check` criterion. It is a landing-commit property, and this reconciliation deliberately does not run the full suite (the orchestrator runs the gate once at the end).

Gate verified by the orchestrator on `df05f7c9aaef`: `mise run check` is green (changelog validation, fmt, clippy with warnings as errors, build and the full test suite), with no leaked test processes afterwards. The reconciliation above left this box unticked only because it ran no suite.
