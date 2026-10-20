# The review loop

`jjfx review <worker>` builds a review prompt from the Worker's pull request and
starts it as a Follow-up Run. The prompt inlines the PR's review state, its
failing CI checks, and an instruction to fetch and address human review
comments: the owner is the reviewer for every PR. Nothing else runs - jjfx does
not ingest an external review bot's findings, and the prompt has no AI-reviewer
step to act on.

## Where a fix belongs

Review feedback often belongs in a **base commit**, not in a new commit stacked
on top. The prompt sends that case through `jj`'s own absorb primitive rather
than a hand-split:

1. Find the commit that introduced the code with `jj log`.
2. Start an edit on top of it with `jj new <commit>`.
3. Make the change.
4. Run `jj absorb`. jj moves each hunk into the closest mutable ancestor where
   those lines last changed, and abandons the edit commit once every hunk has
   landed (give the edit a description if you want to keep it).

`jjfx absorb <worker>` is the same absorb step from outside the workspace: it
runs `jj absorb` in the Worker's Workspace and pushes the rewritten bookmark, so
the PR picks the folded history up. A run that absorbs nothing does not push, so
it is safe to run speculatively. `jj absorb` reports what it did on stderr, and
jjfx reads that summary: a run that leaves hunks behind - because no ancestor
unambiguously owns those lines - says so rather than pretending the working copy
is clean.

Two guards keep that step off the mainline, because a Worker's `@` is its
deliverable and `jj absorb` folds into the closest *mutable* ancestor:

- The destination is pinned to the Worker's own commits above the mainline,
  never jj's default `mutable()`. A Worker whose change is already committed
  therefore has nowhere to fold into, so the command is the no-op it should be
  rather than a rewrite of the branch the Worker sits on.
- The mainline is read before and after the fold. If it moved anyway, the absorb
  is rolled back with `jj undo` and reported as a failure - never as success.

`jjfx absorb` also refuses outright when the repository has no resolvable trunk
(no fetched `main@origin`), because there is nothing for absorb to stop at: in
that repository every commit, `main` included, is mutable. The refusal says so
and names the remedy (`jj git fetch`).

Absorb can leave the commits above it conflicted, because it rewrites a commit
they sit on. The prompt says so: resolve every conflict absorb reports, or
`jj undo` and address that feedback on top of `@` instead. Feedback that does not
belong in an existing commit - a new file, a new test - stays an ordinary edit on
`@`.

## The bound

The re-review loop is **review, address, review again**, bounded by `max_passes`
and by a progress requirement. Three things stop it:

- **The pass cap.** The loop never runs more reviewer passes than `max_passes`,
  whatever the reviewer reports. This is the hard ceiling.
- **Progress.** A round must both surface a finding the previous pass had not
  and retire one it had. A reviewer that repeats itself finds nothing new, and a
  Worker that fixes nothing retires nothing: either way the round is a stall, and
  another pass would only buy the same findings again.
- **Convergence.** A pass that produces no findings has nothing to send a
  Worker, so the loop stops early.

Only convergence means the review passed. A loop that stops on the cap or on a
stalled round has **not** resolved its last round's findings, so it does not
report success: it escalates them for a human to decide. That is the point of
the bound - the failure it exists to prevent is a loop that reaches zero
findings by repetition, spending reviewer compute to learn nothing. The tool
enforces the bound; the prompt does not.

The bound lives in `ReviewLoop` (`src/engine/reviewer.rs`) and is generic over
whatever supplies each pass's findings. jjfx runs no reviewer of its own, so the
loop stands on its own until the harness drives it.

## Replaying a decided pass

The loop is driven through the pool's decision log
(`.jj/pools/<name>/decisions.json`). Each pass is keyed by the step identity
`review:<ticket>:<dimension>:<pass>`, so a pass already recorded is replayed
rather than reaching the reviewer again, and its findings are not re-addressed -
they were addressed when the decision was first made.

Replay does not change the bound. A replayed pass carries the decision it
recorded, so the cap and the progress rule see exactly the rounds the first drive
saw and the loop stops in the same place; a second drive of the same loop
therefore reaches no model and buys no pass past where the first stopped. An
operator sees which passes replayed in `jjfx status --json`'s queue, where the
step's `detail` names the replay. See
[the durable work queue](work-queue.md#replaying-a-decision).
