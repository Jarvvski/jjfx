# The third drive: does jjfx own the loop, and does it hold on a clean config?

Status: driven 2026-10-21 - see `report.md`; four findings filed, the loop's
implementation in flight

## Why this exists

The first drive (`../dogfood/`) asked *does jjfx work* - ten findings, and
`.scratch/harness/` was built from them. The second (`../dogfood-2/`) asked
*does jjfx make working a Ticket backlog better* - twelve findings, all worked
to `resolved`. Its judgement was blunt: **jjfx is a good dispatch control plane
and a broken landing one, and the review loop it was built to bound is not
reachable.**

This drive is the third pass, and it asks the two questions the second could
not answer:

- **Does the landing half hold now?** The second drive's landing failures were
  fixed as a batch (`deliver`, the fresh-bookmark read, rebase atomicity,
  conflicted-rebase reporting). The drive must try to *falsify* that, not
  demonstrate it.
- **Who owns the loop?** The second drive left it open: `jjfx review` sends a
  prompt and produces no findings, and no command invokes `ReviewLoop`. The
  owner has ruled (below) that jjfx owns it again, with the PR's review threads
  as the findings source.

## The owner's rulings (2026-10-21)

- **jjfx owns the review loop again, sourced from PR review threads.** Not an
  external review bot's ingestion (which ADR 0016 cut), and not our own reviewer
  agent: the findings are the unresolved review threads on the Pull Request,
  read through `gh`. The loop's *bound* stays in jjfx (ADR 0016); only the
  findings source is named.
- **The first drive's fixtures are gone.** `dogfood-site`,
  `dogfood-site-origin.git` and `dogfood-site-workspaces` were removed; their
  findings live in `../dogfood/`.

## The fixture

`~/Code/personal/jjfx-drive-3` - `tally`, a Python CLI that summarises JSONL log
records, with a real `unittest` suite (41 tests) and GitHub Actions CI. Origin is
a real private throwaway repository, `Jarvvski/jjfx-drive-3`.

**Eight Tickets** under `.scratch/tally/issues/`: six ready, two blocked on a
`01 -> 07 -> 08` chain; four touch `cli.py` / `aggregate.py` / `format.py`; one
(05) is a design decision; one (06) is a bug that reproduces from a plain file.

The repository config declares only what a repository may own: `[tickets]
source = "local"`, a `[checks]` command, and `[dispatch] max_concurrent = 2`
beside a four-Worker pool - the same shape as the second drive, so the two are
comparable.

## The binary: a local build, and the trap it sets

The drive runs `jjfx` built from this repository's `main`
(`target/release/jjfx`, `0.55.0-dev`), pinned first on `PATH` and verified.

**The trap the second drive found is still live, and this drive must measure it
rather than assume it away.** A Worker is launched through a login interactive
shell, which re-reads the shell config - so the Worker's `jjfx` is
`~/.cargo/bin/jjfx` (a released binary), not the build the drive is running.
The dispatch prompt now names `jjfx deliver <worker>` in both the system and the
worker prompt, and a released binary has no such verb. Whether that matters in
practice is a question for the drive: the prompt also tells the Worker not to
push, so a Worker that obeys never calls it.

## Method

- Drive it **the way an agent would**, through jjfx: `ticket list`, the pool, the
  governor, `dispatch --all`, `status --json`, `children`, `wait`, `resume`,
  and the landing verbs.
- **Test our own claims rather than confirming them** - the discipline of both
  earlier drives, and the reason they were worth running.
- Record **every** friction as a Ticket under `.scratch/dogfood-3/issues/`, each
  on its own PR; keep this map and the report separate from that backlog.
- Leave the fixture's evidence behind deliberately: the branches, the PRs, the
  Run records.

## What the drive must settle

1. **Landing.** Can an agent working a backlog land its work through jjfx alone -
   `deliver`, then the PR, then merge on green - with no hand-run `jj push`?
2. **The loop.** With PR review threads as the findings source, does the bounded
   loop actually terminate, and does it notice a dimension that recurs?
3. **A clean config.** The second drive's worst bug was masked by this machine's
   `remotes.origin.auto-track-bookmarks` and by an inherited jj identity. The
   drive must not accept a green result that only holds with the developer's
   config in place.

## References

- `../dogfood/` - the first drive, its report and its ten findings
- `../dogfood-2/` - the second drive, its map, report and twelve findings
- `docs/adr/0016-jjfx-is-the-deterministic-control-plane.md` - the review bound
- `docs/review-loop.md` - the loop as documented today
- `skills/jjfx-dispatch/` - the playbook the drive follows
