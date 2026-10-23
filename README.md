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
  <a href="https://crates.io/crates/jjfx"><img alt="crates.io" src="https://img.shields.io/crates/v/jjfx"></a>
  <a href="https://github.com/Jarvvski/jjfx/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/Jarvvski/jjfx/actions/workflows/ci.yml/badge.svg"></a>
</p>

<p align="center">
  <img src="assets/demo.gif" alt="jjfx listing workspaces, opening the commit graph, a diff, and the keybindings" width="800">
</p>

---

A jj **workspace** is where an agent works. jjfx gives you one surface for all of
them: a workspace list grouped by what needs you, the commit graph, per-workspace
diffs, and a **Worker pool** that dispatches tickets to background agents - each
in its own jj workspace. No switching between your VCS, your terminal, and your
agents.

## Features

- One list of every workspace, grouped by what needs you: needs you, working,
  ready to forge, idle.
- The commit graph, inline or full screen, laid out like `jj log`, and the diff
  for the workspace you have selected.
- A Worker pool that runs background agents. Each Worker gets its own jj
  workspace; jjfx will not dispatch into one you have open.
- The same binary's command surface, for you and for agents: `dispatch`,
  `status --json`, `deliver`, `review`, and the rest of the table below.
- A bundled skill, so an agent can drive the pool itself.

## Requirements

- **jj 0.46.0** on your `PATH`. jjfx reads jj's on-disk store directly through
  `jj-lib`, so the two are version-coupled - upgrade them together.
- **Rust 1.97.1 or newer**, only to build from source.
  [rustup](https://rustup.rs) is the usual way to get it; this repo's
  [mise](https://mise.jdx.dev) pin is the version the suite is verified against.
  The prebuilt archives do not need a Rust toolchain.

## Install

### Prebuilt binaries

Each [GitHub Release](https://github.com/Jarvvski/jjfx/releases) ships three
archives: macOS Apple silicon, macOS Intel, and Linux x86_64 (musl, so it runs
on glibc too). The archive contains `bin/jjfx`.

```bash
tag=$(curl -fsSL https://api.github.com/repos/Jarvvski/jjfx/releases/latest \
  | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p')
ver=${tag#v}
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)  target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Linux-x86_64)  target=x86_64-unknown-linux-musl ;;
  *) echo "no prebuilt archive for $(uname -s)-$(uname -m); use cargo install" >&2; exit 1 ;;
esac
curl -fsSL -o jjfx.tar.gz \
  "https://github.com/Jarvvski/jjfx/releases/download/${tag}/jjfx-${ver}-${target}.tar.gz"
tar -xzf jjfx.tar.gz
mkdir -p "$HOME/.local/bin"
install bin/jjfx "$HOME/.local/bin/jjfx"
```

Other machines build from source. Checksums sit next to each archive as
`.sha256`.

### Homebrew

```bash
brew install jarvvski/jjfx/jjfx
```

The formula tracks those release archives. The tap is a one-time setup,
described in [Releasing](docs/releasing.md).

### cargo install

```bash
cargo install jjfx
```

That builds and installs the `jjfx` binary into Cargo's bin directory
(`~/.cargo/bin`).

### From source

```bash
git clone https://github.com/Jarvvski/jjfx.git
cd jjfx
mise install        # pins Rust 1.97.1 and jj 0.46.0
mise run install    # release build, installed to ~/.cargo/bin/jjfx
```

Without mise, `cargo install --path .` builds the same binary from the clone.
Build tasks and where the code lives are in
[`CONTRIBUTING.md`](CONTRIBUTING.md).

## Quickstart

### Open the TUI

From anywhere inside a jj repository:

```bash
jjfx tui
```

The workspace list is grouped by what needs you - press `?` for every
keybinding. Bare `jjfx` prints CLI help rather than claiming the terminal, so
scripts and shell completion never unexpectedly enter the TUI.

### Put agents to work

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

### Let your agent drive the pool

Install the bundled skill and a coding agent can dispatch, watch, and steer
Workers itself:

```bash
bunx skills@latest add Jarvvski/jjfx
```

See [`skills/jjfx-dispatch`](skills/jjfx-dispatch/SKILL.md) for what it does.

## Keybindings

The home list. `?` opens the same list over whatever view you are in; the
Worker pool, the diff, and the full-screen graph each have their own.

| Action | Key |
| --- | --- |
| Move down / up | `j` / `k`, or the arrows |
| Go to first / last | `PgUp` / `PgDn` |
| Open workspace | `enter` |
| Open in background | `o` |
| Diff detail | `→` / `l` |
| Toggle the world graph pane | `w` |
| Scroll the world pane | `J` / `K` |
| World graph, full screen | `W` |
| New workspace | `n` |
| Delete workspace | `d` |
| Forge selected / all / default | `f` / `F` / `g` |
| Resolve conflicts | `C` |
| Fetch from remote | `u` |
| Lift onto trunk / lift all | `r` / `R` |
| Tidy this workspace / abandon junk empties | `t` / `T` |
| Worker pool | `p` |
| Fold or expand the idle group | `c` |
| Toggle this help | `?` |
| Quit | `q` / `esc` |

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
  "orphans": [],
  "diverged": []
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
`diverged` names the Workers whose Workspace commit a concurrent `jj` left
divergent - two visible commits sharing one change id, read from jj's
`divergent()` revset - each with its `worker`, `pool`, `change_id` and `copies`,
and is `[]` when nothing diverged. The human table prints the same on the
Worker's row. A commit rewritten out of the visible set cannot be told from one
that never existed, so only divergence is reported, not a gone commit.

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
