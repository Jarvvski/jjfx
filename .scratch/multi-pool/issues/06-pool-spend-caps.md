# How do a pool's spend cap and the global ceiling interact?

Type: grilling
Status: ready-for-human
Blocked by: none

## Question

Each pool gets its own spend cap, under a global ceiling that still binds. Settle the semantics:

- **Precedence.** A pool is under its cap but the global ceiling is reached — does the pool's Dispatch refuse, hold Tickets back, or proceed? Which limit wins, and is the refusal attributed to the pool or to the Repository?
- **What hitting a cap does.** The `waiting` set already exists for Tickets a capped Dispatch held back, and is carried in `status --json`. Should a cap-held Ticket appear there, so it reads as "held, deliberately" rather than "did not happen"? This is the same honesty problem the dispatch-ergonomics batch exists to fix — a cap that silently drops work is the thing that batch is about.
- **Attribution and shape.** Spend is reported per Worker today and pooled by the Repository. What is the new shape — per-pool subtotals under a global total? And does `--budget` on a single Dispatch interact with a pool cap (a per-dispatch cap *inside* a per-pool cap)?

Builds on the Run Record work that landed today: every finalized Run now appends a durable record carrying its cost, whether it ended under the supervisor or was reconciled later. That is what makes per-pool accounting possible at all — before it, spend was always `$0.0000 across 0 Run(s)`.

## Answer

<!-- recorded on resolution -->
