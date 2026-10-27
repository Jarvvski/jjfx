# The fourth dogfood drive: does the artifact users install hold?

A first-hand report from driving the `ledger` backlog (eight Tickets, a real
`unittest` suite, real GitHub CI) through the **released** artifact. Every claim
carries the command that produced it and the output it produced. The friction is
filed as Tickets under `.scratch/dogfood-4/issues/`; this report is separate from
that backlog, as the three earlier drives' were.

Binary under test: `~/.cargo/bin/jjfx`, `jjfx 0.56.0`, installed from crates.io
with `cargo install jjfx --version 0.56.0 --locked`. That path is the point of the
drive: a Worker's login shell resolves `jjfx` there too, so the drive and its
Workers ran the *same* artifact and the third drive's two liabilities - a
development build that could silently measure the wrong binary, and a Worker
running a released binary instead - are gone by construction rather than worked
around. `jj 0.46.0`, `gh`, opencode, Python fixture.

Fixture: `/Users/jarvis/Code/personal/jjfx-drive-4` - `ledger`, a Python CLI that
reads a CSV of transactions and reports per-account balances, 45 tests to begin
with. Eight Tickets, six ready and two blocked on a `01 -> 07 -> 08` chain, a mix
touching `parse.py`, `report.py`, `format.py` and `cli.py`, one (05) a design
decision, one (06) a latent bug covered by no test. `origin` is a real private
throwaway repository, `Jarvvski/jjfx-drive-4`, with GitHub Actions CI.

---

## 0. The shape of the drive

Driven the way an agent would: `jjfx ticket list` to choose work, a four-Worker
pool under `[dispatch] max_concurrent = 2`, `dispatch --all`, `status`, `logs`,
`wait`, `resume`, `rebase`, `review`, and the forge path. The orchestrator was
this agent; jjfx spawned every Worker.

**Result: seven of the eight Tickets resolved and verified, and four of the six
delivered Pull Requests landed.** Ticket 05 is held for a human by design and was
never dispatched - correctly. `runs.jsonl` records `succeeded` and `check: passed`
for all seven Runs, the fixture's CI is green on every Pull Request, and the
released binary spent **$0.3926** doing it.

Two Pull Requests are open on a merge conflict and one change was never pushed at
all - both are deliberate evidence, and both are findings, not accidents (§3.1,
§3.2). Getting the drive to launch at all took a long detour through pool
configuration (§3.4).

---

## 1. What worked, and should not be refactored away

- **`jjfx deliver` performs the whole delivery, on the released artifact.** Six
  Pull Requests were opened by Workers running `0.56.0`, each with the Worker's
  own commit description as its title and body, with **no hand-run `jj push`
  anywhere in the drive**. Four merged on green. The weld, the repository's
  `[checks]` command, the push and the Pull Request all came from the one forge
  path ADR 0016 describes.
- **The concurrency governor holds, across dispatches.** With a four-Worker pool
  and four ready Tickets, `dispatch --all` fired two and held the rest by name:
  `ledger--06 … waiting: held back by the concurrency cap (dispatch.max_concurrent
  = 2): 2 Run(s) in flight`. Every later batch held its excess the same way.
- **Verified done is real.** All seven Runs record `check: passed` - jjfx running
  the repository's own check on the agent's tree - and the Ticket is only rewritten
  to `Status: resolved` after that. Nothing resolved a Ticket on an agent's word.
- **Cost is recorded for opencode Runs.** Drive 3's finding 04 said opencode Runs
  record no cost, so the spend cap could not bind. That fix holds under a real
  backlog: every Run in `runs.jsonl` carries `cost_micro_usd`, and `status` shows a
  per-Worker spend that the pool totals - `$0.3926` across seven Runs, from
  `$0.0282` for a one-flag change to `$0.1294` for the largest.
- **Dependency readiness readies a dependent with no reconcile.** `ledger--07`
  read `blocked by ledger--01-date-filtering (ready-for-agent)`; the moment 01
  resolved, `ticket list` showed 07 `ready` on its own, and 08 stayed blocked on
  07 - including while 07's Run was in flight, which it reported as `blocked by
  ledger--07-monthly-summary (claimed)`.
- **A finished Worker is returned to the pool.** Two Workers carried seven Runs,
  the other two pool members were never needed, and no dispatch reported "grow the
  pool".
- **`jjfx logs` follows a session-delivered Run.** A Worker's process log is empty
  by design for opencode - the activity lives in the session - and `jjfx logs`
  follows the session instead of an empty file, printing the agent's own reasoning.
  This is how the drive read what each Worker decided (§3.1).
- **A repository can choose its Pull Request policy, on the released binary.**
  Drive 3's finding 01 said every delivered Pull Request is a draft and a
  repository cannot say otherwise; the fix is in `0.56.0`. The fixture set
  `[forge] draft = false` in its own `.jjfx/config.toml` - it **parsed**, with no
  "a repository may set only …" refusal - and the next delivery, `#6`, came out
  `[OPEN ready]` rather than a draft, while `#1`–`#5` had all been drafts.
- **Re-welding by name exists, and the conflict path is exemplary.** Drive 3's
  finding 02 was that nothing in jjfx could update a branch once trunk moved; the
  fix is in `0.56.0`. When 01 merged and moved trunk under 02, `jjfx rebase
  adam/ledger--02-input-sources` rebased it and reported:

  ```
  Error: rebased branch adam/ledger--02-input-sources onto main, but it has
  conflicts to resolve: resolve them in Workspace branch-adam-ledger--02-input-sources
  (`jjfx resolve branch-adam-ledger--02-input-sources`), then re-run to push; the
  rebase is left in place
  ```

  That names the conflict, the Workspace, the command to resolve in, and the fact
  that the rebase is left in place. It is the standard the rest of §3 is measured
  against - except for the silent no-op that precedes it (§3.2).
- **The refusal to review a merged Pull Request is correct and precise.** `jjfx
  review 70cae3` resolved its Worker's branch and said: `pull request for branch
  adam/ledger--08-monthly-sparkline is merged; there is nothing left to review`.
  The machinery works; §3.3 is about reaching it.

---

## 2. The claims the drive set out to falsify

| # | Claim | Verdict |
|---|---|---|
| 1 | The released artifact works end to end - the install path, the version string, the first thing a user does | **Held, with one hole.** `cargo install jjfx --version 0.56.0 --locked` produced a working binary that drove the whole backlog. Unpacking the published archive found finding 49 (`jjfx version` refuses to run outside a jj repository) before the drive started. The Homebrew formula and the published checksums were not exercised in this session |
| 2 | The batch landed since the third drive holds under a real backlog - the op-log divergence fix (#188), pool materialisation (#190), divergence reporting (#191), test hermeticity (#187) | **Held where reached.** Pool destroy and re-materialisation behaved (#190); the drive's own failed-push paths never triggered #188; the fixture's CI was green throughout (#187's shape). Divergence reporting (#191) was not exercised - no Worker diverged |
| 3 | `deliver` → PR → merge on green holds with the released binary and no hand-run `jj push`, and the bounded review loop still terminates on review threads | **Half held, half unreachable.** Delivery held: six PRs, four merged, no hand-run push. The review loop **could not be reached at all** on any of the drive's Pull Requests (§3.3) |
| 4 | The Worker trap is gone - both sides run one binary | **Held.** Every Worker's `jjfx` resolved to `~/.cargo/bin/jjfx` (`0.56.0`), the same artifact the drive dispatched. The install path is now part of what is under test, and finding 49 came out of it |

---

## 3. What did not hold

### 3.1 A Run can report success while its change is never delivered - finding 09

The Dispatch prompt tells the agent that **jjfx** delivers the change, and forbids
it from pushing or opening the Pull Request itself. Nothing in Dispatch runs
`jjfx deliver`: `forge.deliver` has exactly one call site, the CLI verb, and the
TUI forges only on an explicit keypress. The outcome is decided by how each agent
reads an ambiguous sentence, and the losing outcome is silent.

Ticket 03's agent read it literally:

> Bookmark `adam/ledger--03-account-filtering` created on the described commit. I
> did **not** push or open a PR - per instructions, `jjfx deliver` handles
> weld/check/push/PR.

Its change - 119 insertions, tests included - is on the local Workspace only: no
push op in the shared repository's operation log, no branch on the remote, no Pull
Request. The Run still records `succeeded`, `check: passed`, `exit_code: 0`,
`error: null`, and the Ticket reads `resolved`.

Ticket 04's agent, under the identical prompt, went back and forth for a long
stretch of its session - *"It's ambiguous whether I should run `jjfx deliver`"* …
*"Hmm, conflicting"* … *"I'll run it"* - and delivered. Ticket 06's agent spent 33
lines on the same question. Four of the five agents that reached step 8 agonised
over it; one obeyed and was stranded.

This is the drive's central question - *can an agent land a change through jjfx
alone?* - answered **only when the agent happens to misread the prompt in the
permissive direction**. See [`09`](issues/09-a-run-can-report-success-without-delivering.md).

### 3.2 `jjfx rebase` reports `Rebased` while rebasing nothing - finding 10

After 01 merged on the forge, 02 and 04 conflicted. The prompt's advertised
recovery is `jjfx rebase <bookmark>`; run against a repository whose local trunk
had not been fetched, it reported success twice while doing nothing:

```
$ jjfx rebase adam/ledger--02-input-sources
Rebased adam/ledger--02-input-sources
$ gh api repos/…/commits/adam/ledger--02-input-sources --jq .sha
3d89bc04beb77176354088b49757a598449ba059     # unchanged
```

No rebase op, no push op, remote head byte-for-byte the same, Pull Request still
`CONFLICTING`. With `jj git fetch` first, the same command rebased properly and
produced the excellent conflict message quoted in §1. `jjfx` has no `fetch` verb -
only `sync`, which re-syncs one Workspace - so the repository-wide fetch the
recovery depends on is an operator step the tool neither performs nor mentions.

The two conflicting Pull Requests, `#2` and `#3`, are left open as this finding's
evidence. See [`10`](issues/10-rebase-reports-success-without-rebasing.md).

### 3.3 `jjfx review` cannot reach a Pull Request whose Worker has moved on - finding 11

The Dispatch prompt states the principle - *"Your bookmark, not your Worker, is the
durable name of your change"* - and `deliver` and `rebase` both accept a bookmark.
`review` does not; it takes only a Worker. Since a Worker's branch handle is
cleared when it is reset to idle, the loop is reachable only in the window before
the Worker is reused - which is not when a human reviews a Pull Request:

```
$ jjfx review adam/ledger--04-json-output
Error: Worker identifier "worker-adam/ledger--04-json-output" is not a safe filename component
$ jjfx review 79bdfa                      # the Worker that delivered the PR
Error: Worker worker-79bdfa has no branch
```

Four of the drive's six delivered Pull Requests had their Workers reused before any
review could happen, and none of them can be reviewed now. The drive's question 3
therefore has no answer on the released artifact: the loop is not broken, it is
unreachable exactly when it is needed. See
[`11`](issues/11-review-cannot-address-a-reused-workers-pr.md).

### 3.4 Getting the drive to launch took a detour that is itself four findings

The drive's first two attempts died at once with `Failed to authenticate: OAuth
session expired` - a credentials message for a configuration problem. The runtime
that actually ran was not the one configured: a repository-declared pool `profile`
is ignored (finding 01), and `[agent] command` is not what runs when a profile is
in play (finding 02). `jjfx pool profile <runtime>` reaches only `default`, so for
a named pool there was no route to a non-default runtime at all.

Recovering cost more friction: `pool destroy` edits the repository's committed
`.jjfx/config.toml` (finding 03) and orphans the Tickets it claimed, with no verb
to release them (finding 04) - recovery was five hand edits. `pool reset` for a
Worker in another pool reports "state is missing" (finding 05). `resume` requires
`--pool` and its help omits it (finding 06). `--help` works on `pool` but not
`pool profile`; `status` has no `--pool` while `ticket list` does (finding 07). And
`ticket list` reports a Ticket that is simply not this lane's - or one with a Run
**busy on it right now** - as `blocked`, the same word it uses for an unresolved
`Blocked by:` (finding 08).

---

## 4. What the drive leaves behind

- **Findings**, all `needs-triage`, under `.scratch/dogfood-4/issues/` (01–11), with
  the commands and output that produced each.
- **The fixture**, `~/Code/personal/jjfx-drive-4` and `Jarvvski/jjfx-drive-4`, left
  with its evidence: `#1`, `#4`, `#5`, `#6` merged; `#2` and `#3` open on a merge
  conflict; ticket 03's change stranded locally with no Pull Request at all; and
  `runs.jsonl` recording `succeeded` / `check: passed` for every one of the seven
  Runs, including the stranded one.
- **The install path**, now part of what is under test: the drive ran the artifact a
  user installs, and a Worker's login shell resolved the same binary.

The friction is not in the parts that were built to be clever. It is in the two
places where jjfx says one thing and does another: a prompt that promises a
delivery no component performs, and a rebase that reports work it did not do. Both
cost the drive real work, and neither announced itself.
