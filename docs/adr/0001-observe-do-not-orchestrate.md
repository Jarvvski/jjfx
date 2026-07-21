# Observe the workspace lifecycle; do not orchestrate agent work (historical limitation)

jjfx models each workspace's lifecycle as an observable state machine but did
not initially own a task queue or auto-dispatch prompts to idle workspaces.
Agents were spawned manually; the tool watched and surfaced state. We chose
this over an active orchestrator - a worker pool plus ticket queue, foreshadowed
by the unused pool machinery in `jj-ws` (`.jj/pool.json`, per-worker state
files`) - to keep the core small and get the observation model right first. A
queue was intended as a later layer on the same state model, not a rewrite.

## Superseded scope

ADR 0009 adopts Workspace Dispatch as that later layer. Its decision supersedes
only this ADR's limitation that jjfx must not own orchestration. The observation
model, the choice to build on the existing Workspace model, and the rationale
for keeping Agent and Work lifecycles distinct remain accepted. The historical
reasoning and the original postponed scope remain readable here.

Status: accepted - orchestration scope superseded by ADR 0009
