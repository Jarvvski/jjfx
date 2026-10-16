# Is the pool index or the pool directories the source of truth?

Type: grilling
Status: ready-for-human
Blocked by: none

## Question

The layout decision introduced two places a pool can be described: `.jj/pools.json` (the index) and `.jj/pools/<name>/` (the directory). They can disagree, and the disagreement is a recovery question rather than a theoretical one:

- A **directory with no index entry** — a pool created, then the index write lost to a crash; or a directory left behind by a partial removal.
- An **index entry with no directory** — the directory deleted by hand, or left by a half-completed conversion.
- A **directory whose pool document disagrees with the index** — a rename that updated one and not the other.

Decide which is authoritative, and what jjfx does when they disagree: reconcile silently, refuse loudly, or treat one as derived and rebuild it from the other.

Two precedents from this map bear on it. The dispatch-ergonomics batch exists to remove silent wrong answers, and the migration decision here chose "verify, then remove" over "assume it worked". A third option exists and is worth weighing: make one of them *derived* — so there is nothing to disagree with.

## Answer

<!-- recorded on resolution -->
