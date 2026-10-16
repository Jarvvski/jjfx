# `pool list` prints an unnamed `pool state is absent` for every pool without state

Type: task
Status: resolved
Blocked by: none

## Problem

Found by driving a throwaway Repository end to end. `jjfx pool list` writes `pool state is absent` to **stderr**, once per pool that has no state directory, with no pool named and no explanation:

    $ mkdir -p .jjfx && printf '[pools.alpha]\nsize = 1\n' > .jjfx/config.toml
    $ jjfx pool list
    pool state is absent
    pool state is absent
    Pools:
    POOL      SIZE   WORKERS  ...
    default   -      -        ...
    alpha     1      -        ...
    $ echo $?
    0

Two problems in one line:

- **Absence is the normal case.** Ticket 02's ruling is that a declared pool with no directory is normal - it materialises on first use (ticket 13). So this is not a condition to warn about; it is the default state of a freshly declared lane, and of `default` in every Repository that has not sized it.
- **It names nothing and repeats.** A user with several pools cannot tell which is which, and the line is redundant with the `WORKERS` column already showing `-`.

The command still succeeds, so this is noise rather than a false failure - but it is exactly the kind of unearned diagnostic that teaches a user to ignore stderr.

## Solution

Remove it. If the information is worth keeping, say it **in the table** - the `DECLARED` and `WORKERS` columns already carry it - rather than as an unnamed stderr fragment.

Check whether the same fragment leaks from any other command; `status --json` did not show it, so it looks specific to `pool list`'s per-pool read.

## Acceptance Criteria

- [x] `pool list` on a Repository with declared-but-unmaterialised pools writes nothing to stderr.
- [x] A test covers it, so the fragment cannot come back.
- [x] `mise run check` is green.

## References

- `src/cli.rs` (`pool list` / `pool_report`)
- `.scratch/named-pools/issues/02-persist-per-pool.md` (absence is normal), `13-materialise-a-declared-pool.md`
