# Record the agent-deep decision as an ADR and correct the map

Status: resolved
Type: docs
Blocked by: none

## Problem

The four decisions in this batch's map are taken but not recorded where the
repo keeps decisions, and `opinionated-loop/map.md` still carries two claims the
research falsified.

## Acceptance Criteria

- [x] An ADR records the agent-deep decision, referencing ADR 0009 which it
      extends.
- [x] `opinionated-loop/map.md`'s two falsified claims (every rival is
      git-worktree-based; jjfx is the only jj-native orchestrator) are corrected
      in place.

## Resolution

The four decisions are recorded as **ADR 0016**
(`docs/adr/0016-jjfx-is-the-deterministic-control-plane.md`), and the two false
claims are withdrawn in place.

- **ADR 0016 extends ADR 0009 rather than repeating it.** ADR 0009 gave the
  control plane its domain (Worker, Worker Pool, Run, Dispatch) and its seam;
  ADR 0016 gives it its job - the queue, the bounds, verification, capacity and
  session topology. Where 0009 says jjfx may orchestrate, 0016 says jjfx owns
  the determinism of it.
- **The ADR records the four decisions with their dates** (2026-10-19) and,
  more importantly, the falsified alternatives: model-level determinism, a
  prompt-enforced bound, the agent's self-report as done, and extra forge
  paths. The measured evidence (1,481 sessions; the $0.71 Worker Pool; the
  144-session PR; 693 of 991 reviewers in drain sessions; 33 orphaned children)
  is cited as the reason, and `.scratch/harness/map.md` is linked rather than
  restated.
- **`opinionated-loop/map.md` corrected.** The sentence asserting jjfx is "the
  only jj-native orchestrator" with "every rival is git-worktree-based" now
  reads as a conclusion whose **both claims are false and are withdrawn**:
  container-based isolators exist (container-use, Sculptor, Terragon) and a
  jj-native cluster exists (jj-navi, ao-jj, jjx, agentic-jujutsu, Kaisen,
  smithers-orchestrator). The surviving claim is narrower and stronger - jjfx
  is the only tool that makes the jj substrate the orchestration model - and
  the field's two unsolved problems are verifying agent output and integrating
  parallel work, which `[checks]` and conflict-as-data address. A dated
  `## Comments` entry records the withdrawal.
- **`.scratch/harness/map.md` points at the ADR**, so the batch's decisions and
  the repo's decision record name each other.

No changelog entry: an ADR and a `.scratch/` map are internal records, not
user-visible behaviour. Docs-only, so `mise run check` is skipped and no code
was touched.

## References

- `docs/adr/0016-jjfx-is-the-deterministic-control-plane.md`
- `docs/adr/0009-workspace-dispatch-domain.md`
- `.scratch/harness/map.md`
- `.scratch/opinionated-loop/map.md`
