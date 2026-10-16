# Where are pools declared?

Type: grilling
Status: ready-for-human
Blocked by: none

## Question

A pool is a property of a Repository — but the repository-local config cannot hold it. `RepoConfig` reads `<repo>/.jjfx/config.toml` with `deny_unknown_fields` and accepts **only** `[checks]`, deliberately: "a repository must not be able to silently configure jjfx beyond its build check". So pools have nowhere to be declared:

- **Grow `RepoConfig`** with a `[pools]` section. Right scope, but it reverses a policy set deliberately — and only hours before this effort began. This is the same wall the `todo-md` test-drive hit when it wanted `[tickets] source = "local"` per repository, and had to fake `XDG_CONFIG_HOME` to get it.
- **User-global config** — `${XDG_CONFIG_HOME}/jjfx/config.toml` holds pools for every repository. Wrong scope: a pool named "bugs" is meaningless in an unrelated repo, and its Ticket source certainly is.
- **A new repository-local file** — for example `.jjfx/pools.toml` — so `config.toml` keeps its narrow policy intact and pools get a file of their own.

Also decide **declarative vs imperative**: do pools exist because config declares them (so a fresh checkout has them), or because `jjfx pool create <name>` was run (so they are state, not config)? Or both, with config seeding and commands adjusting — and if both, which wins on conflict.

This is the sharpest fork on the map, because it decides whether the repo-local policy stays honest or is quietly widened.

## Answer

<!-- recorded on resolution -->
