# Verify a change builds against trunk before the forge pushes it

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

**jjfx has no ground truth about whether a Worker's work is correct, or even
finished.** A Run is `done` or `failed` purely from the agent runtime's own
terminal log event (`src/engine/run_log.rs:358` -> `RunResult` ->
`src/engine/pool.rs:1529-1544`). The build instruction lives *inside the dispatch
prompt* as prose:

> `src/engine/dispatch_prompt.rs:204` - "Run the full lint, type-check, build,
> and test suite and fix every failure"

So the entire "this ticket is finished" signal is **the agent's own report that
it is**. An exhaustive search of `src/engine` finds no compile step, no test step
and no post-run gate. The forge welds onto trunk and checks for jj *conflicts*
(`src/forge.rs:172`, `:612-656`) but never compiles anything.

That is a trust boundary, and the October 2026 batch crossed it. Two changes were
each green, mergeable and at zero open threads while **not compiling together** -
both called a member that a third, still-open change added. Every gate either
change had passed was real; none of them could see the pair. A human caught it by
rebasing one onto trunk and compiling it by hand, which is exactly the step that
should not depend on a human remembering.

## Solution

A configurable, repo-defined check that runs **in the workspace, against merged
trunk, before the push**.

- Add a `[checks]` section to config (`src/config.rs:14-34`; the struct is
  `deny_unknown_fields`, so this is a new typed field). It names a command the
  repo owns - `./gradlew :app:backend:compileKotlin`, `cargo check`, `bun run
  build` - plus a timeout. jjfx must not guess the command; an unconfigured repo
  is a no-op, not a default.
- Hook it into the forge as a new `Step` **between Weld and Push**
  (`src/forge.rs:39-50`). Weld has already rebased onto trunk by that point, so
  the check runs against what `main` will actually be - which is the whole point.
  A post-Run hook in `WorkerActions` would run against the un-welded tree and miss
  exactly the failure above.
- Run it off the UI thread (per bug-hunt ticket 17) and classify the outcome as a
  typed error naming the command and its failing output. A check that cannot run
  (command missing, timed out) must fail loudly, never pass silently - the same
  defect class as a scoping helper that exits 0 having checked nothing.
- The failure should stop the push, not merely warn: an unbuildable change is not
  a change, and pushing it is what makes the pair invisible.

**Scope note.** This ticket is the compile half. The batch's other cross-PR hazard
is migration ordering (ticket 17); both are properties of the set of open changes
and both belong before the push. If they land as two steps, they should share the
config section and the failure shape.

## Commits

1. Add the `[checks]` config section (command, timeout, enabled) with
   `deny_unknown_fields` kept honest and a clear no-op when unset.
2. Implement the check runner: workspace cwd, merged trunk, off the UI thread,
   typed outcome (passed / failed / could-not-run).
3. Wire it into the forge between Weld and Push, stopping the push on failure with
   the command and output named.
4. Tests (a change that does not compile against trunk fails the step; an
   unconfigured repo is a no-op; a missing command is a loud failure, not a pass);
   docs (`docs/configuration.md`, forge docs); changelog.

## Acceptance Criteria

- [ ] With a check configured, a change that does not build against merged trunk
      fails before the push, naming the command and its output.
- [ ] With no check configured, the forge behaves exactly as today.
- [ ] A check that cannot run (missing command, timeout) fails loudly and does not
      report success.
- [ ] The check runs against the welded (trunk-based) tree, not the pre-weld tree.
- [ ] `mise run check` is green.

## Out of Scope

- Choosing or shipping a build command per language (the repo's config owns that).
- Running the full test suite as a gate (a compile check is the cheap, universal
  half; a test gate is a separate decision).
- Migration ordering (ticket 17).
- Reading required CI checks or mergeability as a gate.

## References

- `src/engine/dispatch_prompt.rs:204` (the prompt-only build instruction)
- `src/engine/run_log.rs:358`, `src/engine/pool.rs:1529-1544` (the done signal)
- `src/forge.rs:39-50` (`Step`), `:172`, `:612-656` (weld, conflicts only)
- `src/config.rs:14-34`, `.scratch/bug-hunt/issues/17-move-blocking-work-off-ui-thread.md`
- `docs/adr/` (ADR-0009: jjfx owns active orchestration)

## Comments

- 2026-10-15: Filed from the strategy review. The largest of the three new
  tickets and the one with the highest expected saving: it is the missing half of
  the lifecycle rather than an optimisation. Filed as the compile half first
  because it is universal and cheap; the trust boundary it closes is the whole
  `done` signal.
