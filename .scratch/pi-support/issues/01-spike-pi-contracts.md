# Spike: establish Pi runtime and lifecycle contracts

Status: ready-for-agent

## Parent

pi-support effort (standalone; no PRD)

## Problem Statement

jjfx currently has provider-specific assumptions at every boundary where Claude
Code and Codex differ: executable invocation, structured output, Agent Session
identity, interactive lifecycle hooks, permission state, and ticket discovery.
Pi's documented integration surfaces are different. Pi exposes interactive,
print, JSON, and RPC modes; persists sessions as versioned JSONL; and exposes
lifecycle events through TypeScript extensions. It does not provide built-in
MCP, sub-agents, or permission popups.

Implementing Pi support from the Claude or Codex contract would risk incorrect
resume commands, unusable log parsing, unsafe discovery permissions, or a TUI
that reports lifecycle states Pi cannot actually provide.

## Solution

Run a time-boxed, read-only contract spike against the installed Pi executable
and the documented Pi 0.84.x interfaces. Capture representative, sanitized
fixtures and record the smallest provider adapter contract required by the
remaining tickets. The spike must distinguish facts observed from the local
executable, facts guaranteed by Pi documentation, and capabilities that are
not available or need an extension.

The findings must make explicit decisions for:

- fresh, headless, JSON, interactive, and resumed worker invocations;
- session IDs, session files, working-directory identity, and resume behavior;
- structured stream records needed for activity, tool calls, results, usage,
  cost, failure, and current activity;
- extension lifecycle events that can represent working, waiting, ended, and
  any attention-required state;
- model, system-prompt, display-name, budget, approval, trust, and tool-policy
  controls;
- safe ticket discovery and Linear access without assuming built-in MCP; and
- capability probing, executable failures, malformed records, and unsupported
  features.

## Commits

1. Record the installed Pi version and verify the supported CLI modes and
   relevant flags without changing project configuration.
2. Capture sanitized JSON-mode and session-file fixtures covering session
   startup, agent and turn lifecycle, assistant output, tool execution,
   terminal completion, failure, and usage where available.
3. Verify fresh and resumed session commands, session identity stability,
   working-directory association, and behavior for missing or invalid session
   IDs.
4. Exercise a throwaway Pi extension to capture lifecycle event names,
   ordering, payload fields, shutdown behavior, and non-interactive mode
   behavior.
5. Verify read-only and workspace-write tool policies, project trust behavior,
   approval controls, model/system-prompt/name support, and the absence of a
   native budget or permission-dialog contract.
6. Compare safe approaches for Pi-based ticket discovery and Linear access,
   selecting one implementable adapter or recording discovery as explicitly
   unsupported until a separate capability is supplied.
7. Write the findings, capability matrix, sanitized fixtures, rejected
   assumptions, and implementation decisions under `.scratch/pi-support/`.

## Decision Document

- Pi support must use Pi's JSON/session contracts for worker runs rather than
  scraping interactive terminal output.
- The implementation must preserve provider-neutral public run and log values;
  Pi-specific JSON types remain private to the adapter.
- Missing Pi capabilities must produce typed, actionable errors. They must not
  silently fall back to Claude or Codex commands or permission semantics.
- Any interactive lifecycle integration must use an explicit Pi extension or
  another observed event transport. A guessed transcript-path heuristic is
  not sufficient evidence.
- Ticket discovery is a separate capability from worker execution. If the
  spike cannot establish a safe Linear transport, worker execution may still
  proceed, but discovery must remain visibly unsupported.
- Fixture data must contain no API keys, credentials, private prompts, or
  unreviewed session contents.

## Testing Decisions

Use the real locally installed Pi executable for command and stream evidence,
with bounded timeouts and temporary session/configuration directories. Use a
throwaway extension and temporary project directory so the user's global Pi
settings, trust store, sessions, packages, and credentials are not modified.
Validate captured fixtures with a small deterministic parser or focused tests
where useful. Do not require live Linear credentials for the spike. Run
`mise run check` if any repository code or test harness is changed; findings
and sanitized fixtures alone do not justify adding production code.

## Acceptance Criteria

- [ ] The installed Pi version and the exact tested executable are recorded.
- [ ] Fresh, headless JSON, interactive, and resumed invocation forms are
      proven or explicitly marked unsupported, including exit and error
      behavior.
- [ ] A sanitized fixture set records session identity, working directory,
      assistant/tool activity, terminal success/failure, usage/cost fields,
      and malformed or unknown event behavior where Pi exposes them.
- [ ] The session file location, schema version, identity extraction rule, and
      resume/fork behavior are documented.
- [ ] Extension lifecycle events and payloads are mapped to jjfx lifecycle
      states, with unsupported states called out rather than invented.
- [ ] Tool, trust, approval, model, system-prompt, name, budget, and Linear
      discovery capabilities have an explicit supported/unsupported decision.
- [ ] The findings identify every assumption that implementation tickets must
      not make and provide enough evidence to implement tickets 02-04.
- [ ] No global Pi configuration, credentials, sessions, or project files are
      modified by the spike.
- [ ] Findings and fixtures are reviewable under `.scratch/pi-support/` and
      contain no secrets.

## Out of Scope

- Adding `AgentRuntime::Pi` or `AgentKind::Pi`.
- Production changes to jjfx, Pi extensions, hooks, CLI behavior, or TUI.
- Implementing structured log parsing, dispatch, ticket discovery, or manual
  acceptance.
- Requiring a live Linear account or changing the user's Linear setup.
- Selecting visual branding beyond recording the information needed by the
  interactive-lifecycle ticket.

## Blocked by

None - this spike can start immediately.
