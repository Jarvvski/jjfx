# What happens when a Worker Pool is removed?

Type: grilling
Status: resolved
Blocked by: none

## Question

Membership decided that a Worker belongs to one pool for its life and that a pool owns its Workers' Workspaces. `jjfx pool destroy` already tears a pool down and removes its Workers — but removing one lane while others keep working raises questions the single-pool world never had to answer:

- A Worker's Workspace holds a Change chain. On removal, is that chain discarded, left on disk as an ordinary Workspace, or refused until the work is delivered?
- What happens to unmerged or unpushed work? `Clean` removes every non-default Workspace in one sweep — does removing a pool behave that way for its own Workers, and should it?
- Does removing `default` mean anything different from removing a named pool?
- Is there a **drain** — refuse removal while a Worker is busy, or wait for it?

This ties to a known gap outside this map: after `Reset`, a Worker's earlier branches are already unreachable from `open-pr` / `rebase` / `absorb`. Removing a pool multiplies that exposure, and this is the ticket that decides whether it does so silently.

## Answer

Resolved 2026-10-16.

**Removal refuses while the pool has a busy Worker, unless forced.** A drain, not a wait: `pool destroy` refuses and names the busy Workers, because silently killing work in progress is the one outcome nothing can undo. `--force` overrides it, and that is the only path that loses work.

**Removal reports what it will discard before it discards it.** Each Worker holds a Change chain, and the known gap outside this map — after `Reset`, a Worker's earlier branches are already unreachable from `open-pr` / `rebase` / `absorb` — is exactly the exposure that multiplies here. So removal enumerates each Worker's current branch and whether it is pushed or merged, and refuses if any holds undelivered work unless forced. The principle is the map's: never silently.

**Removal does not behave like `Clean`.** `Clean` sweeps every non-default Workspace in the Repository in one act; removing a pool touches only its own Workers' Workspaces. That is lane isolation (*Do the maintenance verbs need a pool scope?*), and it is what makes removing one lane safe while another keeps working. The Workspaces are **removed, not left on disk** as ordinary ones — leaving them would be a slower, quieter version of the same leak, since nothing would ever come back for them.

**`default` is not removable.** A Repository always has at least one pool: `default` is the lane that exists when nothing is declared, and `jjfx pool <N>` targets it forever, which is what makes the additive stance absolute (*Is this additive, or a redesign?*). Removing `default` is refused with that reason. A Repository that wants only named lanes resizes `default` to zero rather than removing it — explicit, reversible, and the invariant holds.

**Removal is an undeclaration.** `pool destroy` removes the `[pools.<name>]` section, and its state directory and its Workers' Workspaces go with it, because *Where are pools declared?* made the declaration the source of truth and the verbs its editors. There is no second source of truth to keep in step, and a Repository that removes its last named pool is back to one implicit `default`.
