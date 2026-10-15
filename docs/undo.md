# Undoing a jjfx mutation

jj snapshots every mutation in a repository-wide operation log. jjfx records the
operation the repository was at before each mutation it initiates, so a Forge,
weld, tidy or Reset that did something the owner did not want can be undone from
the tool rather than by reasoning about `jj op log` by hand.

## What is recorded

Before a jjfx-initiated mutation, jjfx records the repository's newest operation
id; when the mutation finishes it records the operation it finished at. The pair
lives in a jjfx-owned sidecar, `.jj/pool/undo.json`, beside jjfx's other owned
state (`sessions.json`, `runs.jsonl`, `in-use.json`). A compatibility peer
ignores it, and a missing file means "nothing recorded to undo".

One record is kept per Workspace - the latest mutation - because an undo restores
the operation the repository was at *before* it, which already subsumes every
earlier jjfx-initiated mutation. A mutation that changed nothing is not recorded,
so it cannot displace the record of the last mutation that did.

The recorded mutations are:

| Mutation | Recorded as |
|---|---|
| A Forge run's weld, fix and push for one Workspace | that Workspace's `forge` |
| A lift/weld of one Workspace | that Workspace's `weld` |
| A Reset's Workspace restoration | that Workspace's `reset` |
| A tidy or tidyws sweep | repository-wide `tidy` |
| A lift of every Workspace | repository-wide `weld` |

A tidy, a tidyws sweep or a lift of every Workspace is not confined to one
Workspace, so it is recorded under the reserved Workspace key `*` and is never
mixed with a single Workspace's record.

## The primitive, and why

Undo is `jj op restore <checkpoint>`, where `<checkpoint>` is the operation jjfx
recorded *before* the mutation.

`jj op revert <op>` was tried first and rejected. It reverses one operation's
effect, but it does **not** move any working copy back: after reverting a rebase,
the Workspace's `@` still descended from the rebased commits and the pre-rebase
commits came back as *additional* commits, so the Workspace was not restored. A
jjfx mutation is also a sequence of operations - a Forge is fetch, weld, fix and
push - and reverting one of them is not an undo.

`jj undo` is rejected too: it is `jj op restore` to the previous operation, with
the same repository-wide reach and no way to say what it would discard.

## The scoping rule

`jj op restore` is repository-wide: it discards every operation after the
checkpoint, in every Workspace. jjfx therefore **refuses unless the mutation is
still the repository's tip**. With the checkpoint it also records the operation
that was the tip when the mutation finished, and an undo runs only while the
current tip is still that recorded end - nothing at all has happened since, in
any Workspace. The operations between the checkpoint and the tip are then exactly
jjfx's own mutation, so restoring to the checkpoint cannot discard another
Workspace's work.

When any operation has happened since - another Workspace's Forge, an agent's
commit, a fetch - the undo is refused and the refusal reports how many later
operations a restore would discard. `jjfx undo <workspace> --force` is the
explicit, warned override; the Pool view never forces.

Because jj does not record who created an operation, an operation created by
another process *inside* a mutation's own window is indistinguishable from jjfx's
and a restore would discard it. Recording per Workspace and refusing on anything
later narrows that window to a single mutation; closing it entirely needs jj to
tag operations.

## Using it

`jjfx undo` lists the recorded mutations. `jjfx undo <workspace>` undoes the one
recorded for that Workspace, accepting a Worker in either spelling (`3` or
`worker-3`), any Workspace name, or `*`/`repo` for a repository-wide mutation.
`--force` discards the later operations the refusal names.

In the TUI, `u` in the Pool view asks for confirmation and then submits the same
undo for the selected Worker, reporting the outcome - including a refusal and its
reason - in the status line.

The restore is itself an operation, so an undo can be undone with `jj op log` and
`jj op restore` exactly as any other jj mutation.
