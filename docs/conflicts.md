# Conflicts

In jj a conflict is stored **in the commit**. A rebase that hits one succeeds and
records the conflict, so a conflicted chain is a normal, recoverable state - not
a failed operation. jjfx treats it that way: a conflicted chain is its own Work
state, the forge refuses to push it *by name*, and `jjfx resolve` clears it.

## Reading the state

The read is jj's own machine-readable `conflicts()` revset, over the whole
change chain (`(trunk()..<workspace>@) & conflicts()`) - the same source the
forge's push gate uses, so the surfaced state and the refusal agree by
construction rather than by parsing jj's status prose.

That revset matches every commit whose *tree* holds a conflict, and a commit
above a conflicted one inherits it. A chain therefore reports several conflicted
commits for one conflict. The state names the **owner**: the deepest conflicted
commit, which is the one a rebase rewrote. Every commit above it merely inherits
the conflict, so resolving the owner clears them all.

The commit is named by its short **change id**, not its commit id. The commit id
is rewritten the instant the conflict is resolved, while the change id is the
identity jj's own commands take (`jj resolve -r <change>`, `jj new <change>`), so
the name the list shows is one the owner can act on directly.

The state is read over the whole chain rather than the workspace's owned subset:
a conflict in a shared base is still a conflict the forge will refuse to push,
and hiding it behind the ownership rule would leave a workspace reading as merely
dirty while it cannot be forged at all.

## Where it shows up

| Surface | Behaviour |
|---|---|
| The list | The row leads the **needs-you** lane, in the blocked red, naming the commit (`conflict kqxsylvyrwvq`) |
| The forge | Refuses the target, naming the commit and the Workspace: ``conflict in <change> - resolve it in Workspace <ws> (`jjfx resolve <ws>`), then re-run to push`` |
| The CLI | `jjfx resolve <workspace> [--tool <name>]` |
| The TUI | `C` on the selected workspace, which opens a tab running the CLI command |

A conflicted chain leads the needs-you lane even while an agent is mid-turn.
Unlike a changes-requested PR - where the agent is expected to keep iterating -
a conflict blocks the forge outright, so the human has to decide whether to
resolve it or hand it to an agent.

The forge re-checks for a conflict after `jj fix` as well as after the weld. The
push revset excludes conflicts, so a conflict buried by a formatter would
otherwise vanish from the push silently; now it is refused, by name, at both
points.

## Resolving

The primitive is revision-addressed:

```
jj resolve -r <change> [--tool <name>]
```

`jj resolve -r <change>` rewrites that commit and rebases its descendants.
Without `--tool` the user's own configured merge tool runs; `--tool :ours` and
`--tool :theirs` are jj's built-in one-sided picks. jjfx resolves the chain's
conflicted commits deepest first, re-reading after each one and stopping as soon
as the set stops shrinking - so a merge tool that exits without changing anything
cannot spin.

A conflicted **re-weld** names the Workspace to resolve in, so the round trip is
one command, one resolve, one command:

```
$ jjfx rebase adam/tally--04-json-output
Error: rebased branch adam/tally--04-json-output onto main, but it has conflicts to
resolve: resolve them in Workspace branch-adam-tally--04-json-output
(`jjfx resolve branch-adam-tally--04-json-output`), then re-run to push; the rebase
is left in place
$ jjfx resolve branch-adam-tally--04-json-output
$ jjfx rebase adam/tally--04-json-output
```

When no Workspace held the change - the Worker that delivered it has been reused
- jjfx created the Workspace it names, and left it in place for exactly this. See
`docs/configuration.md` ("Naming the change").

A resolve rewrites a commit *below* the working copy, which leaves the
Workspace's working copy stale, so `jj workspace update-stale` runs at the end.

The merge tool is interactive. The CLI therefore runs it with this process's
terminal inherited, and the TUI opens a tab running the same CLI command: jjfx
cannot host a merge tool inside its own alternate screen, and a terminal is the
one thing it does not have.
