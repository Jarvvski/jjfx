# Is the pool index or the pool directories the source of truth?

Type: grilling
Status: resolved
Blocked by: none

## Question

The layout decision introduced two places a pool can be described: `.jj/pools.json` (the index) and `.jj/pools/<name>/` (the directory). They can disagree, and the disagreement is a recovery question rather than a theoretical one:

- A **directory with no index entry** — a pool created, then the index write lost to a crash; or a directory left behind by a partial removal.
- An **index entry with no directory** — the directory deleted by hand, or left by a half-completed conversion.
- A **directory whose pool document disagrees with the index** — a rename that updated one and not the other.

Decide which is authoritative, and what jjfx does when they disagree: reconcile silently, refuse loudly, or treat one as derived and rebuild it from the other.

Two precedents from this map bear on it. The dispatch-ergonomics batch exists to remove silent wrong answers, and the migration decision here chose "verify, then remove" over "assume it worked". A third option exists and is worth weighing: make one of them *derived* — so there is nothing to disagree with.

## Answer

Resolved 2026-10-16. **Neither — the question dissolved when the declaration model landed.** *Where are pools declared?* made the `[pools]` section the source of truth, which leaves the index as a *third* description of the same thing that nothing needs. So **`.jj/pools.json` is dropped**: the config declares, and `.jj/pools/<name>/` is state keyed by pool name. There is no index left to disagree with.

The three disagreement cases reduce to two, and each has a clear answer:

- **A directory with no declaration is an orphan.** jjfx never adopts it — it reports the name and leaves it, because a directory that appeared without a declaration is either a crash's leftovers or a hand-edit, and silently adopting either is the failure class the dispatch-ergonomics batch exists to remove. It does not block other pools.
- **A declared pool with no directory is normal**, not a disagreement: state materialises on first use. That is what makes a committed declaration work on a fresh clone.
- **A directory whose pool document disagrees with the declaration** resolves in the declaration's favour for *declared* fields — size, label, caps — and the state wins for *runtime* fields — workers, occupancy, sessions. Those are different axes, and a state file was never meant to carry the pool's shape.

**This supersedes the index in ADR 0013**, which is annotated to point here. Everything else in that ADR stands: a directory per pool, the pool-scoped-versus-repository-scoped split, the automatic one-time conversion, and the lock ordering. Only the index goes, and the conversion has one less thing to write.

**The cost, stated:** "which pools exist?" becomes a config read rather than a file read. That is the right trade — it is the same file jjfx already reads at startup, and it removes a whole class of recovery question rather than answering it.
