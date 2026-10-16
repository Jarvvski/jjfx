# Contributing to jjfx

Everything here is about working on jjfx itself. If you only want to use it, see
the [Quickstart](README.md#quickstart).

## Prerequisites

- **jj 0.46.0** on your `PATH`. jjfx reads jj's on-disk store directly through
  `jj-lib`, so the two are version-coupled - upgrade them together.
- **Rust 1.97.1**. [mise](https://mise.jdx.dev) pins it for you; a rustup
  toolchain of the same version works too.

`mise install` installs both pins.

## Get the source and install locally

```bash
git clone git@github.com:Jarvvski/jjfx.git
cd jjfx
mise install        # pin Rust and jj
mise run install    # release build, installed to ~/.cargo/bin/jjfx
```

Without mise, `cargo install --path .` builds the same binary from the clone.

## Build and develop

Tooling is driven by [mise](https://mise.jdx.dev):

| Task | What it does |
| --- | --- |
| `mise run run` | `cargo run --release -p jjfx -- tui` |
| `mise run build` | `cargo build --release --workspace` |
| `mise run test` | `cargo test` for every workspace package |
| `mise run fmt` | `cargo fmt --all` |
| `mise run lint` | `cargo clippy --all-targets -- -D warnings` |
| `mise run check` | the pre-land gate: changelog validation + fmt + lint + build + test |
| `mise run release` | bundle jjfx archives + sha256 into `dist/` |

Plain cargo works too:

```bash
cargo run -p jjfx -- tui        # open the TUI
cargo run -p jjfx -- pool list  # run a CLI command
```

`mise run check` is the gate - land only when it is green.

## Where things live

| Area | Path |
| --- | --- |
| CLI, TUI, and the engine | `src/`, with the absorbed former `wsg-core` under `src/engine/` |
| Integration tests and fixtures | `tests/` |
| Configuration reference | [`docs/configuration.md`](docs/configuration.md) |
| Architecture decision records | [`docs/adr/`](docs/adr/) |
| Local planning notes and tickets | `.scratch/` |

## Conventions

Landing rules, Rust conventions, and agent guidance live in
[`AGENTS.md`](AGENTS.md). It is the project memory for humans and agents alike,
and it is the authority on how a change lands.

## Releasing

Releases are cut by dispatching the `Release` workflow. The versioning model,
when to dry-run, and recovery from a partial release are in
[`docs/releasing.md`](docs/releasing.md).
