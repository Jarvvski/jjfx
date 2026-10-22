# Spend fixtures rot once their seeded Run leaves the rolling week

Status: resolved
Type: bug
Blocked by: none

## Problem

Two integration fixtures seed a durable Run record with a hard-coded
`completed_at` of `2026-10-15T09:00:00Z` and then rely on a spend cap refusing:

- `tests/pool_routing.rs::a_capped_lane_is_held_while_other_lanes_dispatch`
  (`cap_usd = 5.0` on a pool, seeded `$6`),
- all eight tests in `tests/pool_spend.rs` (seeded `$6` against `$5` caps).

The default spend window is a rolling seven days (`SpendWindow::Week`,
`src/config.rs`), so both fixtures are written on the assumption that the seeded
Run is inside it. Seven days later it ages out, the spend reads as `$0`, the cap
stops refusing, and the fixture silently stops describing what it says it
describes.

The two surfaces differ, which is why this looked like two bugs at first:

- In `pool_routing`, the lane that should be *held* instead **dispatches**, and
  the run fails with `cannot reserve Worker Pool: state is missing` - see
  [48](48-a-pool-directory-without-a-document-is-not-materialised.md) for the
  latent defect that turns the unexpected dispatch into that particular error.
- In `pool_spend`, the fixture installs a **deliberately failing** `kitten`
  (`exit 1`) to prove that a *held* batch needs no terminal. Once the cap stops
  refusing the batch is not held, the failing stub is exercised, and every test
  dies on the terminal refusal after burning the full ~120s retry budget:

  ```
  Error: cannot determine which Worker Workspaces are open; re-run with --force to dispatch anyway

  Caused by:
      kitten @ ls failed:
  ```

That 120s terminal failure is what made it look like a terminal-hermeticity bug
rather than a calendar one. It is not: the stub is doing its job, and the batch
was never held.

Neither is flaky. `pool_routing` failed 30/30 locally, and CI failed both on
2026-10-22 having passed the *same* commit on 2026-10-21. **`main` is red from
2026-10-22 onward.**

## Acceptance Criteria

- [x] Both fixtures assert what they mean regardless of the date.
- [x] Falsified: the original shape with only the timestamp freshened passes, so
      the timestamp is the cause and nothing else is.

## Resolution (Thu 22 Oct 2026)

**The fixtures were made clock-independent; no assertion was weakened.**

`support::recent_timestamp()` stamps the seeded Run with *now* (via `jiff`, which
the crate already depends on), so the Run is always inside the window the cap
totals over. That is what the fixtures meant all along - "a cap it has already run
out of" - and it keeps the **default** rolling week exercised rather than pinning
the window to `total` and stepping around the clock.

Falsified both ways before landing:

| shape | result |
|---|---|
| original (seeded `2026-10-15`) | FAILED - `pool_routing` 30/30, `pool_spend` 7/8 after 120s |
| original with the seeded timestamp replaced by now | PASSED |
| the fix | `pool_routing` 11/11, `pool_spend` 8/8 in 2.3s (was 120s) |

The other fixtures carrying hard-coded `completed_at` values
(`review_contract`, `wait`, `agent_runtime`, `pool_status_json`) do not gate on a
spend window - `pool_status_json` only *reads* it - so they do not rot this way.
