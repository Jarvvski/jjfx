# Verify a change builds against trunk before the forge pushes it

Status: resolved
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

- [x] With a check configured, a change that does not build against merged trunk
      fails before the push, naming the command and its output.
- [x] With no check configured, the forge behaves exactly as today.
- [x] A check that cannot run (missing command, timeout) fails loudly and does not
      report success.
- [x] The check runs against the welded (trunk-based) tree, not the pre-weld tree.
- [x] `mise run check` is green.

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
- 2026-10-15: Resolved.
  - `[checks]` (`command`, `enabled`, `timeout_secs`) names the repo-owned command; `enabled` is on by default and `command` unset is the no-op, so an unconfigured repo is not forced into a guessed default. Validated at load (a blank command or a zero timeout fails at startup).
  - The runner (`src/checks.rs`) runs the command through the user's shell (`sh -c`) in the **workspace's** working copy, bounded by `timeout_secs`, and classifies a typed `Outcome`: `NotConfigured` / `Passed` / `Failed { command, output }` / `CouldNotRun { command, detail }`. It runs on the forge pipeline's `spawn_blocking` thread, which is already off the UI thread (bug-hunt 17).
  - `Step::Check` sits **between Weld and Push**, so it sees the just-welded, trunk-based tree - the point of the ticket. A failure, or a check that cannot run, stops the push and names the command and the last line of its output.
  - Decisions:
    - **"Could not run" is exact.** The command runs through `sh -c`, so a missing command would otherwise be just exit 127. `Run` gained `code()`, and 126/127 map to `CouldNotRun` - a check that checked nothing must not read as a pass.
    - **The reason is one bounded line** (the tool's own summary line, ≤240 chars). `Failed`/`CouldNotRun` keep the whole output; the footer is a single line, so only the summary is shown.
    - **An unconfigured check is `Status::NotApplicable`**, which counts as clean and is not drawn - so an unconfigured repo's pipeline, result and row are exactly as before. `NotApplicable` is a new `Status` rather than reusing `Skipped`, because `Skipped` carries a reason and would retain progress, changing today's behaviour.
    - **`forge::Settings`** bundles `[forge]` and `[checks]` into one `Runtime::new` parameter (the positional list had reached clippy's argument limit).
  - `[checks]` shares `[migrations]`'s section shape (doc comment, `deny_unknown_fields`, `impl Default`, `fn validate`) and its docs sit beside it; both are cross-PR-set hazards that belong before the push.
  - Commits: the config section landed alone; the runner and its wiring landed together - the runner is dead code until the pipeline calls it, and `mise run lint` is `-D warnings` - with `Step::Check`, the failure stop, and the render in that commit; docs and changelog last. Tests landed with the code (TDD): the acceptance tests are `the_build_check_runs_against_the_welded_tree_between_weld_and_push` (the check only passes because Weld materialised the tree first, and it asserts the cwd), `a_change_that_does_not_build_stops_the_push_and_names_the_command`, `a_check_that_cannot_run_stops_the_push`, `an_unconfigured_repo_does_not_run_a_check_and_still_pushes`, and the runner's own outcome tests.
  - Scope held: no language-specific command shipped, no test-suite gate, and required CI checks are not read.
