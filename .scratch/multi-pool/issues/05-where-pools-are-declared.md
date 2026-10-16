# Where are pools declared?

Type: grilling
Status: resolved
Blocked by: none

## Question

A pool is a property of a Repository — but the repository-local config cannot hold it. `RepoConfig` reads `<repo>/.jjfx/config.toml` with `deny_unknown_fields` and accepts **only** `[checks]`, deliberately: "a repository must not be able to silently configure jjfx beyond its build check". So pools have nowhere to be declared:

- **Grow `RepoConfig`** with a `[pools]` section. Right scope, but it reverses a policy set deliberately — and only hours before this effort began. This is the same wall the `todo-md` test-drive hit when it wanted `[tickets] source = "local"` per repository, and had to fake `XDG_CONFIG_HOME` to get it.
- **User-global config** — `${XDG_CONFIG_HOME}/jjfx/config.toml` holds pools for every repository. Wrong scope: a pool named "bugs" is meaningless in an unrelated repo, and its Ticket source certainly is.
- **A new repository-local file** — for example `.jjfx/pools.toml` — so `config.toml` keeps its narrow policy intact and pools get a file of their own.

Also decide **declarative vs imperative**: do pools exist because config declares them (so a fresh checkout has them), or because `jjfx pool create <name>` was run (so they are state, not config)? Or both, with config seeding and commands adjusting — and if both, which wins on conflict.

This is the sharpest fork on the map, because it decides whether the repo-local policy stays honest or is quietly widened.

**Inherited from *How does a Ticket reach a pool?*** (resolved 2026-10-16) — three schema questions this ticket now owns, because that ticket decided the routing behaviour and left the shape to the config surface:

- The **ready label is per pool**, since each pool's source may discover by a different label. `--label` therefore becomes an override on the pool's configured value rather than a global default.
- Whether a pool may have **no Ticket source** at all — a lane reachable only by an explicit `--pool`.
- A pool **declares the label that routes to it**; two pools declaring the same label must be refused at config load, not resolved by precedence.

## Answer

Resolved 2026-10-16, the owner having delegated the frontier's decisions. The reasoning below is what the recommendation rests on.

**The governing rule is capability, not section count.** The `repo-local-config` effort that built this wall wrote the rule down: the file is read by default because it "grants no capability the repository did not already have", and `[checks]` qualified only because `.jjfx/setup.sh` already executes arbitrary code in the repository. So the question is not *where* a pool is declared until it is *what a declaration can do*.

**Selection, not definition.** A pool owns five things and they split by capability:

| Field | Capability it carries | Repository-local? |
| --- | --- | --- |
| Agent Runtime Profile | **executable-affecting** — names the command and model that run | reference only |
| Ticket source | selects where work comes from | reference only |
| size | spends money, bounded by the global ceiling | yes |
| spend cap | a *restriction*; it can only lower | yes |
| routing label | inert | yes |

A repository-local pool may **reference** a profile and a source the user-global config defines, set its size, cap its spend, and declare its routing label. It may not define an agent command, a model, credentials, or a new source kind. That is what keeps the policy honest on its own terms instead of widening it — the file can select among capabilities the user already granted and grant none — and it is exactly the `[agent] command` case the earlier effort deferred ("that deserves an ADR, not a side effect of fixing `[checks]`"). It lands as **ADR 0014**.

**Declared in `RepoConfig`, beside `[checks]`.** With the capability rule constraining the schema, a second file buys a second trust story, a second parse path and a second error message to keep in sync, and nothing else. `RepoConfig` grows `[pools]`; `deny_unknown_fields` stays; the permitted set moves from `{checks}` to `{checks, pools}`, and the loud error message and the docs move with it. The policy was never about a section count.

**Declarative, with the verbs as editors.** Pools exist because `<repo>/.jjfx/config.toml` declares them — committed, like `[checks]`, so a fresh clone has the lanes and the state directory materialises on first use. `jjfx pool create <name>` and `pool destroy` therefore *edit that section* rather than writing a second source of truth, which is what makes "which wins on conflict" a non-question. Absent `[pools]`, nothing changes: one implicit `default` pool, and `jjfx pool <N>` keeps working.

**The three inherited questions, settled:**

- The ready label is **per pool**, defaulting to the global value, with `--label` overriding it for one Dispatch.
- A pool **may have no Ticket source** — a lane reachable only by an explicit `--pool`, which is what lets ad-hoc `--text` work land in a named lane.
- Two pools declaring the same routing label is a **config error at load**, naming both, not a precedence rule.

**Consequence worth naming:** the global spend ceiling is what bounds a declared size, so a Repository with no `[spend] cap_usd` is exposed to whatever size a cloned Repository declares. Declaring a ceiling is the mitigation, and `docs/configuration.md` should say so when this is implemented.
