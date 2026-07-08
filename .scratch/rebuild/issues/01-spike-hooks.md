# Spike: confirm Claude Code hook lifecycle events

Status: ready-for-agent

## Parent

epics/A-foundations.md

## What to build

A time-boxed investigation that establishes, empirically against the installed
Claude Code version, which hook events fire at each lifecycle boundary and what
payload each carries, so the agent-lifecycle state machine (ADR 0002/0003) can
be wired with confidence. Resolve whether `Notification` is a first-class event
or whether `PermissionRequest` is the authoritative "blocked, needs the human"
signal. Confirm the events we depend on carry `session_id` and `cwd` (the join
key to a workspace). Produce a findings note with the event -> agent-state
transition map for issue 04 to implement against.

## Acceptance criteria

- [ ] Each candidate hook (SessionStart, UserPromptSubmit, Stop, PermissionRequest, Notification, SessionEnd, and any others present) is triggered and its stdin payload captured verbatim.
- [ ] Confirmed which event(s) map to Working, Waiting, NeedsAttention, Ended; the Notification vs PermissionRequest ambiguity is resolved.
- [ ] Verified `session_id` and `cwd` are present on the events we rely on.
- [ ] Findings recorded as a note under `.scratch/rebuild/`; ADR 0002 amended if reality differs from its assumptions.

## Blocked by

None - can start immediately.
