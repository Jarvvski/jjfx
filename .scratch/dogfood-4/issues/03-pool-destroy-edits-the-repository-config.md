# `jjfx pool destroy <name>` edits the repository's committed configuration

Status: ready-for-human
Type: bug
Blocked by: none

## Problem

`jjfx pool destroy --force <name>` removed the pool's declaration from the
repository's own `.jjfx/config.toml`, and said so as a side note:

```
$ jjfx pool destroy --force ledger
  worker-153fcb  ledger--02-input-sources  clean
  ...
Removed pool `ledger` from /Users/jarvis/Code/personal/jjfx-drive-4/.jjfx/config.toml
```

`.jjfx/config.toml` is a **tracked file** - the drive's fixture had it committed,
and so does any repository using repository-local pool declarations. A runtime
command silently rewrote it, leaving the working copy dirty and, in this case,
taking the pool's `profile` declaration with it. The change is invisible until
someone runs `jj status` or the next commit sweeps it up.

The declaration is also the only place a named pool's *shape* is recorded, so
removing it changes what the repository means, not just what state exists.

## Evidence

Before:

```toml
[pools.ledger]
size = 4
label = "ledger"
profile = "drive"
```

After `jjfx pool destroy --force ledger`:

```toml
[tickets]
source = "local"

[checks]
command = "python3 -m unittest discover -q"

[dispatch]
max_concurrent = 2
```

The `[pools.ledger]` table is gone from the file, and the fixture's working copy
is dirty. Re-running the destroy after restoring the declaration removed it
again.

## Acceptance Criteria

- [x] `pool destroy` states plainly that it will edit a tracked file. **Done in
      #203**: the removal plan announces the declaration edit - naming the file
      and that it is tracked - before a single Workspace or state file moves.
- [ ] ... and either requires confirmation for that or does not touch the
      declaration at all. **Open, and a decision for the owner.** The
      announcement is the mechanism taken here; requiring confirmation would mean
      every `pool destroy <name>` needs `--force`, which changes the common
      clean-pool case. That trade is the owner's to make, so it is not guessed at.
- [x] If it does edit the declaration, the change is reported in the same way a
      mutation is reported elsewhere, so it cannot be missed. **Done in #203** -
      announced with the rest of the plan, before the removal, rather than as an
      after-the-fact side note.
- [x] A test covers destroying a declared pool and asserts what happens to
      `.jjfx/config.toml`. **Done**: `destroy_pool_removes_the_declaration_but_never_default`
      (unit) and `destroy_refuses_while_it_holds_claimed_tickets_and_force_releases_them`
      (integration, asserting the announcement and the file's fate).
- [ ] Removing the declaration for a pool that is still referenced - by a label,
      by a held Ticket, by a Worker Workspace - is refused rather than silently
      accepted. **Partly done in #203**: a Pool holding claimed Tickets is refused
      by name, and `--force` releases them rather than accepting it silently. A
      label or a bare Worker Workspace is not separately gated.

## Comments

- 2026-10-29: #203 lands the announcement and the held-Ticket refusal. The
  confirmation clause and the wider "still referenced" clause are left for a
  ruling: both would change the shape of an ordinary `pool destroy`.
