# Does the queued dispatch-ergonomics batch wait for this design?

Type: grilling
Status: resolved
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

Resolved 2026-10-16. **Option 2 — both land as written**, plus one sequencing note, because the deadline this ticket was written against has dissolved.

**`jjfx wait` needs no amendment, because the rule it would inherit already generalises.** The concern was that its `--all` and its target set need a pool dimension, so the flag surface would be wrong days later. But *How does a Ticket reach a pool?* settled a rule about *the number of pools*, not about `dispatch`: one pool implies the target, several require `--pool`. So `jjfx wait --all` means "every Worker in the selected pool" exactly as `dispatch --all` means "every ready Ticket in the selected pool" — the flag surface does not change, it becomes *narrower* until pools land. Building it now costs nothing to extend and publishes nothing false.

**The stacking ticket stays as written.** `--all` basing a dependent on its blocker is orthogonal to pools; it is what makes more parallel branches in one Repository survivable.

**The sequencing note is this ticket's real output.** Shipping pools multiplies parallel branches in a Repository, and the stacking behaviour is what keeps that from becoming an integration-conflict generator. So `--all` must base a dependent on its blocker should land **before or with** the pools implementation. That is the map's biggest unwritten dependency, and the one thing here that is actively worse to ship pools without.

Option 1 is rejected as unnecessary now that the routing answer shows no amendment is needed. Option 3 is rejected because it would hold an orthogonal, already-designed ticket hostage to a design that is not built.

**One more thing this ticket is the right place to record.** The batch's "deliberately not filed" list holds *`[tickets]` cannot be repository-local*, left as an owner decision because `deny_unknown_fields` is deliberate. *Where are pools declared?* answers it: the capability rule permits a repository to **select** a configured Ticket source, so a repo-local `[tickets] source = "local"` is within the policy — a reference to something the user's global config defines, granting nothing. The wall the `todo-md` test-drive hit is removable, and that line is annotated on the batch's map.
