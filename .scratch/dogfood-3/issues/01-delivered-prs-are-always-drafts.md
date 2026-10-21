# Every delivered PR is a draft, and a repository cannot choose its PR policy

Status: resolved
Type: bug
Blocked by: none

## Problem

`jjfx deliver` opens the Pull Request as a **draft**, and there is no way for the
repository to say otherwise. The forge's `draft` setting defaults to `true`
(`docs/configuration.md`, `[forge]`), and `[forge]` is **user-global only**: a
repository config may set `[checks]`, `[dispatch]`, `[pools]` and `[tickets]`,
and anything else is refused at load:

```
$ cat .jjfx/config.toml          # the fixture's config, with a [forge] section
$ jjfx ticket list
Error: parsing config .../.jjfx/config.toml - a repository may set only
[checks], [dispatch], [pools] and [tickets]; a pool and a [tickets] selection
may select a user-global profile and Ticket source, never define one, and
[dispatch] may only tighten the user-global ceiling
Caused by:
    TOML parse error at line 20, column 2
       |
    20 | [forge]
       |  ^^^^^
       |  unknown field `forge`, expected one of `checks`, `dispatch`, `pools`, `tickets`
```

So the flow `deliver` exists for - push, open a PR, read its check, merge on
green - cannot complete without a step jjfx never mentions. On the third drive's
fixture, both delivered PRs were drafts and the merge was refused:

```
$ jjfx deliver worker-348336
Delivering worker-348336: weld, check, push, Pull Request
Delivered worker-348336

$ gh pr list -R Jarvvski/jjfx-drive-3 --json number,isDraft
2 adam/tally--02-csv-input draft=true
1 adam/tally--01-percentile-durations draft=true

$ gh pr checks 1 -R Jarvvski/jjfx-drive-3
test  pass  7s        # the check is green; the PR is still unmergeable

$ gh pr merge 1 -R Jarvvski/jjfx-drive-3 --squash
GraphQL: Pull Request is still a draft (mergePullRequest)

$ jjfx help | grep -iE 'draft|ready|merge'
  jjfx review <worker>     Address PR review comments
  jjfx open-pr <worker>    Open a Worker's Pull Request (browser)
  (no verb marks a Pull Request ready, and none merges one)
```

`gh pr ready` fixes it, and nothing in jjfx's help, docs or the dispatch prompt
says so. An agent that lands through jjfx alone therefore stalls at the last
step with a green check and a PR it cannot merge - and the failure is not even
attributed to the draft: `deliver` reports `Delivered`, which is true, and the
refusal comes from `gh` afterwards.

## Why this is not simply "user-global by design"

`[forge]` is excluded under ADR 0014's capability rule: a repository may select
among what the user granted, never grant a new capability. But `draft` grants no
capability - it is a preference about *this repository's own* Pull Requests, the
same kind of property as `[checks]` (which the repository does own) and unlike a
model, a credential or a runtime. `pull_requests = false` is arguably the same:
"this repository has no forge" is a property of the repository.

## Acceptance Criteria

- [x] A repository can choose whether the Pull Requests jjfx opens for it are
  drafts, without editing the user-global config, **or** `deliver` marks a PR
  ready when the repository's policy is not to draft - whichever shape the
  maintainer prefers, decided and recorded rather than left implicit.
- [x] The landing flow `deliver` exists for completes without a hand-run step jjfx
  never names: after `deliver` on a green check, the PR can be merged.
- [x] If a draft PR is genuinely the intended end state, `deliver` (or `help
  deliver`) says so and names the command that undoes it.
- [x] `docs/configuration.md`'s `[forge]` section and the repository-owned sections
  list agree with the behaviour.

## Resolution

**Chose the repository-owned `[forge]` section.** A repository now sets its own Pull Request policy in `<repo>/.jjfx/config.toml`:

```toml
[forge]
draft = false
```

That is the shape the first criterion prefers, and it fixes the stall at its source: the landing flow `jjfx deliver` completes when the repository says its Pull Requests are not drafts, because `gh pr create` then omits `--draft` and the PR is mergeable on a green check. `deliver` also now *names* the draft case rather than leaving it implicit: a delivery that opens a draft reports it and points at `gh pr ready <number>`, the command that marks it mergeable, and `jjfx help deliver` and `docs/configuration.md` say the same.

**Which side of the ADR 0014 line this falls on: `draft` selects, `pull_requests` may only restrict.** `draft` is a preference about the repository's *own* Pull Requests - the same kind of property as `[checks]` - and grants no capability, so a repository sets it freely. `pull_requests` gates whether jjfx performs the act of opening a Pull Request over `gh`, which does use the user's authority, so a repository may only turn its own forge **off**: the resolved value is `user_global && repository`, exactly as `[dispatch]` resolves to the *minimum* rather than the repository's value outright. A repository that asks for a forge the user withheld is held to the user's setting, never quietly widened. That is the ADR's "select, not grant" rule read against the two keys: neither can grant a capability, one can only restrict.

Because `[forge]` joins the repo-owned set, `RepoConfig` gains `forge: Option<RepoForgeConfig>` (`deny_unknown_fields`, every key optional so an absent key keeps the user-global value), the loader's refusal message and every list of repository-owned sections grow to `[checks]`, `[dispatch]`, `[forge]`, `[pools]` and `[tickets]`, and `Config::forge_for(repo_root)` is the one resolver - used by both `deliver` and the TUI's forge - so the two cannot drift.

Docs: `docs/configuration.md`'s `[forge]` section gains "Repository-local Pull Request policy" and the repository-owned lists name `[forge]`; `jjfx help deliver` names the draft policy and the `gh pr ready` that marks a draft mergeable.

Tests: unit tests in `src/config.rs` (a repository may choose its draft policy; may only turn its forge off, never on; an unknown `[forge]` key is refused) and `src/forge.rs` (a draft delivery names `gh pr ready`; a non-draft delivery opens a mergeable PR with no notice); a hermetic end-to-end case in `tests/deliver.rs` that a repository-owned `[forge]` is accepted at load. `mise run check` is green.

## References

- `src/config.rs` (`ForgeConfig`, `RepoForgeConfig`, `RepoConfig`, `Config::forge_for`)
- `src/forge.rs`, `src/forge/pull_requests.rs` (the draft notice), `src/cli.rs` (`deliver`)
- `docs/adr/0014-repository-local-pool-declarations.md` (a repository may select, not grant)

## Triage (2026-10-21)

Triaged from `needs-triage` to `ready-for-agent`. The evidence is a **live
reproduction during the third drive**: the fixture's own `.jjfx/config.toml` was
rejected at load for naming `[forge]`, and both delivered Pull Requests were
drafts whose merge `gh` then refused.

Independently **confirmed** in the code: `ForgeConfig` (`src/config.rs:531`)
carries `pull_requests` and `draft`, both defaulting to true, and the
repository-config loader accepts only `[checks]`, `[dispatch]`, `[pools]` and
`[tickets]`.

The acceptance criteria are unchanged. The open question - a repository-owned
`[forge]`, or `deliver` marking the Pull Request ready when the policy is not to
draft - is left to the implementer, who should record the decision and why.
