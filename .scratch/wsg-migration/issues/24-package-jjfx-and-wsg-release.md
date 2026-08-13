# Package jjfx and wsg release artifacts

Status: ready-for-agent

## Parent

epics/D-interfaces-and-cutover.md

## Problem Statement

The Rust implementation must be distributable as two independently versioned
binaries from the jjfx repository. The current release task packages only
`jjfx`, while the migration requires a candidate artifact containing both
`jjfx` and `wsg` without overwriting the installed Go `wsg` before owner
validation.

## Solution

Extend the existing local release task to build both Cargo packages for each
requested target, emit one archive containing both executables plus a
machine-readable manifest and checksums, and provide a staged installation
path for owner validation. Package metadata comes from each package manifest:
`Cargo.toml` is authoritative for `jjfx` and `crates/wsg/Cargo.toml` is
authoritative for Rust `wsg`.

Publication and promotion remain separate from local packaging. No task in
this ticket may replace `~/.local/bin/wsg`, change the legacy `qwe` alias, or
publish a tag or release.

## Commits

1. Extend release packaging to include `jjfx` and `wsg`, independent versions,
   target metadata, archive contents, checksums, and a release manifest.
2. Add archive smoke tests that execute both host binaries and verify version
   output, executable layout, checksums, and operation outside a Repository.
3. Add a staged candidate installation task that installs both binaries under a
   non-PATH candidate prefix without touching the installed Go binary or `qwe`.
4. Add owner-controlled CI artifact generation for the supported target matrix
   without automatic publication or cutover.

## Decision Document

- The two Cargo package manifests are independent versions of record.
- A release artifact is a bundle, not a version merge; both versions appear in
  the archive name, manifest, checksums, and release notes.
- The archive contains `bin/jjfx`, `bin/wsg`, `release-manifest.json`, and the
  applicable license material.
- Candidate installation is separate from promotion and uses an explicit
  non-PATH prefix.
- Existing Go installation paths and the `qwe` alias remain untouched until
  ticket 25 receives owner approval.
- Release publication, signing, notarization, and tag identity are owner
  decisions and are not performed by this ticket.

## Testing Decisions

Use the exact unpacked candidate archive for smoke tests. Verify both binary
names, independent `--version` output, archive structure, checksums, and
non-Repository startup. Keep `mise run check` independent of cross-compilation
and release publication.

## Acceptance Criteria

- [ ] Release artifacts contain working `jjfx` and `wsg` binaries.
- [ ] Artifact metadata records both package versions and target information.
- [ ] Checksums and archive layout are verified automatically.
- [ ] Candidate installation uses a non-PATH prefix and leaves the installed
      Go `wsg` and `qwe` unchanged.
- [ ] CI can produce inspectable artifacts without publishing or promoting them.
- [ ] `mise run check` is green.

## Out of Scope

- Owner release/tag identity
- Signing, notarization, or public release publication
- Replacing `~/.local/bin/wsg`
- Repointing or removing `qwe`
- Live existing-pool or provider acceptance

## Blocked by

- issues/17-prove-go-rust-conformance.md
