# What happens when a Worker Pool is removed?

Type: grilling
Status: ready-for-human
Blocked by: none

## Question

Membership decided that a Worker belongs to one pool for its life and that a pool owns its Workers' Workspaces. `jjfx pool destroy` already tears a pool down and removes its Workers — but removing one lane while others keep working raises questions the single-pool world never had to answer:

- A Worker's Workspace holds a Change chain. On removal, is that chain discarded, left on disk as an ordinary Workspace, or refused until the work is delivered?
- What happens to unmerged or unpushed work? `Clean` removes every non-default Workspace in one sweep — does removing a pool behave that way for its own Workers, and should it?
- Does removing `default` mean anything different from removing a named pool?
- Is there a **drain** — refuse removal while a Worker is busy, or wait for it?

This ties to a known gap outside this map: after `Reset`, a Worker's earlier branches are already unreachable from `open-pr` / `rebase` / `absorb`. Removing a pool multiplies that exposure, and this is the ticket that decides whether it does so silently.

## Answer

<!-- recorded on resolution -->
