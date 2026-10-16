# Declare pools in `[pools]`

Type: task
Status: ready-for-agent
Blocked by: none

## Problem

A Repository can only have one Worker Pool, and nothing can declare otherwise: `RepoConfig` reads `<repo>/.jjfx/config.toml` with `deny_unknown_fields` and accepts **only** `[checks]`, so a pool has nowhere to be declared at all.

The design settled both halves - *Where are pools declared?* and **ADR 0014**:

- Pools are declared in `RepoConfig`, in a `[pools]` section beside `[checks]`. `deny_unknown_fields` stays; the permitted set grows from `{checks}` to `{checks, pools}`.
- A declaration may **select, not grant**. It may reference an Agent Runtime Profile and a Ticket source the user-global config defines, set its size, cap its spend, and declare its routing label. It may **not** define an agent command, a model, credentials, or a new source kind.

## Scope

- A `[pools]` section holding one entry per named pool: size, a reference to a user-global Agent Runtime Profile, a reference to a configured Ticket source, a spend cap, and a routing label. Each optional except the name, with documented defaults.
- **`default` is implicit when `[pools]` is absent**, and the Repository behaves exactly as it does today. This is the additive stance and it keeps `jjfx pool <N>` working.
- A pool **may have no Ticket source** - a lane reachable only by an explicit `--pool`.
- **Validation is loud**: two pools declaring the same routing label is an error at load naming both, not a precedence rule; a reference to a profile or source the user config does not define is an error; a size or cap that cannot be honoured is an error. Same rule as `[checks]` - a file that parses but is wrong fails at startup, naming the file.
- The ready label is **per pool**, defaulting to the global value.

## Acceptance Criteria

- [ ] `[pools]` parses, with every field optional but the name.
- [ ] With no `[pools]` section, behaviour is byte-for-byte today's, and `jjfx pool <N>` still sizes `default`.
- [ ] A declaration can only reference a profile/source the user config defines; naming an unknown one is a loud error naming the file.
- [ ] Duplicate routing labels are refused at load, naming both pools.
- [ ] An unknown key inside `[pools]` is still a loud error (`deny_unknown_fields` intact).
- [ ] `mise run check` is green.

## References

- `src/config.rs` (`RepoConfig`, `load_repo`, `ChecksConfig::validate`, the `deny_unknown_fields` error text)
- `docs/adr/0014-repository-local-pool-declarations.md`
- `docs/configuration.md` (the repository-local section)
