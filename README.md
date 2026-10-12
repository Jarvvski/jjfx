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
- **Rust 1.97.1**. [mise](https://mise.jdx.dev) pins it for you; a rustup
  toolchain of the same version works too.

### 2. Install

```bash
git clone git@github.com:Jarvvski/jjfx.git
cd jjfx
mise install        # pin Rust and jj
mise run install    # release build, installed to ~/.cargo/bin/jjfx
```

Without mise, `cargo install --path .` builds and installs the same binary.

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
jjfx pool profile claude   # the agent every Worker runs (claude | codex | pi | opencode)
jjfx pool 2                # two Workers, ready for dispatch
jjfx dispatch AMBA-42      # hand a ticket to an idle Worker
jjfx status                # WORKER  NAME  STATUS  TICKET  ELAPSED
```

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
| Workspaces | `add`, `rm`, `list`, `clean`, `root`, `where`, `path`, `refresh`, `colocation` |
| Pool | `pool <N>`, `pool list`, `pool rm`, `pool reset`, `pool profile`, `pool destroy` |
| Dispatch & sessions | `dispatch`, `send`, `review`, `logs`, `mount`, `rebase`, `open-pr` |
| Lifecycle | `hooks install`, `hooks status` |

Run `jjfx help` for usage.

## Build & develop

Tooling is driven by [mise](https://mise.jdx.dev):

```bash
mise install            # pin the Rust toolchain and jj
mise run run            # cargo run --release -p jjfx -- tui
mise run build          # cargo build --release --workspace
mise run test           # build the workspace, then cargo test
mise run fmt            # cargo fmt --all
mise run lint           # cargo clippy --all-targets -- -D warnings
mise run check          # fmt + lint + build + test (the pre-land gate)
mise run conformance    # Go/Rust wsg conformance (needs the Go oracle; see docs/wsg-conformance.md)
mise run release            # bundle jjfx+wsg archives + sha256 into dist/
mise run install-candidate  # stage both binaries under a non-PATH prefix
```

Plain cargo works too:

```bash
cargo run -p jjfx -- tui        # open the TUI
cargo run -p jjfx -- pool list  # run a CLI command
```

## Documentation

- [Configuration](docs/configuration.md) - the `config.toml` reference: agent,
  terminal layouts, first-pane command, forge.
- [Workspace hooks](docs/workspace-hooks.md) - repository-local `.jjfx/setup.sh`
  and `.jjfx/teardown.sh`.
- [Agent lifecycle integration](docs/lifecycle-integration.md) - how Claude Code,
  Codex, Pi, and OpenCode drive each workspace's agent status.
- [Pi Worker actions and ticket discovery](docs/pi-workers.md) - Pi Direct
  Dispatch, the pinned adapter, and the read-only Linear helper protocol.
- [`docs/adr/`](docs/adr/) - architecture decision records.
- [`skills/jjfx-dispatch/`](skills/jjfx-dispatch/SKILL.md) - the agent skill that
  teaches a coding agent to dispatch work to Workers.
- [`docs/wsg-compatibility-contract.md`](docs/wsg-compatibility-contract.md) -
  the compatibility contract for the migration-era `wsg` target.
- [wsg conformance](docs/wsg-conformance.md) - the migration-era Go/Rust
  conformance suite, its invocation, and the accepted differences.
- [`CHANGELOG.md`](CHANGELOG.md) - what has landed.

## Status

`jjfx` provides the CLI and an explicit interactive TUI entrypoint. The
compatibility `wsg` target remains available for migration conformance.

## Contributing

Project conventions and agent guidance live in [`CLAUDE.md`](CLAUDE.md).

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE).
