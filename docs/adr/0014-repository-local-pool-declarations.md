# A repository-local pool declaration may select, but not grant

Status: accepted

## Context

ADR 0012 made a Repository hold several named Worker Pools. Where a pool is
declared decides whether the repository-local config policy - set deliberately,
hours before that effort began - stays honest or is quietly widened.

That policy's rule is a capability rule, written down when `[checks]` became the
one repository-owned setting: the file is read by default because it "grants no
capability the repository did not already have". `[checks]` qualified only
because `.jjfx/setup.sh` and `teardown.sh` already execute arbitrary code in the
repository. The same effort deferred the general case explicitly: a repo-local
layer that could set `[agent] command` or `[terminal] launch_command` is
"executable-affecting" and "deserves an ADR, not a side effect of fixing
`[checks]`".

A pool owns five things - an Agent Runtime Profile, a Ticket source, a size, a
spend cap and a routing label - and one of them is executable-affecting.

## Decision

**A repository-local pool declaration may select among capabilities the
user-global config already granted, and may not grant one.**

- **The Agent Runtime Profile is a reference.** A pool names a profile the user
  config defines; it cannot define an agent command, a model, or credentials.
- **The Ticket source is a reference** to a configured source. A repository
  cannot introduce a new source kind.
- **Size, spend cap and routing label are repository-local**, because none of
  them grants a capability: size is bounded by the global ceiling, a cap is a
  restriction that can only lower, and a label is inert.

**Pools are declared in `<repo>/.jjfx/config.toml`, in a `[pools]` section beside
`[checks]`.** `deny_unknown_fields` stays; the permitted set grows from
`{checks}` to `{checks, pools}`. A second repo-local file would buy a second
trust story, a second parse path and a second error message to keep in sync, and
nothing else.

**Declaration is the source of truth; the verbs edit it.** `jjfx pool create` and
`pool destroy` write the `[pools]` section rather than a parallel state, so there
is no conflict to resolve. Absent `[pools]`, the Repository has one implicit
`default` pool and behaves exactly as before.

## Considered options

- **A second repo-local file (`.jjfx/pools.toml`)** - `config.toml` keeps its
  literal policy, at the cost of two files, two trust stories and two error
  messages, the second existing only to avoid a one-line change to the first.
- **User-global declaration** - pools defined once for every Repository. Wrong
  scope: a pool's state is per-Repository (ADR 0013), so this names lanes in
  repositories that have none.
- **Letting a repo-local pool define its runtime profile outright** - the
  executable-affecting case. Not rejected on principle, but it is a larger trust
  decision than this design needs, and nothing in the destination requires it.

## Consequences

- The repository-local policy is unchanged in substance: the file still grants
  nothing. Only the list of sections it may carry grows.
- A Repository with no global `[spend] cap_usd` is exposed to whatever size a
  cloned Repository declares. The mitigation is to declare a ceiling, which
  `docs/configuration.md` should say when this is implemented.
- Because a declaration can grant nothing, it is safe for the declaration to be
  *committed* - teammates share lane definitions, never running Workers.
