# Releasing jjfx

Status: current

How a jjfx release is cut, what the versioning rules are, and how to recover
from a partial release.

## The versioning model

Versioning and the changelog are [momo](https://github.com/Jarvvski/momo)'s job.
The CLI is pinned in `mise.toml`; the configuration is
[`momo.toml`](../momo.toml).

The version of record is the **latest tag** (`v<x.y.z>`), and it is
**release-owned** - an ordinary change never moves it.

Every user-visible change is recorded with momo, which owns the changelog heading
and the section it lands in:

| Command | Heading | Meaning | Pre-1.0 slot | Post-1.0 slot |
| --- | --- | --- | --- | --- |
| `momo break` | `### Breaking` | an incompatible change | minor | major |
| `momo add` | `### Added` | new functionality, backwards compatible | minor | minor |
| `momo fix` | `### Fixed` | a backwards-compatible bug fix | patch | patch |

jjfx is pre-1.0, so the minor position is the breaking slot: Cargo's `^0.52.0`
means `>=0.52.0, <0.53.0`, which makes `0.52.0 -> 0.53.0` a breaking change for
a dependent while `0.52.0 -> 0.52.1` is not. That is the same statement as the
rule that a MAJOR bump needs the owner's explicit approval - 1.0.0 is exactly
where `break` stops being a minor.

The next version is the latest tag plus the **strongest** recorded kind, by
momo's zero-aware rule:

```bash
momo status                  # what is unreleased, and the version it would mint
momo status --version-only   # just the version
```

`mise run check` runs `momo status` as its changelog validation: it fails on a
changelog momo cannot read (a duplicate `## [Unreleased]` heading) and passes
when nothing is unreleased.

`crates/wsg` is frozen at its current version while the migration-era binary is
retired, so a release versions `jjfx` alone. The archive name still reports
wsg's version: `jjfx-0.53.0-wsg-0.19.0-<target>.tar.gz`.

Entries released before the momo adoption keep the old `(break)`/`(add)`/`(fix)`
markers; they are history and carry no meaning.

## Cutting a release

`.github/workflows/release.yml`, dispatched by hand. One input:

| Input | Default | Meaning |
| --- | --- | --- |
| `dry_run` | `true` | stop before building, committing, tagging, publishing, or releasing |

momo derives the bump from the recorded changes; there is no manual override.

**`workflow_dispatch` only runs from the default branch**, so this workflow has
to be on `main` before it can be dispatched at all. There is no rehearsal from a
feature branch.

A dry run first:

```bash
gh workflow run release.yml -f dry_run=true
gh run watch
```

That runs the gate, derives the version, mints it (`momo release` writes the
changelog, the `.momo/version` ledger and the `Cargo.toml` scalar), and verifies
the package with `cargo publish --dry-run --locked --allow-dirty` - then stops
and prints the diff. `--allow-dirty` is required because the mint is deliberately
left uncommitted at that point, so the checkout is dirty by design; it packages
exactly what the commit step would record. Nothing is committed, tagged,
published, or released.

The real thing:

```bash
gh workflow run release.yml -f dry_run=false
```

The `release` environment requires a reviewer, so the job pauses for approval
before it can reach the crates.io token.

### Step order

1. `mise run check` - the full gate, before anything irreversible.
2. Derive the version with `momo status`, refusing when nothing is unreleased, or
   when the tag or the crates.io version already exists.
3. `momo release` mints it; `cargo metadata` then syncs `Cargo.lock`, because
   momo rewrites `Cargo.toml` but never the lock and `cargo publish --locked`
   needs the workspace version entry in sync.
4. `cargo publish --dry-run --locked`.
5. `mise run release aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-musl`.
6. Commit, `momo tag`, push.
7. `cargo publish --locked`.
8. `gh release create` with the archives and `momo notes` as notes.

Two orderings are deliberate. Archives are built **before** the commit, so a
failed cross-compile leaves a dirty checkout rather than a tag. The crate is
published **before** the GitHub Release, so a failed publish leaves a tag but no
release - which is recoverable - rather than a public release pointing at a crate
version that does not exist.

## Recovering from a partial release

| Failed at | State | Recovery |
| --- | --- | --- |
| Archive build | dirty checkout, nothing published | re-dispatch |
| Commit or tag push | dirty checkout | re-dispatch |
| `cargo publish` | tag pushed, crate not published | re-dispatch, or publish by hand |
| `gh release create` | tag and crate published, no release | `gh release create v<version> --notes-file <(momo notes --version <version>) dist/*` |

A crates.io version number can never be reused, which is why `dry_run` defaults
to `true`.

## Prerequisites

- A `release` environment on the repository with the owner as a required
  reviewer.
- `CARGO_REGISTRY_TOKEN` as a secret **on that environment**, holding a
  crates.io token with publish scope for `jjfx`:

  ```bash
  gh secret set CARGO_REGISTRY_TOKEN --env release < <(printenv CI_CRATES_TOKEN)
  ```

- `contents: write` for the workflow, which it declares itself. `main` is
  unprotected, so the bump commit and tag push work with `GITHUB_TOKEN`; the
  workflow touches only `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md` and
  `.momo/version`, so the `workflow`-scope gate on `.github/workflows/` never
  applies.

## Local equivalents

Everything the workflow runs is a command, so it can be exercised locally:

```bash
momo status                  # what is unreleased and the version it would mint
momo status --version-only   # just the version
momo break|add|fix "<text>"  # record a change
momo notes --version 0.52.0  # print a released section
momo release --dry-run       # show what a release would write, and write nothing
momo release                 # mint the version (changelog + ledger + manifest)
momo tag                     # tag the minted version
mise run release             # build the host archive
```

`.github/workflows/release-artifacts.yml` remains the artifact-only path: it
builds the same target matrix and uploads workflow artifacts without releasing
anything.
