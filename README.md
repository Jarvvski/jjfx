# jjfx

A terminal TUI for working with [Jujutsu (jj)](https://jj-vcs.github.io/jj/)
workspaces alongside coding agents - one surface where you drive your VCS and
the agents editing it, instead of switching between them.

**Status:** `jjfx` provides the CLI and an explicit interactive TUI entrypoint.
The compatibility `wsg` target remains available for migration conformance.

## Build & run

Tooling is driven by [mise](https://mise.jdx.dev):

```bash
mise install     # pin the Rust toolchain
mise run run     # cargo run -p jjfx -- tui
mise run build   # cargo build
mise run test    # cargo test
mise run fmt     # cargo fmt --all
mise run lint    # cargo clippy --all-targets -- -D warnings
mise run check   # fmt + lint + build + test (the pre-land gate)
```

Plain cargo works too:

```bash
cargo run -p jjfx -- tui       # open the TUI
cargo run -p jjfx -- pool list # run a CLI command
```

## Quick start

Open the TUI from anywhere inside a jj repository:

```bash
jjfx tui
```

The workspace list is grouped by what needs you; press `?` for the full
keybinding list. Bare `jjfx` prints CLI help rather than claiming the terminal,
so scripts and shell completion never unexpectedly enter the TUI.

The same binary carries the non-interactive command surface:

| Group | Commands |
| --- | --- |
| Workspaces | `add`, `rm`, `list`, `clean`, `root`, `where`, `path`, `refresh` |
| Pool | `pool <N>`, `pool list`, `pool rm`, `pool reset`, `pool profile`, `pool destroy` |
| Dispatch & sessions | `dispatch`, `send`, `review`, `logs`, `mount`, `rebase`, `open-pr` |
| Lifecycle | `hooks install`, `hooks status` |

Run `jjfx help` for usage.

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
- [`docs/wsg-compatibility-contract.md`](docs/wsg-compatibility-contract.md) -
  the compatibility contract for the migration-era `wsg` target.
- [`CHANGELOG.md`](CHANGELOG.md) - what has landed.

## Contributing

Project conventions and agent guidance live in [`CLAUDE.md`](CLAUDE.md).

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE).
