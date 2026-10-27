# Help and pool selectors are inconsistent between commands

Status: needs-triage
Type: issue
Blocked by: none

## Problem

Three small inconsistencies, found while driving a Repository with two pools. Each
is minor; together they make the pool surface hard to predict.

**1. `--help` works at one level and not the next.**

```
$ jjfx pool --help
jjfx pool - Manage the Repository's Worker Pools
Usage: jjfx pool [<N>]
Commands:
  jjfx pool create <name> ...

$ jjfx pool profile --help
Error: unknown option for pool profile: --help
```

The changelog says every command has a help page, reached as `jjfx help
<command>`. `jjfx help pool profile` does work - but a user who has just learned
that `--help` works on `pool` reasonably tries it on `pool profile`.

**2. A pool selector exists where it is least needed and is absent where it is
most.**

```
$ jjfx ticket list --pool ledger     # accepted
$ jjfx status --pool ledger
Error: unknown option for status: --pool
```

`status` is the verb an operator uses to watch Runs, and it prints every pool's
Workers in one table. A `--pool` filter is what `ticket list` already offers.

**3. `pool profile` addresses only `default`, and says nothing about it.**

```
$ jjfx pool profile opencode
Pool profile set to opencode
$ jjfx status | grep Profile
Profile (default): opencode
Profile (ledger): claude
```

Its help reads "Set the runtime profile every Worker runs" - "every Worker" is
what it says, `default` only is what it does. (The named-pool half of this is
[01](01-a-declared-pool-profile-is-ignored.md); this ticket is the reporting
inconsistency.)

## Acceptance Criteria

- [ ] `--help` behaves the same at every command depth, or is refused with a
      message naming `jjfx help <command>`.
- [ ] Every pool-scoped verb either takes `--pool` or says in its help which pool
      it addresses.
- [ ] `pool profile`'s help states the scope it actually applies to.
