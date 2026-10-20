# No CLI verb performs the delivery the dispatch prompt promises

Status: resolved
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

- [x] A CLI verb performs the delivery the prompt describes - weld onto trunk,
      run the repo check, push, open one PR - so an agent driving from the shell
      can land without running `gh pr create` itself.
- [x] The prompt, `jjfx help`, and the skill agree on which command that is, and
      on what `rebase` / `open-pr` do on their own.
- [x] A repository with no forge stays a supported, non-degraded path.

## Resolution

**The verb: `jjfx deliver <worker>`.** It is the delivery the Dispatch prompt
promises, as a shell verb: weld the Worker's chain onto trunk, run the
repository's own check (`[checks]`), push the chain, and open one Pull Request
per change based on the repository's default branch. It resolves the Worker's
Workspace exactly as `rebase`/`absorb`/`open-pr` do, builds the forge's
`Settings` from the config (`[forge]` plus the repository's `[checks]`), and
reports each step's skip notice on stderr, exiting non-zero with the reason when
the run is not clean. A missing Workspace is refused by name before anything
runs.

**Why that shape - the forge's one path, exposed, not a second one.** The
delivery is not a new pipeline: `Forge` gained a synchronous `run` that drives
the *same* `run_pipeline` the TUI's `f` key drives, and `deliver` folds its
streamed `Update`s into a `DeliveryReport`. `jjfx deliver` is therefore a second
*frontend* onto the one blessed forge path (ADR 0016, "One forge path"), not a
second path - the CLI and the `f` key cannot drift about what delivering means.
The forge was chosen over extending `rebase` because `rebase` welds the bookmark
onto `main` and pushes the bookmark only, while the forge welds the whole
mutable chain, runs the check against the just-welded tree, and opens the PR;
making `rebase` do all that would either duplicate the forge or change what an
existing verb means.

**The prompt, help and skill agree.** `dispatch_prompt.rs` now names the command
in both the system prompt and the worker prompt ("jjfx delivers the change to
{repo} with `jjfx deliver <worker>`"), the `deliver` help page documents the
whole path, and `SKILL.md` §6 and `PLAYBOOK.md` §4 lead with `jjfx deliver`.
`rebase` and `open-pr` each gained a note stating their narrower scope - `rebase`
runs no check and opens no Pull Request; `open-pr` opens one that already exists
- and both point at `deliver`. `tests/deliver_contract.rs` holds the three
surfaces together: the prompt names the verb, `jjfx help deliver` describes
weld/check/push/Pull Request, the skill and playbook name it, and the `rebase`
and `open-pr` pages state their own scope.

**No forge stays supported.** With `[forge] pull_requests = false`, or no
`origin` remote to open one against, the Pull Request step is `NotApplicable`
and a clean weld, check and push still succeeds - the forge's own no-forge rule,
now reachable through the verb rather than only the TUI. `tests/deliver.rs`
proves the push lands on a local `origin` and the verb exits 0.

**The push a clean config refused (fixed in review).** CI caught what this
machine's config masked: `jj git push --revisions <revset>` **refuses to create**
a remote bookmark for a local bookmark that is not tracked
(`Refusing to create new remote bookmark <name>@<origin>; Run jj bookmark track
...`), so a fresh Worker's bookmark was left behind, the push read as
`PushOutcome::Nothing`, and `deliver` stopped with `push: nothing to push` - the
verb could not deliver on any user without
`remotes.origin.auto-track-bookmarks = "glob:*"`. `jj::push` now tracks, on the
remote the push targets, every local bookmark the revset names *before* it
pushes - read out of the same revset, so the two cannot disagree, and only the
bookmarks being delivered, never the repository's. `push_remote` resolves the
remote jj will use (`git.push`, else `origin`, else the only remote). Pinned by
`jj::tests::a_fresh_untracked_bookmark_is_pushed_when_the_revset_is_pushed`,
which disables auto-tracking in the repository's own config so it fails without
the fix rather than passing on a machine that sets it. That fixture also writes
its own `user.name`/`user.email`: jj has no identity on a clean config, so
`jj git push` refuses a commit with no author, and the first CI run of the test
failed that way - the same developer-config inheritance as `bug-hunt/45`.

**Verified.** `mise run check` is green (changelog validation, fmt, clippy with
warnings as errors, build and the full suite), and the `tests/deliver.rs` cases
pass under the CI-shaped environment (`env -u JJ_CONFIG -u XDG_CONFIG_HOME
HOME=$(mktemp -d)`). The forge gained three unit tests
(`deliver_opens_one_pull_request_and_reports_delivered`,
`deliver_without_a_forge_pushes_cleanly`,
`deliver_stops_and_names_a_failed_check`), the verb four integration tests in
`tests/deliver.rs` (no-forge push, check-fails before push, a configured forge
reached and its non-GitHub origin reported, an unknown Worker refused), the
push regression test above, and the contract test in `tests/deliver_contract.rs`.
Recorded with `momo add` and `momo fix`.

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
