# `status --json` reports pools

Type: task
Status: ready-for-agent
Blocked by: 06

## Problem

`status --json` is the machine-readable surface an agent drives jjfx through, and it knows nothing about pools. The additive stance constrains the shape hard: that JSON changed to `{"workers": [...], "waiting": [...]}` hours before the multi-pool effort began and shipped in 0.56.0, so breaking it again would be the worst kind of churn.

*Is this additive, or a redesign?* and *How do a pool's spend cap and the global ceiling interact?* settled the shape:

- **`workers[]` and `waiting[]` stay flat and each gains a `pool` field.** Nesting Workers under their pools was rejected: `jq '.workers[] | select(.pool == "bugs")'` answers the same question.
- **Per-pool spend arrives as a sibling `pools` key**, carrying `{name, size, spend, cap, window}` per pool - a subtotal under the global total, never a restructure.
- The prose `status` names the pool a Worker belongs to, and shows per-pool spend.

## Acceptance Criteria

- [ ] `workers[]` and `waiting[]` keep their shape and gain a `pool` field.
- [ ] A sibling `pools` key carries per-pool size and spend against its cap.
- [ ] `jq '.workers[] | select(.pool == "bugs")'` works as the documented way to filter.
- [ ] The prose output names the pool.
- [ ] `mise run check` is green.

## References

- `src/cli.rs` (the Pool status JSON document and its renderers)
- `.scratch/multi-pool/issues/07-additive-or-redesign.md` and `06-pool-spend-caps.md`
