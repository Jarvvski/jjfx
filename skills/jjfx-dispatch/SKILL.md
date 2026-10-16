---
name: jjfx-dispatch
description: Dispatch work to background agents with jjfx. Use when the user wants to schedule or hand tickets to a Worker pool, run several agents in parallel on separate tasks, or check on and steer agents already running.
---

# jjfx dispatch

jjfx runs background agents as **Workers**. A Worker is one reusable slot backed by its own jj workspace (a colocated Git worktree) running a single coding agent — Claude, Codex, or opencode. A **Ticket** is the unit of work, and **Dispatch** assigns tickets to idle Workers. You drive all of it from the `jjfx` CLI (the `wsg` binary exposes the same commands).

## Before dispatching

Settle three things first, so a dispatch does not fail on setup:

1. **Repository** — `jjfx root` prints the repo jjfx creates Workers beside. Run from inside it.
2. **Colocation** — `jjfx colocation status`. Colocated Workers carry a working `.git`, which `gh stack` and other Git tools need. If it reports not colocated, run `jjfx colocation enable`.
3. **Runtime** — `jjfx pool profile <runtime> [--provider P --model M]` sets the agent every Worker runs. `claude`, `codex`, and `opencode` are supported; provider and model are optional where the runtime picks them itself.

## Steps

### 1. Give the pool capacity

```bash
jjfx pool 4          # set the pool to 4 Workers (creates it if absent)
jjfx pool list       # WORKER  NAME  STATUS  TICKET  ELAPSED
```

`jjfx pool <N>` is a **safe shrink**: it removes only idle Workers. `jjfx status` is an alias for `jjfx pool list`.

**Done when** the pool holds the capacity you want and at least one Worker reads `idle`.

### 2. Decide the tickets

Name tickets directly, or let jjfx discover them:

- **Direct** — `jjfx dispatch <TICKET>...` takes tracker ids you supply.
- **Discovered** — `jjfx dispatch --all [--label <label>]` fetches every ticket carrying that label (default `ready-for-agent`) with status `Todo`. Discovery runs a short-lived read-only Linear query through the pool runtime, so the runtime must be able to reach Linear; otherwise name the tickets yourself.

**Done when** you hold the exact ticket ids to dispatch.

### 3. Dispatch

```bash
jjfx dispatch AMBA-42 AMBA-43
jjfx dispatch --all
```

Dispatch runs **in the background** by default; `--fg` runs it in the foreground instead.

| Flag | Effect |
| --- | --- |
| `--all` | Dispatch every discovered ready ticket |
| `--label <label>` | Label used for discovery (default `ready-for-agent`) |
| `--provider P` / `--model M` | Override the pool profile for this dispatch |
| `--budget <usd>` | Cap spend for this dispatch (Claude only) |
| `--no-orchestrate` | Dispatch each ticket directly, skipping Parent orchestration |
| `--force` | Dispatch even to a Worker whose Workspace is open (see below) |

**jjfx will not dispatch into a Workspace you have open in a terminal.** A Worker whose tab is open is not idle capacity, so jjfx skips it (implicit selection) or refuses it (an explicit Worker). When jjfx cannot see the terminal at all - a headless or off-kitty run - it cannot tell which Workspaces are open and **fails closed**, printing a message that names `--force`. Add `--force` to dispatch anyway. An agent running inside the terminal jjfx drives (or with `terminal.listen_on` configured) sees the open tabs, so it needs no flag and will correctly leave your open Workspaces alone.

A ticket that has sub-issues is a **Parent**: jjfx orchestrates the sub-issues as a Dispatch Group in dependency order and reports each one's progress, rather than handing the Parent itself to one Worker.

Before launching, jjfx fetches each Ticket's title and description through the pool runtime and inlines them in the Worker's prompt, so a Worker can implement without tracker tools; if the runtime cannot reach Linear, Dispatch fails before anything is reserved.

**Done when** every ticket prints an outcome naming the Worker that took it. A ticket with no free Worker reports the capacity gap rather than waiting silently.

### 4. Watch

```bash
jjfx status              # one row per Worker
jjfx logs worker-01      # follow one Worker's activity (log)
```

A Worker moves `idle` -> `busy` -> `done` or `failed`. `done` means its Run finished and its changes are sitting in its workspace.

**Done when** every dispatched Worker has reached `done` or `failed`.

### 5. Steer

```bash
jjfx send worker-01 "also update the changelog"   # follow-up prompt (s)
jjfx review worker-01                             # address its PR review comments (rev)
jjfx mount worker-01                              # open the Worker in kitty (m)
```

`send` obeys the same rule as Dispatch: it refuses a Worker whose Workspace is open, and fails closed when jjfx cannot see the terminal (add `--force`). `review` is exempt - it is how you tell a waiting Worker to address its pull request - so it works even while its Workspace is open.

**Done when** the follow-up Run finishes and the Worker is `idle` or `done` again.

### 6. Land

```bash
jjfx rebase worker-01    # rebase and push its bookmark (rb)
jjfx open-pr worker-01   # open its pull request (pr)
```

**Done when** each Worker's PR exists, or the user has taken the branch over.

### 7. Reclaim

```bash
jjfx pool rm worker-04     # drop one idle Worker (refuses while busy)
jjfx pool reset worker-01  # reset a Worker to idle
jjfx pool destroy          # tear down the pool
```

**Done when** the pool matches the capacity the user wants.

## Rules

- **Dispatch real tickets.** Never invent an id; jjfx reads the Parent's sub-issues and each Worker's ticket back from the tracker.
- **Never merge.** Dispatch produces pull requests; landing them is the user's call.
- **Leave open Workspaces alone.** jjfx will not dispatch to (or `send` into) a Workspace that is open in a terminal. If a headless run reports it cannot see the terminal, re-run with `--force`; otherwise do not force past a Workspace a human is using.
- **Set the runtime deliberately.** The pool profile is repository state that survives a restart — set it with `jjfx pool profile`, not per dispatch, unless the user asks for a one-off `--provider`/`--model` override.
