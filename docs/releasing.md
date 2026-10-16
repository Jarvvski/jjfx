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

`jjfx version` reports `<version>-dev` for a build from a checkout, because such
a build is not the tagged release: `build.rs` marks every build the release did
not produce. The release task sets `JJFX_RELEASE_VERSION` when it builds the
archives, and a crate unpacked from crates.io carries no VCS metadata, so both
report the clean version. A bug report therefore says which build is in hand.

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

A release versions `jjfx` alone, and the archive name carries that version:
`jjfx-0.53.0-<target>.tar.gz`.

Entries released before the momo adoption keep the old `(break)`/`(add)`/`(fix)`
markers; they are history and carry no meaning.

## Cutting a release

`.github/workflows/release.yml`, dispatched by hand. One input:

| Input | Default | Meaning |
| --- | --- | --- |
| `dry_run` | `false` | stop before building, committing, tagging, publishing, or releasing |

momo derives the bump from the recorded changes; there is no manual override.

**`workflow_dispatch` only runs from the default branch**, so this workflow has
to be on `main` before it can be dispatched at all. There is no rehearsal from a
feature branch.

Dispatch the release:

```bash
gh workflow run release.yml
gh run watch
```

The `release` environment requires a reviewer, so the job pauses for approval
before it can reach the crates.io token.

### When to dry-run instead

`-f dry_run=true` runs the gate, derives the version, mints it (`momo release`
writes the changelog, the `.momo/version` ledger and the `Cargo.toml` scalar),
and verifies the package with `cargo publish --dry-run --locked --allow-dirty` -
then stops and prints the diff. `--allow-dirty` is required because the mint is
deliberately left uncommitted at that point, so the checkout is dirty by design;
it packages exactly what the commit step would record. Nothing is committed,
tagged, published, or released.

That is worth an extra run only when the release *process* changed - a new step
in this workflow, a change to the packaging, or a first release after a long gap.
For an ordinary release of accumulated changes, dispatch the real run: the
environment approval is the gate, and the step order below already fails before
anything irreversible.

```bash
gh workflow run release.yml -f dry_run=true
```

### Step order

1. `mise run check` - the full gate, before anything irreversible.
2. Derive the version with `momo status`, refusing when nothing is unreleased, or
   when the tag or the crates.io version already exists.
3. `momo release` mints it; `cargo metadata` then syncs `Cargo.lock`, because
   momo rewrites `Cargo.toml` but never the lock and `cargo publish --locked`
   needs the workspace version entry in sync.
4. `cargo publish --dry-run --locked`.
5. `mise run release aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-musl`.
6. Commit, rebase onto the latest `main`, push the branch, `momo tag`, push the
   tag.
7. `cargo publish --locked`.
8. `gh release create` with the archives and `momo notes` as notes.

Three orderings are deliberate. Archives are built **before** the commit, so a
failed cross-compile leaves a dirty checkout rather than a tag. The release
commit is **rebased onto the latest `main` and the push retried** (a bounded
number of times), because the ~13 minutes of gate and cross-compile give
concurrent sessions time to move the branch; a rebase conflict on `CHANGELOG.md`
fails the run loudly rather than dropping the other session's entry. The branch
is pushed **before** the tag, so a rejected branch push cannot orphan the tag,
and the crate is published **before** the GitHub Release, so a failed publish
leaves a tag but no release - which is recoverable - rather than a public release
pointing at a crate version that does not exist.

## Recovering from a partial release

| Failed at | State | Recovery |
| --- | --- | --- |
| Archive build | dirty checkout, nothing published | re-dispatch |
| Commit, rebase or branch push | release commit local, nothing published | resolve any reported `CHANGELOG.md` conflict, then re-dispatch |
| Tag push | branch pushed, tag not published | re-dispatch, or `momo tag` and push the tag by hand |
| `cargo publish` | tag pushed, crate not published | re-dispatch, or publish by hand |
| `gh release create` | tag and crate published, no release | `gh release create v<version> --notes-file <(momo notes --version <version>) dist/*` |

A crates.io version number can never be reused, so recovery is always
re-dispatching or running the step by hand - never reusing the version.

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
