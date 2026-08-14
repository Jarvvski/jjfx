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

## Pi Worker actions

The shared Worker action layer supports Pi 0.84.x for fresh and resumed
Follow-ups and interactive kitty mounts. The `pi` executable must be on `PATH`,
and the host must select an authenticated provider and model explicitly through
`WorkerActions::with_model(AgentModel::new(model).with_provider(provider))`.
Broad Pi selection in the jjfx CLI and TUI is tracked separately from these
Worker action contracts.

Pi Worker runs and mounts use the repository-owned `.jj/pool/pi-sessions`
directory, ignore inherited extensions, skills, prompt templates, themes,
context files, and project trust, and allow only the built-in
`read,bash,edit,write,grep,find,ls` tools. This tool policy is not filesystem
confinement: Pi runs with the host user's permissions, so use an operating-system
sandbox when the Workspace needs a stronger boundary.

Pi core does not provide aggregate budget limits, per-tool approval dialogs,
or native Linear ticket discovery. jjfx reports those capabilities as
unsupported rather than falling back to Claude or Codex.

## Contributing

Project conventions and agent guidance live in [`CLAUDE.md`](CLAUDE.md); see
[`CHANGELOG.md`](CHANGELOG.md) for what's landed.

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE).
