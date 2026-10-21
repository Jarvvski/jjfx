# The review loop

`jjfx review <worker>` drives the tool-enforced re-review loop over the Worker's
Pull Request. Each pass reads the Pull Request's **unresolved review threads**
through `gh`, maps each open thread onto a finding, and - when it found
something - sends the pass's findings to the Worker to address. A pass that finds
nothing has nothing to send, so the loop converges and stops. The bound is the
tool's, not a prompt's: it is `ReviewLoop` in `src/engine/reviewer.rs`, and
`jjfx review` is its caller.

The findings source is named, not inferred. jjfx does not ingest an external
review bot's findings (ADR 0016 cut that), and it does not dispatch a reviewer
agent of its own: the reviewer is the Pull Request's own review threads, read
through `gh`. A repository with no forge, or a Worker with no Pull Request, is
refused by name rather than silently reporting convergence.

Only an **open** Pull Request is reviewable, so a Worker whose Pull Request has
already merged or been closed is refused by name too - naming the state `gh`
reports - because there is nothing left to review. That is the answer a landed
Worker gets before it has been reused: its Pull Request is right there, and the
refusal says so rather than claiming no Pull Request exists. Only a branch with
no Pull Request in any state reads as "no pull request found".

## What the loop reads

Each pass asks `gh` for the Pull Request's review threads and keeps the
**unresolved** ones. Every open thread becomes one finding carrying the thread's
severity, file and lines, title, comments and any suggested change.

Three decisions are deliberate, because a thread is not a finding an agent
produced:

- **Addressing a thread does not resolve it.** Resolution is the reviewer's
  action - a human or a bot resolves the conversation - not something jjfx or
  the Worker does. So a pass sends its findings, the Worker pushes its fix, and
  the threads stay open until the reviewer resolves them. If the next pass sees
  the same open threads and retires none, the progress rule reads a stall and the
  loop stops on **no progress** - reported as such, never as a review that
  passed. That is the honest reading: the Worker's self-report is not
  verification (ADR 0016).
- **A plain issue comment is not a finding.** Only a review thread carries an
  `isResolved` state; a comment has none, so it can never be retired and would
  make the loop unable to converge. The address prompt still points the Worker at
  the Pull Request's comments, but the bound counts threads alone.
- **A Pull Request with no review at all converges on the first pass.** No
  threads is no findings, which is exactly the empty pass the loop stops on.

A read that failed is **not** "no findings". A `gh` call that cannot read the
threads produces an *incomplete* pass, and an incomplete pass stops the loop as
**an unreadable review** and escalates - it never reads as a clean one.

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

The re-review loop is **review, address, review again**, bounded by
`[review] max_passes` and by a progress requirement. Three things stop it:

- **The pass cap.** The loop never runs more reviewer passes than `max_passes`,
  whatever the reviewer reports. This is the hard ceiling.
- **Progress.** A round must both surface a finding the previous pass had not
  and retire one it had. A reviewer that repeats itself finds nothing new, and a
  Worker that fixes nothing retires nothing: either way the round is a stall, and
  another pass would only buy the same findings again.
- **Convergence.** A pass that produces no findings has nothing to send a
  Worker, so the loop stops early.

Only convergence means the review passed. A loop that stops on the cap, on a
stalled round, or on a review it could not read has **not** resolved its last
round's findings, so it does not report success: it escalates them for a human to
decide, and `jjfx review` exits non-zero. That is the point of the bound - the
failure it exists to prevent is a loop that reaches zero findings by repetition,
spending reviewer compute to learn nothing. The tool enforces the bound; the
prompt does not.

The bound lives in `ReviewLoop` (`src/engine/reviewer.rs`) and is generic over
whatever supplies each pass's findings. `jjfx review` supplies the Pull Request's
unresolved review threads; the harness seam (`RecordedReviewLoop` in
`src/engine/review_run.rs`) drives the same bound over any other reviewer.

## Replaying a decided pass

Each pass is keyed by the step identity `review:<ticket>:threads:<pass>`, and
replay is scoped to the invocation that made the decision. Within one run a pass
already decided is not decided again, and its findings are not re-addressed -
they were addressed when the decision was first made. A pass decided by an
*earlier* `jjfx review` is not replayed: that decision was made on the Pull
Request's review threads as they were then, and the reason to ask for another
review is that they may have changed since. A recorded "no findings" must never
outlive the state it saw, or the loop would report convergence on a Pull Request
whose threads have since been opened.

Replay does not change the bound. A replayed pass carries the decision it
recorded, so the cap and the progress rule see exactly the rounds the first drive
saw and the loop stops in the same place. The harness seam (`RecordedReviewLoop`)
still replays across invocations over the pool's durable decision log, because its
reviewer is a model and its decisions do not depend on state the outside world can
move; see [the durable work queue](work-queue.md#replaying-a-decision).
