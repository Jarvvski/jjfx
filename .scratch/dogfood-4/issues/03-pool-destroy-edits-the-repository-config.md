# `jjfx pool destroy <name>` edits the repository's committed configuration

Status: needs-triage
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

- [ ] `pool destroy` states plainly that it will edit a tracked file, and either
      requires confirmation for that or does not touch the declaration at all -
      destroying runtime state and editing repository configuration are
      different acts.
- [ ] If it does edit the declaration, the change is reported in the same way a
      mutation is reported elsewhere, so it cannot be missed.
- [ ] A test covers destroying a declared pool and asserts what happens to
      `.jjfx/config.toml`.
- [ ] Removing the declaration for a pool that is still referenced - by a label,
      by a held Ticket, by a Worker Workspace - is refused rather than silently
      accepted.
