# The fourth drive: does the artifact users install hold?

Status: open - running

## Why this exists

The first drive (`../dogfood/`) asked *does jjfx work* - ten findings, and
`.scratch/harness/` was built from them. The second (`../dogfood-2/`) asked *does
jjfx make working a Ticket backlog better* - twelve findings. The third
(`../dogfood-3/`) asked whether the landing half held, and who owned the review
loop - seven findings, and the loop came back to jjfx.

All three drove a **development build** (`target/release/jjfx`), and all three
recorded the same two liabilities:

- the drive's own rebuild could silently measure the wrong binary - the third
  drive caught itself doing exactly that, because `mise run build` no-op'd
  against a stale checkout in 0.41s;
- a Worker's login shell resolves `jjfx` to `~/.cargo/bin/jjfx`, a *released*
  binary, so the Worker ran something other than the build under test. The third
  drive scored that "held, by luck": no Worker needed the verb, so it never bit.

This drive removes both by construction and asks the question none of the three
could: **does the released artifact - the thing a user installs - hold?**

## The owner's rulings (2026-10-27)

- **The drive dispatches the released binary, not a local build.** `jjfx 0.56.0`
  is installed from crates.io into `~/.cargo/bin/jjfx`, which is where a Worker's
  login shell resolves it. The drive and its Workers therefore run the *same*
  artifact - the third drive's trap is not worked around, it is gone - and the
  install path itself becomes part of what is under test.
- **The third drive's fixture is gone.** `~/Code/personal/jjfx-drive-3` and
  `~/Code/personal/jjfx-drive-3-workspaces` were removed. The throwaway
  repository `Jarvvski/jjfx-drive-3` is **archived** rather than deleted, because
  deletion needs a `delete_repo` scope the session did not hold; its findings live
  in `../dogfood-3/`.

## The fixture

`~/Code/personal/jjfx-drive-4` - `ledger`, a Python CLI that reads a CSV of
transactions and reports per-account balances, with a real `unittest` suite and
GitHub Actions CI. Origin is a real private throwaway repository,
`Jarvvski/jjfx-drive-4`.

**Eight Tickets** under `.scratch/ledger/issues/`: six ready, two blocked on a
`01 -> 07 -> 08` chain; a mix touching `parse.py`, `report.py`, `format.py` and
`cli.py`; one (05) is a design decision; one (06) is a latent bug that reproduces
from a plain CSV file and is covered by no test.

The repository config declares only what a repository may own: `[tickets]
source = "local"`, a `[checks]` command, and `[dispatch] max_concurrent = 2`
beside a four-Worker pool - the same shape as the second and third drives, so the
three stay comparable.

## Method

- Drive it **the way an agent would**, through jjfx: `ticket list`, the pool, the
  governor, `dispatch --all`, `status --json`, `children`, `wait`, `resume`,
  `review`, and the landing verbs.
- **Test our own claims rather than confirming them** - the discipline of all
  three earlier drives, and the reason they were worth running. The claims to
  falsify are named below.
- Record **every** friction as a Ticket under `.scratch/dogfood-4/issues/`, each
  on its own PR; keep this map and the report separate from that backlog.
- Leave the fixture's evidence behind deliberately: the branches, the PRs, the
  Run records.

## What the drive must settle

1. **The install path.** Does the released artifact work end to end? `cargo
   install`, the GitHub release archive, the published checksums, the Homebrew
   formula, the version string, and the first thing a user does with it. Finding
   49 (`jjfx version` refuses to run outside a jj repository) was found this way
   before the drive even started, which is the argument for aiming here.
2. **The batch that landed since the third drive.** Falsify, under a real
   backlog: the op-log divergence fix (#188 - a failed-push rollback no longer
   undoes a sibling Workspace's operation), the pool-materialisation fix (#190 -
   a lane whose state directory survives without a document is materialised
   again), divergence reporting (#191 - a Worker whose commit diverged is told),
   and the test-hermeticity work (#187).
3. **Landing and the loop.** Does `deliver` → PR → merge on green hold with the
   released binary, with no hand-run `jj push`, and does the bounded review loop
   still terminate on PR review threads?
4. **The Worker trap.** Both sides now run one binary. Is the trap actually gone,
   or does something else reintroduce a mismatch - a Worker's `PATH`, the visor
   shell, a `mise` shim?

## Findings

Recorded as the drive goes, each under `issues/`. All are `needs-triage` until
the drive's triage pass.

| # | Finding | Type |
|---|---|---|
| 01 | A repository-declared pool `profile` is ignored, and the lane runs the default runtime - reproduced twice, with two profile names, after a full destroy and re-materialisation. A named pool has no other route to a non-default runtime. A declared `size` is silently inert too | bug |
| 02 | `[agent] command` is not what runs when a profile is in play, and the mismatch surfaced only as `Failed to authenticate: OAuth session expired` - a credentials message for a configuration problem | bug |
| 03 | `jjfx pool destroy <name>` edits the repository's committed `.jjfx/config.toml`, removing the declaration and dirtying the working copy | bug |
| 04 | Destroying a pool orphans the Tickets it claimed: the files stay `claimed`, the queue is gone, and `resume` reports "No unfinished steps recorded" - recovery was five hand edits | bug |
| 05 | `pool reset <worker>` for a Worker in another pool reports "state is missing" rather than "not in this pool" - the unhelpful-error shape of `bug-hunt/48` | bug |
| 06 | `resume` requires `--pool` with several pools, and its help does not mention it - and `resume` is the recovery path for interrupted work | issue |
| 07 | `--help` works on `pool` but not `pool profile`; `status` has no `--pool` while `ticket list` does; `pool profile` says "every Worker" but reaches only `default` | issue |
| 08 | A Ticket excluded because its status is not the lane's is reported as `blocked`, the same word used for an unresolved `Blocked by:` | issue |

Finding `49` in `bug-hunt/` - `jjfx version` refusing to run outside a jj
repository - was found by unpacking the published `v0.56.0` archive before this
drive started, which is the argument for aiming at the released artifact.

## References

- `../dogfood/` - the first drive, its report and its ten findings
- `../dogfood-2/` - the second drive, its map, report and twelve findings
- `../dogfood-3/` - the third drive, and the claim table this one extends
- `docs/adr/0016-jjfx-is-the-deterministic-control-plane.md` - the review bound
- `docs/review-loop.md` - the loop as documented today
- `docs/releasing.md` - the install and release paths under test
- `skills/jjfx-dispatch/` - the playbook the drive follows
