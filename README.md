# jjfx

A terminal TUI for working with [Jujutsu (jj)](https://jj-vcs.github.io/jj/)
workspaces alongside coding agents - one surface where you drive your VCS and
the agents editing it, instead of switching between them.

**Status:** `jjfx` provides the CLI and an explicit interactive TUI entrypoint.
The compatibility `wsg` target remains available for migration conformance.

## Build & run

Tooling is driven by [mise](https://mise.jdx.dev):

```
mise install     # pin the Rust toolchain
mise run run     # cargo run -p jjfx -- tui
mise run build   # cargo build
mise run test    # cargo test
mise run fmt     # cargo fmt --all
mise run lint    # cargo clippy --all-targets -- -D warnings
mise run check   # fmt + lint + build + test (the pre-land gate)
```

Plain cargo works too:

```
cargo run -p jjfx -- tui       # open the TUI
cargo run -p jjfx -- pool list # run a CLI command
```

## Contributing

Project conventions and agent guidance live in [`CLAUDE.md`](CLAUDE.md); see
[`CHANGELOG.md`](CHANGELOG.md) for what's landed.

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE).
