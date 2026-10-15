# Adopt `gh stack` for stack state instead of hand-rolling base-chaining

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

The forge builds PR stacks itself: `base_for` computes each PR's base from the
stack relationship and `stack_section` writes a `## Stack` body section
(`src/forge/pull_requests.rs`), and `src/prs.rs` projects the stack. This is
bespoke bookkeeping that must be kept correct across rebases, and it earns no
integration with GitHub's native stack/merge-queue features.

`gh stack` is the only stack tool explicitly built to accommodate jj-style
tooling (`gh stack link`), and it offers cascade-rebase (`gh stack sync`) and a
merge-queue-aware `gh stack merge`.

## Solution

**Spike first** (this is a `sweep` claim, not yet verified): reproduce jjfx's
current base-chained stack with `gh stack link` on a real repo and confirm it
produces the same PR bases and bodies without loss. If it does, delegate stack
state to `gh stack` and delete jjfx's own assembly.

If the spike shows a behavioural loss, record it and close as `wontfix` — the
hand-rolled path stays.

## Commits

1. Spike: build the same stack both ways and compare bases, bodies, and rebase
   behaviour; record findings in `## Comments`.
2. If viable: delegate base-chaining and stack bookkeeping to
   `gh stack link`/`sync`/`merge`, keeping jjfx's push/weld steps.
3. Tests; docs; changelog (or a `wontfix` note with the spike findings).

## Acceptance Criteria

- [ ] The spike result is recorded with evidence (real repo, real `gh stack`).
- [ ] If adopted: jjfx's stack assembly is removed and the resulting PR stack is
      identical in bases and reviewable bodies; `gh stack sync` cascades
      correctly after a trunk move.
- [ ] If not adopted: the reason is recorded and the ticket closes `wontfix`.
- [ ] `mise run check` is green.

## Out of Scope

- Merge queue and auto-landing — ticket 10.
- Graphite/Sapling/Aviator/spr — git-branch-centric and jj-blind.

## References

- `gh stack` CLI: https://docs.github.com/en/pull-requests/reference/stacked-prs-cli-commands
- GitHub native stacked PRs: https://docs.github.com/en/pull-requests/reference/use-other-tools-with-stacked-pull-requests
- `src/forge/pull_requests.rs`, `src/prs.rs`

## Comments

- 2026-10-15: Filed from the strategy review. `gh stack link`'s jj support was
  read from GitHub docs; whether it *fully* replaces jjfx's assembly is the spike.
