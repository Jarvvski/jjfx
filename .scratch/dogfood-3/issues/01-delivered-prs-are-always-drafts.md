# Every delivered PR is a draft, and a repository cannot choose its PR policy

Status: needs-triage
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

- A repository can choose whether the Pull Requests jjfx opens for it are
  drafts, without editing the user-global config, **or** `deliver` marks a PR
  ready when the repository's policy is not to draft - whichever shape the
  maintainer prefers, decided and recorded rather than left implicit.
- The landing flow `deliver` exists for completes without a hand-run step jjfx
  never names: after `deliver` on a green check, the PR can be merged.
- If a draft PR is genuinely the intended end state, `deliver` (or `help
  deliver`) says so and names the command that undoes it.
- `docs/configuration.md`'s `[forge]` section and the repository-owned sections
  list agree with the behaviour.
