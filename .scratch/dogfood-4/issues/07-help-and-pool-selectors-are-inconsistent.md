# Help and pool selectors are inconsistent between commands

Status: resolved
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

- [x] `--help` behaves the same at every command depth, or is refused with a
      message naming `jjfx help <command>`. **Done in #206** (the second form):
      `jjfx pool profile --help` now says `` `--help` is not a flag of `pool
      profile`; use `jjfx help pool profile` for its help page `` rather than
      "unknown option". The page form works at every depth already.
- [x] Every pool-scoped verb either takes `--pool` or says in its help which pool
      it addresses. **Done in #206**: `status`/`pool list` gained `--pool NAME`,
      which narrows the whole view - summary row, Workers, held Tickets and the
      total's share - to one lane, and `pool profile`'s page now says it sets the
      `default` Pool's runtime and how a *named* Pool's runtime is set instead
      (its declaration). `pool reset`/`rm` resolve the Worker's own Pool (#205).
- [x] `pool profile`'s help states the scope it actually applies to. **Done in
      #206**: the summary is "Set the `default` Pool's runtime profile" and the
      page explains that a named Pool's runtime comes from
      `[pools.<name>] profile = "<granted-profile>"`.

## Comments

- 2026-10-29: #206 closes the three. `--help` is refused with the page it should
  have been, rather than made to work at every depth - `--text --help` is a
  dispatch whose *value* is the string `--help`, so a blanket "any `--help` means
  show help" would silently swallow it; the page form is unambiguous.
