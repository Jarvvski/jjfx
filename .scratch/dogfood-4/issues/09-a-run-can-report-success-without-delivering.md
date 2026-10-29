# A Run reports success while its change is never delivered, because nothing runs the delivery the prompt promises

Status: ready-for-human
Type: bug
Blocked by: none

## Problem

The Dispatch prompt tells the agent that **jjfx** delivers the change:

> Name your change's bookmark `adam/<ticket>-<short-description>` … jjfx delivers the change to Jarvvski/jjfx-drive-4 with `jjfx deliver <worker>`: it rebases your bookmark onto trunk, runs the repository's check, pushes it, and opens one Pull Request. **Do not push or open the Pull Request yourself.**

Step 8 of the Worker prompt repeats it: *"jjfx delivers the change with `jjfx deliver <worker>` … Do not push or open the Pull Request yourself."*

Nothing in Dispatch runs `jjfx deliver`. There is exactly one call site of
`forge.deliver` in the tree - the `deliver` CLI verb (`src/cli.rs:5799`) - and the
TUI drives the same pipeline only from an explicit keypress (`f`, `g`, `F`;
`src/app.rs:2438-2477`). So the prompt describes a delivery that no component
performs, while forbidding the agent from doing it by hand.

The outcome is decided by how each agent reads an ambiguous instruction, and the
losing outcome is **silent**.

Ticket 03's agent read it literally and stopped, saying so in its own summary:

> **Delivery state** - Bookmark `adam/ledger--03-account-filtering` created on the
> described commit. I did **not** push or open a PR - per instructions, `jjfx
> deliver` handles weld/check/push/PR.

Ticket 04's agent, under the identical prompt, spent a long stretch of its
session reasoning in circles about the same sentence before deciding to run it:

> "It's ambiguous whether I should run `jjfx deliver`."
> "I think the safest reading: the orchestrator runs `jjfx deliver` after my run.
> I should NOT run it."
> "Hmm, conflicting. … I think I should run it. But if it fails or requires args
> I don't know, I'll note it."
> "However, 'Do not push or open the Pull Request yourself' - running jjfx deliver
> is the sanctioned mechanism, not doing it myself. I'll run it."

Both Runs are recorded as complete successes.

## Evidence

Ticket 03's change exists, is real, and is stranded **only** in the local
Workspace - never pushed, no Pull Request:

```
$ gh api repos/Jarvvski/jjfx-drive-4/branches --jq '.[].name'
adam/ledger--01-date-filtering
adam/ledger--02-input-sources
adam/ledger--04-json-output
main                                   # 03 is absent

$ jj --color=never log -r 'adam/ledger--03-account-filtering' --stat | tail -7
  README.md            | 11 ++++++++++
  ledger/cli.py        | 11 ++++++++--
  ledger/report.py     |  9 ++++++++
  tests/test_cli.py    | 58 +++++++++++++++++++++++++++++++++++++++++++
  tests/test_report.py | 32 ++++++++++++++++++++++++++
  5 files changed, 119 insertions(+), 2 deletions(-)
```

The shared repository's operation log has a push op for every delivered Run and
none for 03:

```
1bd0c932347f | 13:46:10 | push bookmark "adam/ledger--04-json-output" to git remote origin
6409613fcb78 | 13:45:53 | create bookmark "adam/ledger--03-account-filtering" pointing to commit 2763a7
9c2445756166 | 12:10:27 | push bookmark "adam/ledger--02-input-sources" to git remote origin
7cc8a7d49ac4 | 12:09:31 | push bookmark "adam/ledger--01-date-filtering" to git remote origin
```

Every record of the Run says it succeeded:

```json
$ .jj/pools/default/worker-70cae3.json
{"status": "done", "agent": "opencode", "ticket": "ledger--03-account-filtering",
 "branch_name": "ledger--03-account-filtering", "exit_code": 0, "error": null}

$ .jj/pools/default/runs.jsonl
{"conclusion": "succeeded", "check": "passed", ...}
```

`check: passed` is jjfx's own check, run on the agent's tree after the Run
(`src/engine/pool.rs:1830`) - a post-Run step that reads as the pipeline
completing, on a Run whose delivery never happened.

`jjfx status` and `ticket list` show nothing wrong either:

```
ledger--03-account-filtering     resolved     done
```

The Run is `done`, the Ticket is `resolved`, and the change is on nobody's remote.

## Why this matters

This is the drive's central question - *can an agent land a change through jjfx
alone?* - and the answer is **only when the agent happens to misread the prompt
in the permissive direction**. Three Runs delivered and one did not, under one
identical prompt, with no record distinguishing them. A repository cannot tell
which of its finished Runs actually landed.

It is also the second time this shape has appeared: drive 3's
[`07`](../../dogfood-3/issues/07-a-refused-push-is-reported-as-nothing-to-push.md)
was a push that failed and was reported as nothing to do. Here the push is never
attempted and the Run is reported as a success. Both are deliveries that did not
happen and said nothing.

## Acceptance Criteria

- [x] **The prompt names the actor.** Either Dispatch invokes delivery itself once
      the Run ends - which is what the prompt already promises - or the prompt
      tells the agent to run `jjfx deliver <worker>` and says the Run is not
      finished until it does. "jjfx delivers the change" plus "do not push or open
      the Pull Request yourself" is not a decision an agent can make reliably, as
      four Runs of this drive demonstrate. **Done in #198**: the prompt now says
      "Deliver the change yourself, as your last step … Your Run is not finished
      until that command has delivered the change", and the help, the skill and
      the two tests that encoded the old promise were brought along.
- [ ] **A Run whose change was not delivered does not report a clean success.** A
      `conclusion: succeeded` Run with no push and no Pull Request is
      indistinguishable from a delivered one today; the record, or `jjfx status`,
      must say which it is.
- [ ] **The state is already modelled - surface it.** `PoolRemovalWorker` tracks
      delivery per Worker (`is_delivered`, "Undelivered Workers",
      `src/cli.rs:3040-3130`), so a Run's delivery state exists in the system
      without being reported where an operator looks.
- [ ] **Tests pin both halves.** One that a Run ending with a committed change and
      no delivery is reported as undelivered, naming the command that fixes it;
      one that the Dispatch prompt names the actor that delivers.

## Comments

- 2026-10-27: The prompt half is fixed in #198. The remaining half - a Run whose
  change was never delivered must not read as a clean success - is a design
  decision rather than a wording one: it asks whether Dispatch should invoke
  delivery itself once the Run ends, which is what the old prompt promised. Left
  for the owner to rule on rather than picked here.
