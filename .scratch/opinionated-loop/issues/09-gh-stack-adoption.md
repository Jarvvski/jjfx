# Adopt `gh stack` for stack state instead of hand-rolling base-chaining

Status: wontfix
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
- 2026-10-15: **Spike: not adopted — closes `wontfix`.** Run on a real throwaway repo (`Jarvvski/jjfx-stack-spike`, private, now archived) with the real tool (`github/gh-stack` v0.1.0, `gh` 2.102.0). Native stacked PRs are enabled there (`GET /repos/Jarvvski/jjfx-stack-spike/stacks` → `[]`, not 404), and jj bookmarks do export as git branches in a colocated workspace, so `gh stack` can see jj's branches — the jj-accommodation claim holds. `gh stack link t1 t2 t3` pushed the three branches, created three PRs, set the bases exactly as `base_for` would (t1→main, t2→t1, t3→t2), and created native stack #4. Bases match; nothing else does.
- The rest is a behavioural loss, so the hand-rolled path stays:
- **Titles and bodies are lost.** `link` titled the PRs from the *branch names* (`t1`, `t2`, `t3`) and wrote a fixed gh-stack footer as the body, discarding the jj commit description that `split_title_body` turns into the PR title and body. jjfx's `## Stack` section is not written at all (the native stack UI replaces it), so the bodies are not "identical". A re-run of `link` preserves an existing body only because it never manages bodies — it cannot produce jjfx's bodies either.
- **No local tracking, so no cascade rebase.** `link` "does not create or modify any local tracking state": afterwards `gh stack view` and `gh stack sync` both exit 2 ("current branch is not part of a stack"). The merge-queue-aware `sync`/`merge` value the ticket hoped for is unavailable through `link`; jjfx's weld would still be the only cascade path, so nothing is removed there.
- **It pushes the branches itself** (`git push`, "Pushing 3 branches to origin"), overlapping the `jj git push` push step the ticket wanted to keep.
- **Portability and dependency cost.** The Stacks REST API is gated per repository: without the feature it 404s and `gh stack` warns "Stacked PRs are not enabled for this repository" (`cmd/utils.go`, `internal/github/github.go`; `docs/.../reference/rest-api.md`). `gh stack` is also a separate extension, not core `gh`, so adopting it would make a general tool depend on a gated GitHub feature plus an extra extension install.
- Net: `link` can replace `base_for` but loses the description-derived title/body and the `## Stack` section, gains no cascade rebase, and adds a gated-feature + extension dependency. The adoption criterion ("stack assembly removed, PR stack identical in bases **and** reviewable bodies") is not met, so no code change is made.
- Spike hygiene: the throwaway repo's PRs were closed and its branches deleted, and the repo was archived. It could not be deleted — the token lacks the `delete_repo` scope, and refreshing scopes was out of scope — so the owner may delete `Jarvvski/jjfx-stack-spike` at leisure.
