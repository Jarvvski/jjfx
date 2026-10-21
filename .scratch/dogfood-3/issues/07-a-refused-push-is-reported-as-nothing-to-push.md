# A push jj refused is reported as a harmless skip, and the delivery fails later at the Pull Request

Status: resolved
Type: bug
Blocked by: none

## Problem

`deliver`'s push step can push **nothing** while the pipeline carries on, so the
failure surfaces two steps later as a `gh` error that names none of the real
causes. Reproduced on the third drive's fixture, on a Worker whose chain
contained an empty commit with no description - a commit jj refuses to push:

```
$ jjfx deliver worker-348336
Delivering worker-348336: weld, check, push, Pull Request
  push: nothing to push
  pr: gh pr create (adam/tally--10-version-in-readme): pull request create failed:
      GraphQL: Head sha can't be blank, Base sha can't be blank,
      No commits between main and adam/tally--10-version-in-readme,
      Head ref must be a branch (createPullRequest)
Error: delivery of worker-348336 stopped: pr: gh pr create (...)
```

`push: nothing to push` is printed in the same indented shape as the pipeline's
other *skip* notices, and the delivery continues. The branch never reached the
remote, so `gh pr create` had no head to work with and reported it as a GraphQL
shape error.

jj had said exactly what was wrong, and jjfx dropped it. Run by hand, the forge's
own push revset says:

```
$ jj git push --revisions '::@ ~ trunk() ~ conflicts()'
Warning: Won't push bookmark "adam/tally--10-version-in-readme": commit 5722e547527f has no description
  yvyqxv 5722e5 (empty) (no description set)
Warning: Won't push bookmark main: commit 5722e547527f has no description
  yvyqxv 5722e5 (empty) (no description set)
Nothing changed.
```

The chain, in the Worker's own Workspace:

```
71becd  empty=false  "Show the version flag in the README usage"
5c130b  empty=false  "File a tenth Ticket: show the version flag in the README"
5722e5  empty=true   ""                       <- jj refuses to push this
603e9b9 empty=false  "Accept a whole-file JSON array as input, not only JSONL (#9)"
```

Two distinct problems sit here, and both are worth fixing:

1. **A refused push is not a "nothing to push".** They are different facts: one
   means the change is already on the remote, the other means the remote will not
   take it and the delivery must stop. jjfx reads jj's warning as a no-op and
   reports it as a skip. `dogfood-2`'s finding 05 fixed the *stop* for a genuine
   nothing-to-push; this is the case where "nothing" is not benign.
2. **An empty, undescribed commit survives the weld.** `5722e5` is in the chain
   `deliver` welds and pushes, and it is the reason the push is refused at all. A
   deliverable chain should not carry it: either the weld drops an empty commit
   (it is not a change), or the refusal names it.

Whichever way it is fixed, the operator must not be left reading a GraphQL error
about a blank head sha to discover that a commit in their branch has no
description.

## Acceptance Criteria

- [x] A push that pushed nothing because jj **refused** it stops the delivery and
  names the reason jj gave (the offending commit, and that it has no
  description), rather than reporting a benign skip and continuing.
- [x] A genuine nothing-to-push - the change is already on the remote - still
  behaves as it does today.
- [x] A delivery never reaches the Pull Request step with nothing pushed: the step
  is either skipped with a reason that says the branch is not on the remote, or
  the pipeline has already stopped.
- [x] An empty commit in the welded chain is either dropped by the weld or named in
  the refusal.

## Resolution

Both problems are fixed, in one focused commit on the ticket's bookmark. They
are two facets of the same bug - a chain that jj will not push - and the fix for
one (the refusal report) is the safety net for the other (the weld drop), so
they land together rather than as an artificial split that would leave the
weld's integration test in a commit without the weld it exercises.

**1. A refused push is a failure, not a benign skip.** `jj git push` exits zero
and leaves the remote untouched both when there is genuinely nothing to push and
when it *refuses* a commit it will not push, so the remote-bookmark state alone
cannot tell them apart. `jj::classify_push` now reads jj's own refusal warning
(`Warning: Won't push bookmark <name>: commit <id> has no description`) from the
zero-exit run's stderr and returns `PushOutcome::Failed` with that line (minus
the `Warning: ` prefix) instead of `PushOutcome::Nothing`. The refusal is checked
*before* the snapshot comparison, so a push that refused one commit while moving
another is still a failure, not `Pushed`. The forge already stops the pipeline on
`Failed`, so the delivery now stops before the Pull Request step and the reason
reaches the operator through the existing skip-notice and `stopped:` paths. The
genuine no-op path is untouched: a run with no warning still classifies as
`Nothing`, so a change already on the remote behaves exactly as before.

This is a deliberate, narrow exception to the typed-jj-access rule, exactly like
the existing `jj::fix_tool_failure`: jj exposes no machine-readable field for a
refusal, so its warning line is the only signal, and dropping it is what let the
delivery carry on into `gh`'s `Head sha can't be blank` error.

**2. The weld drops an empty, undescribed commit.** `jj rebase --skip-emptied`
drops a commit that *became* empty, but deliberately leaves a commit that was
already empty (`It will not be abandoned if it was already empty before the
rebase`), so the artifact that caused the refusal survived the weld. The weld now
runs `jj abandon` over `::@- ~ trunk() & mutable() & empty() & description(exact:"")`
before its rebase, dropping those commits (their descendants are rebased onto
their parents) so the chain is pushable. The revset is scoped to `::@-` so the
working copy itself is never abandoned, and to `mutable()` so the immutable root
cannot be named. It is a no-op - jj prints `No revisions to abandon.` and exits
zero - when the chain carries none. A *non-empty* undescribed commit is left
alone (it carries a change, so dropping it would lose work); the refusal from (1)
names it instead.

**Why both, in one ticket.** The second is the root cause of the first's symptom,
and the acceptance criteria accept either "dropped by the weld" or "named in the
refusal". Landing both makes a Worker chain that carries an empty commit deliver
rather than stop, while the refusal remains the safety net for any commit jj
refuses that the weld cannot safely drop. No separate finding is filed: the weld
change is contained, hermetic and tested here.

**Tests.** `jj::tests` gains the pure-classification cases
(`classify_push_reads_a_refused_commit_as_a_failure_not_a_no_op`,
`classify_push_reads_a_refusal_ahead_of_a_moved_remote_bookmark`,
`push_refusal_is_none_when_jj_pushed_every_bookmark`); `tests/deliver.rs` gains
two end-to-end cases against a real `origin` -
`deliver_drops_an_empty_undescribed_commit_before_pushing` (the delivery succeeds
and the welded chain carries no empty commit) and
`deliver_stops_and_names_a_commit_jj_refuses_to_push` (a non-empty undescribed
commit stops the verb, names jj's `no description` reason, reaches no remote ref
and never reaches the Pull Request step). Both were verified under the CI-shaped
environment (`env -u JJ_CONFIG -u XDG_CONFIG_HOME HOME=$(mktemp -d)
JJFX_TEST_SIGNING_OFF=1`), and the fixture writes its own identity and
`auto-track-bookmarks = "glob:never"` so it does not depend on the developer's
config.

**Verified.** `mise run check` is green (changelog validation, fmt, clippy with
warnings as errors, build and the full suite). `docs/configuration.md`'s `[forge]`
section gains a "Welding and pushing" note stating the weld drop and that a
refused push stops the delivery naming jj's reason. Recorded with two `momo fix`
entries.

## Triage (2026-10-21)

Triaged from `needs-triage` to `ready-for-agent`. The evidence is a **live
reproduction during the third drive**: a Worker's chain held an empty commit with
no description, `jjfx deliver` printed `push: nothing to push` and carried on,
and the Pull Request step then failed with `gh`'s `GraphQL: Head sha can't be
blank ... No commits between main and ...`. Removing the empty commit from the
chain made the same delivery succeed, so the cause is confirmed rather than
inferred.

Independently **confirmed in the code**: `PushOutcome::Nothing` is a `skip_step`
that leaves `push_failed` false (`src/forge.rs:853`), so the pipeline carries on
into the Pull Request step; only `PushOutcome::Failed` stops it (`:857`). jj
leaves the push a no-op when it refuses a commit, and that no-op classifies as
`Nothing`.

The acceptance criteria are unchanged. The second problem the ticket names - an
empty commit surviving the weld - belongs to the same fix; if it is easier to
land separately, file it as its own finding rather than dropping it.
