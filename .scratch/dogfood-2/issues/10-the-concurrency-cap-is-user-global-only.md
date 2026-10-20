# The concurrency cap can only be set user-globally

Status: resolved
Type: feature
Blocked by: none

## Problem

`harness/05` made the concurrency governor tool-enforced, reading
`[dispatch] max_concurrent`. But that setting lives only in the user-global config
(`${XDG_CONFIG_HOME}/jjfx/config.toml`); the repository-local file may set only
`[checks]`, `[pools]` and `[tickets]` (`src/config.rs`, `RepoConfig`), and there
is no `--max-concurrent` flag on `dispatch`.

So the one actor the governor is *for* - an agent about to fan work out - cannot
bound its own capacity. To exercise the governor at all in this drive it was
necessary to edit the user's global config by hand:

    $ printf '\n[dispatch]\nmax_concurrent = 2\n' >> ~/.config/jjfx/config.toml

which changes every repository that user drives, and is not something an agent
should be doing to test or to pace one backlog.

## Acceptance Criteria

- [x] A repository can declare its own dispatch ceiling (e.g. `[dispatch]` as a
      repo-owned section alongside `[checks]`/`[pools]`/`[tickets]`), or
      `dispatch` takes a per-invocation concurrency flag.
- [x] Whichever is chosen is reported in `status --json`'s `governor` so the
      binding limit is visible where the pool is read.
- [x] The skill says how to set the limit without editing the user's global
      config.

## Resolution

Chose the repository-owned `[dispatch]` section over a per-invocation flag: a
flag is transient, so a later `status` could not report the binding limit, which
the second criterion requires. `[dispatch]` now sits beside `[checks]`, `[pools]`
and `[tickets]` in `<repo>/.jjfx/config.toml` (`RepoConfig::dispatch`), reusing
`DispatchConfig` so `max_concurrent` means the same key in both files.

**Which side of the ADR 0014 line this falls on: select, not grant.** A ceiling
is a *restriction*, so it can only lower - the same reasoning the ADR already
gives for a pool's `cap_usd` ("a cap is a restriction that can only lower"). The
repository-local ceiling is therefore resolved as the **minimum** of the
user-global and repository-local values (`Config::dispatch_cap_for`, absent =
unbounded), never the repository's value outright:

- user-global 4, repository 2 → **2** (the repository tightens);
- user-global absent, repository 2 → **2** (still only a restriction - the case
  the finding hit, since a repository with no global cap can bound itself);
- user-global 2, repository 8 → **2** (a repository may **never** widen the
  ceiling the user granted).

Because it can only tighten, the declaration grants no capability the
user-global config did not already have, so it is safe under the ADR's rule. The
resolved limit flows through `Config::dispatch_cap_for` to every Dispatch seam
(`dispatch_with_capacity_prompt`, the orchestration wave, the TUI via
`DispatchSettings::concurrency_cap`) and to `pool_report`, so `status --json`'s
`governor.limit` and the prose `status` line report the **binding** limit, not
only the user-global one. Validation is loud: `max_concurrent = 0` and an unknown
key in the repository `[dispatch]` are errors naming the file, as for the other
repo-owned sections.

Docs: `docs/configuration.md` gains the section in `[dispatch]` and lists it as
repo-owned; `skills/jjfx-dispatch/SKILL.md` (step 3 and the Rules) and
`PLAYBOOK.md` §2/§5 say to set it in `<repo>/.jjfx/config.toml` and never in
`${XDG_CONFIG_HOME}/jjfx/config.toml`.

Tests: unit tests in `src/config.rs` for the no-ceiling, tighten, no-widen and
error cases plus `dispatch_settings`; end-to-end tests in
`tests/capacity_governor.rs` (`a_repository_dispatch_ceiling_binds_without_touching_the_user_global_config`,
`..._tightens_the_user_global_one`, `..._never_widens_the_user_global_one`),
driven through the binary with `support::stub_terminal`. `mise run check` is
green.

## References

- `src/config.rs` (`DispatchConfig`, `RepoConfig`), `src/engine/governor.rs`
- `.scratch/harness/issues/05-capacity-governor.md`
- `docs/adr/0014-*.md` (a repository may select, not grant)

## Triage (2026-10-20)

Triaged from `needs-triage` to `ready-for-agent`. The evidence is a **live
reproduction during the second drive**, not a code reading: the drive hit this
while working a real backlog through jjfx, and the report
(`.scratch/dogfood-2/report.md`) records what happened around it.

Independently **confirmed**: `docs/configuration.md` allows a repository to set
only `[checks]`, `[pools]` and `[tickets]`, so the cap has no repository-level
home.

The acceptance criteria are unchanged - they were already specific and
testable, which is why this needed triage rather than a rewrite.
