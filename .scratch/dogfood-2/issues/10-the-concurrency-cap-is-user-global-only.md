# The concurrency cap can only be set user-globally

Status: needs-triage
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

- [ ] A repository can declare its own dispatch ceiling (e.g. `[dispatch]` as a
      repo-owned section alongside `[checks]`/`[pools]`/`[tickets]`), or
      `dispatch` takes a per-invocation concurrency flag.
- [ ] Whichever is chosen is reported in `status --json`'s `governor` so the
      binding limit is visible where the pool is read.
- [ ] The skill says how to set the limit without editing the user's global
      config.

## References

- `src/config.rs` (`DispatchConfig`, `RepoConfig`), `src/engine/governor.rs`
- `.scratch/harness/issues/05-capacity-governor.md`
- `docs/adr/0014-*.md` (a repository may select, not grant)
