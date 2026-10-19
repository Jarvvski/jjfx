# The review loop

`jjfx review <worker>` builds a review prompt from the Worker's pull request and
starts it as a Follow-up Run. By default the prompt inlines the PR's review
state, its failing CI checks, and an instruction to fetch and address human
review comments: the owner is the reviewer for every PR.

Configuring `[review] reviewer` adds an AI reviewer to that loop. The reviewer
produces findings, jjfx normalises them, and they are inlined into the same
prompt - so the Worker's authoring agent addresses them alongside the human
comments and the failing checks, with no second review mechanism to learn.

```toml
[review]
reviewer = "coderabbit"
max_passes = 2
```

With no reviewer configured, nothing changes: no reviewer runs, no loop runs,
and the prompt is exactly the one described above.

## What a configured reviewer does

1. jjfx resolves the Worker's open pull request and describes it as
   `owner/repo`, a base ref, and the Worker's bookmark.
2. The reviewer reviews that pull request and returns findings.
3. jjfx normalises each finding to a severity, a location, a body, and any
   suggested fixes.
4. The findings are inlined into the review prompt and the Follow-up Run starts
   as usual.

A finding's body prefers the reviewer's agent-oriented fix instructions and
falls back to its human-readable comment, so a Worker always has something to
act on. Lines the adapter does not recognise are ignored, and a reviewer that
reports an incomplete review keeps the findings it did emit - a partial review
is still worth acting on.

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
`max_passes` and by a progress requirement. Three things stop it:

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
report success: it prints them and stops for a human to decide. That is the
point of the bound - the failure it exists to prevent is a loop that reaches
zero findings by repetition, spending reviewer compute to learn nothing. The
tool enforces the bound; the prompt does not.

The loop reports how many passes ran, how many findings it saw, and why it
stopped, so the cost of a review is visible rather than implied.

A background Run returns before it has addressed anything, so only a foreground
review can iterate: `jjfx review <worker> --fg` runs the full pass cap, while a
background review is a single pass. Both are bounded by the cap.

## Authentication

The CodeRabbit adapter shells out to `cr review --agent --remote owner/repo
--base <base> --source-branch <branch>`, which reviews a pull request without a
local checkout.

jjfx never puts a credential on that command line. `cr` reads its own stored
auth, so headless environments authenticate once, out of band, with
`cr auth login --api-key` (or the `--agent` OAuth flow) and jjfx simply inherits
the environment. Nothing jjfx runs is visible in `ps` with a secret in it.

## The `opencode-action` reviewer

`reviewer = "opencode-action"` is accepted by the config but not implemented
yet. Selecting it fails at load with a setup error, rather than silently
reviewing nothing.
