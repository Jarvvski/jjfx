# Does the queued dispatch-ergonomics batch wait for this design?

Type: grilling
Status: ready-for-human
Blocked by: none

## Question

This is a sequencing decision with a deadline, because a batch is already queued and unblocked.

`.scratch/dispatch-ergonomics/` holds nine unresolved tickets, of which two change shape if pools land:

- **`jjfx wait`** — its `--all` and its target set need a pool dimension. Build it as specified and it gets extended days later; the flag surface it publishes would be wrong almost immediately.
- **`--all` must base a dependent on its blocker** — orthogonal to pools, but it is what makes more parallel branches in one Repository survivable, and more pools is exactly what multiplies them. This is the map's biggest unwritten dependency: shipping pools without it gives a faster way to create integration conflicts.

Decide one of:

1. **Amend before dispatch** — add the pool dimension to `jjfx wait` and note the stacking ticket's relationship *before* either is picked up. Cheapest, but needs this map's routing answer first.
2. **Land as written, extend later** — accept a follow-up change to a flag surface that is days old.
3. **Hold both** until the multi-pool design lands.

Whichever is chosen, record it here so the next session picking up either ticket sees it. This is the one ticket on the map whose answer expires.

## Answer

<!-- recorded on resolution -->
