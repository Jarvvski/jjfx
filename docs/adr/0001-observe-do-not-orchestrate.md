# Observe the workspace lifecycle; do not orchestrate agent work (yet)

jjfx models each workspace's lifecycle as an observable state machine but does
not own a task queue or auto-dispatch prompts to idle workspaces. Agents are
spawned manually; the tool watches and surfaces state. We chose this over an
active orchestrator - a worker pool plus ticket queue, foreshadowed by the
unused pool machinery in `jj-ws` (`.jj/pool.json`, per-worker state files) - to
keep the core small and get the observation model right first. A queue is
intended as a later layer on the same state model, not a rewrite.

Status: accepted
