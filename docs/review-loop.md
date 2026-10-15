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

## The pass cap

The re-review loop is **review, address, review again**, bounded by
`max_passes`. Two things stop it:

- **The pass cap.** The loop never runs more reviewer passes than `max_passes`,
  whatever the reviewer reports. This is the bound.
- **Convergence.** A pass that produces no findings has nothing to send a
  Worker, so the loop stops early.

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
