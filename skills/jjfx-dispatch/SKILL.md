---
name: jjfx-dispatch
description: Dispatch work to background agents with jjfx. Use when the user wants to schedule or hand tickets to a Worker pool, run several agents in parallel on separate tasks, or check on and steer agents already running.
---

# jjfx dispatch

jjfx runs background agents as **Workers**. A Worker is one reusable slot backed by its own jj workspace (a colocated Git worktree) running a single coding agent — Claude, Codex, or opencode. A **Ticket** is the unit of work, and **Dispatch** assigns tickets to idle Workers. You drive all of it from the `jjfx` CLI.

`jjfx help <command>` is the command surface: it prints a command's usage, arguments and flags, derived from the same definitions the parser validates against, so the two cannot drift. Take the flag surface from there — `jjfx help dispatch`, `jjfx help pool list`, `jjfx help send` — rather than transcribing it here. This skill carries the judgement a help page cannot: when to dispatch, what a flag *means* for how you drive the pool, and how to read the state back.

That surface is only as good as the binary answering it, so check which one you are driving. `command -v jjfx` names the binary and `jjfx version` the build; a stale `jjfx` earlier on `PATH` answers `jjfx help <command>` with an older flag set (and refuses a command it does not have), which makes the promise above false against it. Pin the binary you mean - run it by its path, or install the version you are driving - and re-check `jjfx version` after any upgrade.

Every step ends on a checkable condition, and every condition is read from a command's `--json` output rather than from its prose — so the same command decides the same way on every run.

[`PLAYBOOK.md`](PLAYBOOK.md), beside this file, is the working guide: the loop as it actually runs, the traps a real drive hits and the workaround for each, and what jjfx should be doing for you rather than asking you to remember. Read it before the first `dispatch`.

## Before dispatching

Settle four things first, so a dispatch does not fail on setup:

1. **Repository** — `jjfx root` prints the repo jjfx creates Workers beside. Run from inside it.
2. **Colocation** — `jjfx colocation status`. Colocated Workers carry a working `.git`, which `gh stack` and other Git tools need. If it reports not colocated, run `jjfx colocation enable`.
3. **Runtime** — `jjfx pool profile <runtime> [--provider P --model M]` sets the agent every Worker runs. `claude`, `codex`, and `opencode` are supported; provider and model are optional where the runtime picks them itself. Naming a runtime provisions the empty Pool document when the Pool does not exist yet, so this is safe before `jjfx pool <N>` - either order works, and neither one creates capacity.
4. **Lifecycle hooks** — `jjfx hooks status`. The hooks are how each runtime reports needs-input, quota-blocked and subagent progress; without them a Worker waiting on you still reads `busy`, which is exactly the state you cannot tell from a working one. Claude Code and Codex integrate through hook entries in their own config, but opencode integrates through a plugin jjfx writes to `~/.config/opencode/plugins/jjfx-lifecycle.ts`, so an opencode runtime with no plugin installed reports nothing at all and `jjfx hooks status` names the gap (`opencode: missing: lifecycle plugin`). `jjfx hooks install` writes them into each agent's own config, so run it when the user asks and otherwise report which integrations are missing.

**Done when** `jjfx root` resolves, colocation is enabled, the profile names the runtime the user wants, and you have reported the hook state.

## Steps

### 1. Give the pool capacity

```bash
jjfx pool 4          # set the pool to 4 Workers (creates it if absent)
jjfx status --json   # {"workers": [one object per Worker, naming its pool], "waiting": [held Tickets]}
```

`jjfx pool <N>` is a **safe shrink**: it removes only idle Workers. `jjfx status` is an alias for `jjfx pool list`.

Capacity is how many Runs may be in flight at once, not how many Tickets the pool may ever run: a Worker whose Run finishes is returned to the pool automatically, so a pool of N serves a backlog larger than N without you reclaiming anything between rounds.

**Done when** the pool holds the capacity you want and at least one Worker reads `"status": "idle"`.

### 2. Decide the tickets

Name tickets directly, or let jjfx discover them:

- **Direct** — `jjfx dispatch <TICKET>...` takes the ids you supply.
- **Discovered** — `jjfx dispatch --all [--label <label>]` takes every ready ticket from the configured source.

The source is `[tickets] source` in jjfx's user config, `${XDG_CONFIG_HOME:-~/.config}/jjfx/config.toml`:

- `source = "linear"` (the default) queries Linear for tickets in `tickets.linear.status` (default `Todo`) carrying the `--label` label (default `ready-for-agent`). Discovery runs a short-lived read-only query through the pool runtime, so the runtime must be able to reach Linear.
- `source = "local"` reads the repository's own markdown tickets instead: `tickets.local.root` (default `.scratch`) holding `<feature>/issues/*.md` whose `Status:` is `tickets.local.ready_status` (default `ready-for-agent`). No tracker and no runtime reachability, so this is the deterministic choice when the repository keeps its work in `.scratch/`.

**Done when** you hold the exact ticket ids to dispatch.

### 3. Dispatch

```bash
jjfx dispatch --json AMBA-42 AMBA-43
jjfx dispatch --json --all
```

Dispatch runs **in the background** by default, and `jjfx help dispatch` lists every flag. The one flag whose meaning a help page cannot carry is `--force`, in the open-Workspace rule below.

Read what a Dispatch did from its `--json`: one object per Ticket in the order you gave them, **including the Tickets it did not dispatch**. `outcome` is `dispatched`, `failed`, `waiting` or `orchestrating`; `worker` names the Worker that took it (null when none did); `phase`, `pid` and `detail` carry the failure phase, the background process id, and the prose detail. A `dispatch --all` that finds nothing carries the reason as a `skipped` entry with a null `ticket` and the `label` and `root` it searched, so an empty batch is never a bare `[]` you cannot explain. The objects go to stdout and the human prose to stderr, so `jjfx dispatch --json | jq` reads a batch result without parsing sentences.

**jjfx will not dispatch into a Workspace you have open in a terminal.** A Worker whose tab is open is not idle capacity, so jjfx skips it (implicit selection) or refuses it (an explicit Worker). When jjfx cannot see the terminal at all - a headless or off-kitty run - it cannot tell which Workspaces are open and **fails closed**, printing a message that names `--force`. Add `--force` to dispatch anyway. An agent running inside the terminal jjfx drives (or with `terminal.listen_on` configured) sees the open tabs, so it needs no flag and will correctly leave your open Workspaces alone.

A ticket that has sub-issues is a **Parent**: jjfx orchestrates the sub-issues as a Dispatch Group in dependency order and reports each one's progress, rather than handing the Parent itself to one Worker.

Before launching, jjfx fetches each Ticket's title and description through the configured source and inlines them in the Worker's prompt, so a Worker can implement without tracker tools; if the source cannot be read, Dispatch fails before anything is reserved.

**Done when** every Ticket you named has an object in the JSON: `dispatched` or `orchestrating` naming what took it, or `waiting` / `failed` naming why. A ticket with no free Worker reports the capacity gap rather than waiting silently, and a `[spend] cap_usd` or `[dispatch] max_concurrent` can hold one back deliberately - its object reads `"outcome": "waiting"`, and `jjfx status --json` carries the held set in its pool-level `waiting` array. A finished Worker is returned to the pool on the next Dispatch, so a `waiting` Ticket is one a Run is genuinely running on, not one a `done` Worker is sitting on: run the next round once the Runs finish.

**Pace the backlog from the repository, not the user's config.** To bound how much you fan out, set `[dispatch] max_concurrent` in the **repository** config, `<repo>/.jjfx/config.toml` - never in `${XDG_CONFIG_HOME:-~/.config}/jjfx/config.toml`, which would change every repository the user drives:

```toml
# <repo>/.jjfx/config.toml
[dispatch]
max_concurrent = 2
```

The repository ceiling may only **tighten** the user-global one, never widen it: with both set the smaller binds, and with no user-global cap a repository ceiling is the only limit. `jjfx status --json` reports the resolved limit as `governor.limit`, so the binding ceiling is visible where you read the pool. This is the deliberate way to hold work back - the same `waiting` outcome a full Pool gives, but by your choice rather than the pool's size.

### 4. Watch

```bash
jjfx status --json                                  # the whole pool, machine-readable
jjfx logs worker-01                                 # follow one Worker's activity
```

The output is an object: `workers` is one entry per Worker across every pool, each naming its `pool`; `waiting` lists the Tickets a capped Dispatch held back, each with the pool that held it (empty when nothing was held). Poll it and read each Worker's `status`; `done` and `failed` are the only terminal values, and `done` means its Run finished with its changes sitting in its workspace. `exit_code` and `error` say why a Worker failed, and `stuck` is true when the Run has made no progress inside the watchdog window. A Repository with several pools reports a per-pool subtotal under `pools`; filter one lane with `jq '.workers[] | select(.pool == "bugs")'`.

`jjfx logs <worker>` follows that Worker's current Run until it ends, printing each activity line as it appears; it is a reading aid, not a state source — read state from `jjfx status --json`. Where the activity lives depends on how the Run was delivered, and the banner says which: a Run with a client process writes a structured log, `log_file` names that file, and `logs` follows it (`Following opencode log for <worker>`); an opencode Run delivered through its session — the common case when the daemon is reachable — writes no process log at all, so `logs` reads the session's messages instead (`Following opencode session <id> for <worker>`) and `log_file` stays empty. When the daemon cannot be reached for a session-delivered Run, `logs` says there is no process log and names the session rather than following an empty file.

```bash
jjfx status --json | jq -r '.workers[] | select(.status != "done" and .status != "failed") | .worker'
```

**Done when** every dispatched Worker reads `done` or `failed`.

### 5. Steer

```bash
jjfx send worker-01 "also update the changelog"   # follow-up prompt (s)
jjfx review worker-01                             # bounded loop over its PR review threads (rev)
jjfx mount worker-01                              # open the Worker in kitty (m)
```

`send` obeys the same rule as Dispatch: it refuses a Worker whose Workspace is open, and fails closed when jjfx cannot see the terminal (add `--force`). `review` is exempt - it is how you tell a waiting Worker to address its pull request - so it works even while its Workspace is open. `review` reads the PR's unresolved review threads through `gh`, sends each pass's findings to the Worker, and stops on `[review] max_passes` or on a stalled round; only a pass that finds nothing is success, and any other stop escalates and exits non-zero.

**Done when** the follow-up Run finishes and the Worker reads `idle` or `done` again.

### 6. Land

```bash
jjfx deliver worker-01   # weld, check, push, open one Pull Request (the one forge path)
```

`deliver` is the delivery the Dispatch prompt promises ("jjfx delivers the change"): it welds the Worker's chain onto trunk, runs the repository's own build check (`<repo>/.jjfx/config.toml` `[checks]`), pushes the chain, and opens one Pull Request per change based on the repository's default branch. A failed check, a conflict, or an unreachable forge stops the run before the push, names why, and exits non-zero; a change that already has a Pull Request is left alone, so re-running is safe. **Do not run `gh pr create` yourself** — `deliver` is that step. A repository with no forge (`[forge] pull_requests = false`, or no `origin` remote to open one against) is still delivered: the Pull Request step does not apply, and a clean weld, check and push succeeds.

The narrower verbs each do one thing, and check their precondition before they mutate anything, so a refusal leaves the Workspace as it found it:

```bash
jjfx absorb worker-01    # fold its working-copy edits into the commits they belong to (ab)
jjfx rebase worker-01    # rebase and push its bookmark (rb)
jjfx open-pr worker-01   # open the Worker's existing Pull Request (pr)
jjfx resolve worker-01   # run jj resolve over a Workspace's conflicted commits
```

- **`absorb`** needs `trunk()` to resolve - a fetched `main@origin`. Without it every commit is mutable and the fold could rewrite trunk, so it refuses by name rather than running. It resolves the Worker's bookmark by Ticket (a missing or ambiguous bookmark is an error, never a guess) and excludes the commit(s) the Workspace was stacked on, so the fold can only reach the Worker's own commits above the mainline and its stack base. It pushes only when it absorbed something, so running it speculatively is safe.
- **`rebase`** needs the change's bookmark to resolve; it rebases that bookmark onto `main` and pushes. It runs no build check and opens no Pull Request - it is not the whole delivery.
- **`open-pr`** needs the Pool's `gh_repo` to be a real GitHub repository: a missing `origin`, or one that is not GitHub, is refused by name (`origin is not a GitHub remote`) rather than guessed from a path. It then opens the Pull Request that already exists in the browser (`gh -R <repo> pr view <branch> --web`); it does not create one, so it is not how a change gets delivered.

### When a PR conflicts after a sibling merged

Two Workers delivered from one trunk in a batch; the first merges and moves trunk under the second, whose Pull Request now reads `CONFLICTING`. **`rebase`, `deliver` and `open-pr` name the change by its bookmark as well as by its Worker** - and the bookmark is the durable name to use, because a Worker is a reusable slot and the next Dispatch takes it back the moment its Run ends:

```bash
jjfx rebase adam/tally--04-json-output     # re-weld the delivered change by its bookmark
jjfx deliver adam/tally--04-json-output    # the whole path, when the check should run again
```

Both verbs are the same operation on the same change whichever name you give them. When **no Workspace holds the change** - the Worker that delivered it has been reused, and its Workspace now carries the next Ticket - jjfx creates one for it, named `branch-<bookmark>`, and says so. A rebase that conflicts is left in place and names that Workspace:

```
$ jjfx rebase adam/tally--04-json-output
Created Workspace branch-adam-tally--04-json-output to hold adam/tally--04-json-output; remove it with `jjfx rm branch-adam-tally--04-json-output` when you are done
Error: rebased branch adam/tally--04-json-output onto main, but it has conflicts to resolve: resolve them in Workspace branch-adam-tally--04-json-output (`jjfx resolve branch-adam-tally--04-json-output`), then re-run to push; the rebase is left in place

$ jjfx resolve branch-adam-tally--04-json-output     # the merge tool runs here
$ jjfx rebase adam/tally--04-json-output             # reuses the same Workspace; pushes clean
```

**Done when** each Worker's PR exists and is mergeable, or the user has taken the branch over.

### 7. Reclaim

```bash
jjfx pool rm worker-04     # drop one idle Worker (refuses while busy)
jjfx pool reset worker-01  # reset a Worker to idle
jjfx undo worker-01        # undo the last jjfx mutation on a Worker (list when omitted)
jjfx pool destroy          # tear down the pool
```

You do not have to reclaim a finished Worker to reuse it: one whose Run has finished is returned to `idle` automatically on the next Dispatch, so `pool reset` is for a Worker you want to reclaim *now* - an abandoned Run, or a finished one whose Workspace you want restored before the next round - rather than a step the loop depends on.

**Done when** the pool matches the capacity the user wants.

## Rules

- **Dispatch real tickets.** Never invent an id; jjfx reads the Parent's sub-issues and each Worker's ticket back from the source.
- **Read state from `jjfx status --json`.** `workers[]` carries each Worker's `status`, `stuck`, `exit_code`, `error` (the terminal-state and failure signals) and `pool`, and `waiting` carries the Tickets a capped Dispatch held back, each with its pool; do not infer any of it from `jjfx logs` prose.
- **Never merge.** Dispatch produces pull requests; landing them is the user's call.
- **Leave open Workspaces alone.** jjfx will not dispatch to (or `send` into) a Workspace that is open in a terminal. If a headless run reports it cannot see the terminal, re-run with `--force`; otherwise do not force past a Workspace a human is using.
- **Set the runtime deliberately.** The pool profile is repository state that survives a restart — set it with `jjfx pool profile`, not per dispatch, unless the user asks for a one-off `--provider`/`--model` override.
- **Pace from the repository config, never the user's.** To bound concurrency, set `[dispatch] max_concurrent` in `<repo>/.jjfx/config.toml`. It tightens the user-global ceiling and can never widen it, and `jjfx status --json` reports the resolved limit as `governor.limit`. Do not edit `${XDG_CONFIG_HOME:-~/.config}/jjfx/config.toml` to pace one backlog — that changes every repository the user drives.
