# No CLI verb performs the delivery the dispatch prompt promises

Status: ready-for-agent
Type: bug
Blocked by: none

## Problem

The dispatch prompt tells the agent that jjfx will deliver the change:

> jjfx delivers the change to {repo}: it rebases your bookmark onto trunk, runs
> the repository's check, pushes it, and opens the Pull Request. **Do not push or
> open the Pull Request yourself.**

(`src/engine/dispatch_prompt.rs:157`, `:205`)

No CLI verb does that. The forge that welds, checks, pushes and opens the PR is
reachable only from the TUI (`src/app.rs`, the `f` key); the CLI's landing verbs
are:

    $ jjfx help rebase     # "Rebase and push a Worker's bookmark"  - pushes, no PR
    $ jjfx help open-pr    # "Open a Worker's Pull Request in the browser"
                           #   - `gh pr view <branch> --web`, needs the PR to exist

So an agent following the prompt and the skill ends up with either an unpushed
branch (nothing ran the forge) or, after `rebase`, a pushed branch with **no Pull
Request**, and `open-pr` cannot create one. Observed on a finished Worker whose
run recorded `check: passed`:

    $ gh pr list -R Jarvvski/jjfx-drive-2          # empty after a clean run
    $ jjfx open-pr worker-ccb8a5
    Opened Pull Request for adam/sift--08-sparkline   # only because the PR was
                                                       # created by hand first

The `open-pr` help summary was corrected in #144 to "Open a Worker's Pull Request
in the browser", but the delivery contract in the prompt still names jjfx as the
thing that opens it, and no command bridges the two.

## Acceptance Criteria

- [ ] A CLI verb performs the delivery the prompt describes - weld onto trunk,
      run the repo check, push, open one PR - so an agent driving from the shell
      can land without running `gh pr create` itself.
- [ ] The prompt, `jjfx help`, and the skill agree on which command that is, and
      on what `rebase` / `open-pr` do on their own.
- [ ] A repository with no forge stays a supported, non-degraded path.

## References

- `src/engine/dispatch_prompt.rs:157`, `:205` (the delivery contract)
- `src/app.rs` (the forge, TUI-only), `src/forge.rs`
- `src/engine/worker_actions.rs:466` (`rebase`), `:493` (`open_pull_request`)
- `.scratch/harness/issues/10-cut-the-forge-to-one-path.md`
- `.scratch/dogfood/issues/13-open-pr-docs-contradict-the-tool.md`

## Triage (2026-10-20)

Triaged from `needs-triage` to `ready-for-agent`. The evidence is a **live
reproduction during the second drive**, not a code reading: the drive hit this
while working a real backlog through jjfx, and the report
(`.scratch/dogfood-2/report.md`) records what happened around it.

Independently **confirmed**: no CLI verb opens a Pull Request; the only
push-shaped verb is `rebase`, which pushes a Worker's branch and nothing more.

The acceptance criteria are unchanged - they were already specific and
testable, which is why this needed triage rather than a rewrite.
