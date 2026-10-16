# How do a pool's spend cap and the global ceiling interact?

Type: grilling
Status: resolved
Blocked by: none

## Question

Each pool gets its own spend cap, under a global ceiling that still binds. Settle the semantics:

- **Precedence.** A pool is under its cap but the global ceiling is reached — does the pool's Dispatch refuse, hold Tickets back, or proceed? Which limit wins, and is the refusal attributed to the pool or to the Repository?
- **What hitting a cap does.** The `waiting` set already exists for Tickets a capped Dispatch held back, and is carried in `status --json`. Should a cap-held Ticket appear there, so it reads as "held, deliberately" rather than "did not happen"? This is the same honesty problem the dispatch-ergonomics batch exists to fix — a cap that silently drops work is the thing that batch is about.
- **Attribution and shape.** Spend is reported per Worker today and pooled by the Repository. What is the new shape — per-pool subtotals under a global total? And does `--budget` on a single Dispatch interact with a pool cap (a per-dispatch cap *inside* a per-pool cap)?

Builds on the Run Record work that landed today: every finalized Run now appends a durable record carrying its cost, whether it ended under the supervisor or was reconciled later. That is what makes per-pool accounting possible at all — before it, spend was always `$0.0000 across 0 Run(s)`.

## Answer

Resolved 2026-10-16.

**A pool cap is the existing mechanism, scoped.** The global ceiling is `[spend]` in the user config: `cap_usd`, `window` (default `week`), `on_exceed` (default `block`; `warn` proceeds and reports the overage), totalled from the durable per-Run cost records rather than from per-Run hints. A pool's cap takes the same shape in its `[pools.<name>]` section, defaulting `window` and `on_exceed` to the global values so a pool states only what differs. It is repository-local because a cap is a **restriction** — it can only lower what the ceiling already allows (ADR 0014).

**Precedence: both bind, the most restrictive wins, and the refusal names which.** Dispatch proceeds only when every applicable limit has room — the per-Dispatch `--budget`, the pool cap, and the global ceiling — so the effective limit is the minimum. When the ceiling is reached while a pool still has room, the pool's Dispatch **holds its Tickets back** and the attribution names the **Repository**, because that is the dial the owner has to turn; naming the pool would send them to the wrong one. `on_exceed = warn` stays per-level: either level may be configured to proceed and report instead of blocking.

**A cap-held Ticket appears in `waiting`; it never disappears.** That set exists for exactly this, and a cap that silently dropped work is the failure class the dispatch-ergonomics batch was filed to remove. So `--all` discovery holds the Ticket back *visibly*, reading as "held, deliberately". An **explicit** single Dispatch is the exception: it **refuses loudly**, naming the binding limit, because the owner asked for that specific Ticket rather than for whatever happens to fit.

**Shape: per-pool subtotals under a global total, additively.** `status --json` keeps its flat `workers[]` and `waiting[]` — each gaining a `pool` field — and gains a sibling `pools` key carrying `{name, size, spend, cap, window}` per pool. Nesting Workers under their pools was rejected in *Is this additive, or a redesign?*: `jq '.workers[] | select(.pool == "bugs")'` answers the same question, and that JSON shape changed hours before this effort began.

**`--budget` is a cap inside the caps.** It stays what it is — a per-Dispatch limit — and composes by the same rule: the effective limit is the minimum of the three. Nothing about pools changes its meaning, which is the point.
