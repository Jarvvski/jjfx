# Add Pi read-only ticket discovery

Status: claimed

## Parent

pi-support effort (standalone; no PRD)

## Problem Statement

Pi Worker execution is available, but Ready Ticket and dependency discovery
still reject `AgentRuntime::Pi`. Pi core has no native Linear or MCP contract,
so discovery cannot safely copy Claude or Codex commands, spend a model turn,
load project resources, or reserve a Worker.

The contract spike selected a dedicated read-only helper as the preferred host
seam. The helper needs a small typed protocol, bounded execution, explicit
configuration, provider-neutral output, and actionable failures that do not
leak credentials or prompts.

## Solution

Deepen the existing Ticket query module around a typed `TicketQueryRequest`.
Claude and Codex adapters privately render their existing prompts, while Pi
serializes the typed request to a configured helper. `TicketDiscovery` remains
the authoritative validation and one-retry module for Ready Tickets and
dependency graphs.

Configure the helper executable with `JJFX_PI_LINEAR_HELPER`. Invoke it
directly, without a shell, from the repository root. Send a versioned JSON
request on stdin and accept one versioned JSON result or typed error on stdout.
The helper owns credential lookup; jjfx never places credentials in argv or the
request. Missing configuration and unsupported capabilities are visible setup
errors and never fall back to Claude or Codex.

## Commits

1. Replace the free-form `TicketQuery` prompt seam with typed Ready Ticket and
   dependency-graph requests while preserving Claude and Codex behavior.
2. Add the Pi helper adapter, versioned JSON protocol, bounded execution, and
   typed setup, transport, timeout, authentication, unsupported, and malformed
   protocol outcomes.
3. Reuse existing output validation and retry behavior for Pi, covering safe
   success and failure behavior with deterministic fake executables.
4. Wire helper configuration into shared CLI and jjfx TUI discovery composition
   so errors occur before Worker reservation or Pool mutation.
5. Update setup guidance, versions, and changelog, then run the full repository
   verification gate.

## Decision Document

- Ticket discovery remains separate from Worker reservation and Run execution.
- `TicketQueryRequest` is provider-neutral. Provider prompts, helper DTOs, and
  command assembly remain private adapter details.
- Pi discovery uses only the configured dedicated helper. The optional Pi MCP
  adapter is not a discovery fallback.
- The helper protocol has two operations: `ready_tickets` with label/status and
  `dependency_graph` with parent/repository identity.
- The helper emits one JSON envelope. Wrapper streams, interactive output, and
  Pi sessions are not part of this helper-only contract.
- Helper startup and protocol setup failures are permanent. A 30-second timeout
  and errors explicitly marked transient use the existing single retry.
- A well-formed result whose Ticket payload is malformed continues through the
  existing `TicketDiscovery` malformed-response retry.
- The helper runs in the repository root but is independently responsible for
  read-only Linear access. No Pi tools, project resources, or write policy are
  loaded for discovery.
- Pi Direct Dispatch and Linear mutations require the explicit extension
  profile tracked by issue 07.

## Testing Decisions

Test through the public `TicketQuery`/`TicketDiscovery` seam with fake helper
executables. Assert typed requests, working directory, direct argv execution,
normalization, retries, timeout, process reaping, and sanitized diagnostics.
Exercise CLI and TUI outcomes for missing configuration and verify discovery
fails before Pool state changes. Preserve Claude and Codex behavior through the
same seam. Use vertical red-green cycles and run `mise run check` before
resolution.

## Acceptance Criteria

- [ ] Ready Ticket and dependency discovery select Pi through typed requests
      without reserving a Worker.
- [ ] `JJFX_PI_LINEAR_HELPER` identifies the directly executed helper; prompt
      text and credentials are never passed in argv.
- [ ] Versioned stdin/stdout JSON covers both operations, success, typed error,
      unknown fields, empty output, malformed envelopes, and malformed result
      payloads.
- [ ] Missing or blank configuration, startup failure, authentication,
      unsupported capability, timeout, transient transport failure, and
      permanent failure have actionable sanitized outcomes.
- [ ] Existing validation and one-retry behavior apply to Pi result payloads
      without treating unavailable discovery as an empty Ticket list.
- [ ] Pi discovery never loads project resources, starts a Pi session, broadens
      tools, falls back to another runtime, or mutates Worker Pool state.
- [ ] Shared CLI and jjfx TUI discovery surfaces report Pi setup/capability
      failures consistently.
- [ ] Claude and Codex discovery behavior remains unchanged and green.
- [ ] Setup guidance, versions, changelog, and `mise run check` are complete.

## Out of Scope

- Pi model/provider selection, Dispatch prompts, Linear write tools, Direct
  Dispatch, Follow-up, or Worker execution policy, tracked by issue 07.
- Dispatch Group progression and remaining CLI/TUI runtime surfaces, tracked by
  issue 08.
- Pi interactive lifecycle hooks, tracked by issue 05.
- Live Linear credentials or manual acceptance, tracked by issue 06.

## Blocked by

- issues/01-spike-pi-contracts.md
- issues/03-complete-pi-worker-actions-and-release.md
