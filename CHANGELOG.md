# Changelog

All notable changes to this project are documented here (newest first). The version of record lives in the project manifest.

## [Unreleased]

### Added

- 2026-07-08 - Walking skeleton: `jjfx` launches in a jj repo and renders a
  keyboard-driven workspace list (default + named). It reconciles the
  authoritative in-memory store from jj plus `.jj/ws-cache`, writes the cache
  through atomically (`name\tpath`) so the bash tools stay consistent, and
  watches `.jj/` so a shell-created workspace appears without a restart. `q`/esc
  quit and a restore-on-panic guard keeps the terminal intact. `--list` dumps the
  reconciled store headlessly.
