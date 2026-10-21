<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.svg">
    <source media="(prefers-color-scheme: light)" srcset="assets/logo-light.svg">
    <img src="assets/logo-light.svg" alt="jjfx" width="304">
  </picture>
</p>

<p align="center">
  <strong>One terminal for your <a href="https://jj-vcs.github.io/jj/">Jujutsu</a> workspaces and the coding agents editing them.</strong>
</p>

<p align="center">
  <a href="LICENSE"><img alt="License: GPL-3.0-or-later" src="https://img.shields.io/badge/license-GPL--3.0--or--later-blue"></a>
</p>

---

A jj **workspace** is where an agent works. jjfx gives you one surface for all of
them: a workspace list grouped by what needs you, the commit graph, per-workspace
diffs, and a **Worker pool** that dispatches tickets to background agents - each
in its own jj workspace. No switching between your VCS, your terminal, and your
agents.

## Quickstart

### 1. Prerequisites

- **jj 0.46.0** on your `PATH`. jjfx reads jj's on-disk store directly through
  `jj-lib`, so the two are version-coupled - upgrade them together.
- **Rust 1.97.1 or newer** - [rustup](https://rustup.rs) is the usual way to
  get it.

### 2. Install

```bash
cargo install jjfx
```

That builds and installs the `jjfx` binary into Cargo's bin directory
(`~/.cargo/bin`). To build from a clone instead, see
[`CONTRIBUTING.md`](CONTRIBUTING.md).

### 3. Open the TUI

From anywhere inside a jj repository:

```bash
jjfx tui
```

The workspace list is grouped by what needs you - press `?` for every
keybinding. Bare `jjfx` prints CLI help rather than claiming the terminal, so
scripts and shell completion never unexpectedly enter the TUI.

### 4. Put agents to work

jjfx runs background agents as **Workers**, each backed by its own jj workspace:

```bash
jjfx pool profile claude   # the agent every Worker runs (claude | codex | opencode)
jjfx pool 2                # two Workers, ready for dispatch
jjfx dispatch AMBA-42      # hand a ticket to an idle Worker
jjfx dispatch --text "Fix the login bug"   # or hand in the work directly
jjfx status                # WORKER  NAME  STATUS  TICKET  ELAPSED
```

jjfx will not dispatch into a Workspace you have open in a terminal - an open
Worker is not idle capacity, so it is skipped (or refused, if you name it). When
jjfx cannot see the terminal at all (a headless or off-kitty run) it fails closed
rather than risk a collision; add `--force` to dispatch anyway. `send` follows
the same rule; `review` is exempt.

Run `jjfx colocation enable` first so every Worker gets a working `.git`, which
tools such as `gh stack` need.

### 5. Let your agent drive the pool

Install the bundled skill and a coding agent can dispatch, watch, and steer
Workers itself:

```bash
bunx skills@latest add Jarvvski/jjfx
```

See [`skills/jjfx-dispatch`](skills/jjfx-dispatch/SKILL.md) for what it does.

## Command surface

The same binary carries the non-interactive command surface alongside the TUI:

| Group | Commands |
| --- | --- |
| Workspaces | `add`, `rm`, `list`, `clean`, `root`, `where`, `path`, `refresh`, `sync`, `colocation` |
| Pool | `pool <N>`, `pool create`, `pool destroy`, `pool resize`, `pool list`, `pool rm`, `pool reset`, `pool profile` |
| Dispatch & sessions | `dispatch`, `send`, `review`, `logs`, `mount`, `rebase`, `absorb`, `open-pr`, `deliver` |
| Lifecycle | `hooks install`, `hooks status` |

Run `jjfx help` for the command list, or `jjfx help <command>` for one
command's arguments and flags (`jjfx help pool list`, `jjfx help dispatch`).
`pool list` (and its `status` alias) and `dispatch` take `--json` to print
machine-readable output instead of the prose.

`jjfx status --json` (alias `jjfx pool list --json`) emits a single JSON object
rather than a bare array, so it can carry the pool-level state the human table
prints alongside the per-Worker rows:

```json
{
  "workers": [
    {
      "worker": "worker-01",
      "name": "-",
      "status": "idle",
      "stuck": false,
      "ticket": null,
      "elapsed": "-",
      "error": null,
      "exit_code": null,
      "log_file": null,
      "cost_usd": null,
      "pool": "bugs"
    }
  ],
  "waiting": [{ "ticket": "AMBA-42", "pool": "bugs" }],
  "pools": [
    { "name": "default", "size": 1, "workers": 1, "implicit": true, "spend": 0.0, "cap": null, "window": "week" },
    { "name": "bugs", "size": 2, "workers": 1, "implicit": false, "spend": 1.5, "cap": 5.0, "window": "week" }
  ],
  "orphans": []
}
```

`workers` holds one object per Worker across every pool (the fields the table
shows, plus `stuck`, `exit_code`, `error` and `log_file`), each naming its
`pool`. Filter one lane with the documented query:

```bash
jjfx status --json | jq '.workers[] | select(.pool == "bugs")'
```

`waiting` lists the Tickets a capped Dispatch held back, each with the pool that
held it, and is `[]` when nothing was held. `pools` names every pool the
Repository holds, including an implicit `default` when no `[pools]` declaration
names one, with its size and its recorded spend against its own `cap` and
`window` - a subtotal under the Repository-wide `Spend:` line. `orphans` names
pool directories no declaration claims, which jjfx reports and never adopts.

## Documentation

- [Configuration](docs/configuration.md) - the `config.toml` reference: agent,
  terminal layouts, first-pane command, forge.
- [The review loop](docs/review-loop.md) - how `jjfx review` drives the bounded
  re-review loop over a pull request's unresolved review threads.
- [Workspace hooks](docs/workspace-hooks.md) - repository-local `.jjfx/setup.sh`
  and `.jjfx/teardown.sh`.
- [Agent lifecycle integration](docs/lifecycle-integration.md) - how Claude Code,
  Codex, and OpenCode drive each workspace's agent status.
- [Run records](docs/run-records.md) - the durable per-Run cost and usage record
  jjfx appends at finalization.
- [`docs/adr/`](docs/adr/) - architecture decision records.
- [`skills/jjfx-dispatch/`](skills/jjfx-dispatch/SKILL.md) - the agent skill that
  teaches a coding agent to dispatch work to Workers.
- [Releasing](docs/releasing.md) - how a release is cut: the versioning model,
  the dispatch workflow, and recovery from a partial release.
- [`CHANGELOG.md`](CHANGELOG.md) - what has landed.

## Contributing

Build instructions, the development task list, and where things live are in
[`CONTRIBUTING.md`](CONTRIBUTING.md). Project conventions and agent guidance
live in [`AGENTS.md`](AGENTS.md).

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE).
