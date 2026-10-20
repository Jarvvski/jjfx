---
name: jjfx-dispatch
description: Dispatch work to background agents with jjfx. Use when the user wants to schedule or hand tickets to a Worker pool, run several agents in parallel on separate tasks, or check on and steer agents already running.
---

# jjfx dispatch

jjfx runs background agents as **Workers**. A Worker is one reusable slot backed by its own jj workspace (a colocated Git worktree) running a single coding agent — Claude, Codex, or opencode. A **Ticket** is the unit of work, and **Dispatch** assigns tickets to idle Workers. You drive all of it from the `jjfx` CLI.

`jjfx help <command>` is the command surface: it prints a command's usage, arguments and flags, derived from the same definitions the parser validates against, so the two cannot drift. Take the flag surface from there — `jjfx help dispatch`, `jjfx help pool list`, `jjfx help send` — rather than transcribing it here. This skill carries the judgement a help page cannot: when to dispatch, what a flag *means* for how you drive the pool, and how to read the state back.

That surface is only as good as the binary answering it, so check which one you are driving. `command -v jjfx` names the binary and `jjfx version` the build; a stale `jjfx` earlier on `PATH` answers `jjfx help <command>` with an older flag set (and refuses a command it does not have), which makes the promise above false against it. Pin the binary you mean - run it by its path, or install the version you are driving - and re-check `jjfx version` after any upgrade.

Every step ends on a checkable condition, and every condition is read from a command's `--json` output rather than from its prose — so the same command decides the same way on every run.

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

**Done when** every Ticket you named has an object in the JSON: `dispatched` or `orchestrating` naming what took it, or `waiting` / `failed` naming why. A ticket with no free Worker reports the capacity gap rather than waiting silently, and a `[spend] cap_usd` or `[dispatch] max_concurrent` in the user config can hold one back deliberately - its object reads `"outcome": "waiting"`, and `jjfx status --json` carries the held set in its pool-level `waiting` array.

### 4. Watch

```bash
jjfx status --json                                  # the whole pool, machine-readable
jjfx logs worker-01                                 # follow one Worker's activity
```

The output is an object: `workers` is one entry per Worker across every pool, each naming its `pool`; `waiting` lists the Tickets a capped Dispatch held back, each with the pool that held it (empty when nothing was held). Poll it and read each Worker's `status`; `done` and `failed` are the only terminal values, and `done` means its Run finished with its changes sitting in its workspace. `exit_code` and `error` say why a Worker failed, `stuck` is true when the Run has made no progress inside the watchdog window, and `log_file` is the file `jjfx logs` follows. A Repository with several pools reports a per-pool subtotal under `pools`; filter one lane with `jq '.workers[] | select(.pool == "bugs")'`.

```bash
jjfx status --json | jq -r '.workers[] | select(.status != "done" and .status != "failed") | .worker'
```

**Done when** every dispatched Worker reads `done` or `failed`.

### 5. Steer

```bash
jjfx send worker-01 "also update the changelog"   # follow-up prompt (s)
jjfx review worker-01                             # address its PR review comments (rev)
jjfx mount worker-01                              # open the Worker in kitty (m)
```

`send` obeys the same rule as Dispatch: it refuses a Worker whose Workspace is open, and fails closed when jjfx cannot see the terminal (add `--force`). `review` is exempt - it is how you tell a waiting Worker to address its pull request - so it works even while its Workspace is open.

**Done when** the follow-up Run finishes and the Worker reads `idle` or `done` again.

### 6. Land

```bash
jjfx absorb worker-01    # fold its working-copy edits into the commits they belong to (ab)
jjfx rebase worker-01    # rebase and push its bookmark (rb)
jjfx open-pr worker-01   # open the Worker's Pull Request (pr)
jjfx resolve worker-01   # run jj resolve over a Workspace's conflicted commits
```

Each verb checks its precondition before it mutates anything, so a refusal leaves the Workspace as it found it:

- **`absorb`** needs `trunk()` to resolve - a fetched `main@origin`. Without it every commit is mutable and the fold could rewrite trunk, so it refuses by name rather than running. It resolves the Worker's bookmark by Ticket (a missing or ambiguous bookmark is an error, never a guess) and excludes the commit(s) the Workspace was stacked on, so the fold can only reach the Worker's own commits above the mainline and its stack base. It pushes only when it absorbed something, so running it speculatively is safe.
- **`rebase`** needs the Worker's bookmark to resolve by Ticket; it rebases that bookmark onto `main` and pushes. A push whose name matches no bookmark is a failure, not a silent success.
- **`open-pr`** needs the Worker's bookmark to resolve by Ticket, and the Pool's `gh_repo` to be a real GitHub repository: a missing `origin`, or one that is not GitHub, is refused by name (`origin is not a GitHub remote`) rather than guessed from a path. It then opens the existing Pull Request in the browser (`gh -R <repo> pr view <branch> --web`).

Welding a chain onto trunk, running the repository's own build check (`<repo>/.jjfx/config.toml` `[checks]`) and pushing are the forge's steps, not `open-pr`'s - it only opens the Pull Request they produce. A check that fails stops the push and names the command and its output, so fix the tree and re-run rather than forcing.

**Done when** each Worker's PR exists, or the user has taken the branch over.

### 7. Reclaim

```bash
jjfx pool rm worker-04     # drop one idle Worker (refuses while busy)
jjfx pool reset worker-01  # reset a Worker to idle
jjfx undo worker-01        # undo the last jjfx mutation on a Worker (list when omitted)
jjfx pool destroy          # tear down the pool
```

**Done when** the pool matches the capacity the user wants.

## Rules

- **Dispatch real tickets.** Never invent an id; jjfx reads the Parent's sub-issues and each Worker's ticket back from the source.
- **Read state from `jjfx status --json`.** `workers[]` carries each Worker's `status`, `stuck`, `exit_code`, `error` (the terminal-state and failure signals) and `pool`, and `waiting` carries the Tickets a capped Dispatch held back, each with its pool; do not infer any of it from `jjfx logs` prose.
- **Never merge.** Dispatch produces pull requests; landing them is the user's call.
- **Leave open Workspaces alone.** jjfx will not dispatch to (or `send` into) a Workspace that is open in a terminal. If a headless run reports it cannot see the terminal, re-run with `--force`; otherwise do not force past a Workspace a human is using.
- **Set the runtime deliberately.** The pool profile is repository state that survives a restart — set it with `jjfx pool profile`, not per dispatch, unless the user asks for a one-off `--provider`/`--model` override.
