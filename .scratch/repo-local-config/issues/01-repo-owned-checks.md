# `[checks]` is repo-owned but can only be configured user-globally

Status: resolved
Type: task
Blocked by: none

## Problem

`[checks]` is documented as "the repo-owned build check the forge runs against the welded tree" (`docs/configuration.md`, §`[checks]`), and it exists to catch the case where two changes are each green, mergeable and at zero open threads while not compiling together - Weld has already rebased onto trunk when the check runs, so it sees what `main` will actually be.

But the command can only be set in the **user-global** config, `${XDG_CONFIG_HOME:-~/.config}/jjfx/config.toml`, which jjfx reads once at startup and applies to **every** repository it welds in (`src/config.rs::config_path`). So a check configured for one repo runs in all of them.

Concretely: `mono` wants `./gradlew :app:backend:compileKotlin` (that repo's own merge-side rule (`mono`'s `docs/agents/ai-review.md` §"Merging a stack")). Setting it globally means jjfx then runs `./gradlew` in every other repository too, where it may not exist - and a command that cannot run is a **loud failure**, not a silent pass ("a command that does not exist (a shell reports exit 127) ... is a loud failure, never a silent pass", `docs/configuration.md`). The push is then blocked in repos the user never configured. The only per-repo workaround today is to edit the global file when switching repos, which is not a workflow.

Note the asymmetry: every other repo-owned thing already lives in the repo - the lifecycle hooks are `<repo>/.jjfx/setup.sh` and `<repo>/.jjfx/teardown.sh` (`src/engine/workspace.rs`). `[checks]` is the one thing documented as repo-owned that is not.

## Evidence

- `src/config.rs:637` `config_path()` resolves exactly one path, `${XDG_CONFIG_HOME:-~/.config}/jjfx/config.toml`; `load()` (line 652) reads only that. There is no second layer.
- `docs/configuration.md` (top): "jjfx reads its own settings once at startup from `${XDG_CONFIG_HOME:-~/.config}/jjfx/config.toml` ... it is the only file jjfx itself owns."
- `docs/configuration.md` §`[checks]`: "The repo-owned build check the forge runs against the welded tree".
- `src/engine/workspace.rs:18-19`: the repo-owned hooks are read from `<repo>/.jjfx/`.
- `.scratch/opinionated-loop/issues/18-forge-build-check.md`: the ticket that added `[checks]`.

## Trust boundary (read before choosing an option)

`config_path()`'s own comment rejects a relative fallback because it "would resolve against the process CWD (letting a repository plant it)" - the user-global-only rule is deliberate. A repo-local config re-opens that question, so the decision should be explicit rather than incidental.

Two things bound the risk: the repo already executes code through `.jjfx/setup.sh`, so a repo-local `[checks]` is not a *new kind* of trust; and `[checks]` runs through the user's shell in the workspace, which the supervised agent is already doing. What is new is that it would run **on push, automatically**, without the user invoking anything.

## Options

1. **A repo-local `[checks]` at `<repo>/.jjfx/config.toml`**, merged over the user-global config (repo wins). Smallest change, makes `[checks]` genuinely repo-owned, and matches where the other repo-owned things live. Needs decisions on: per-key override vs whole-section replace; whether an unknown key in the repo file is a loud error (it should be, to match the user file); and whether the repo file is trusted by default or must be opted into (e.g. a user-global allowlist).
2. **A general repo-local config layer** - every section overridable from `<repo>/.jjfx/config.toml`. More consistent long-term, but it lets a repository set `[agent] command` and `[terminal] launch_command`, which are executable-affecting. That deserves an ADR, not a side effect of fixing `[checks]`.
3. **A repo-owned hook, `<repo>/.jjfx/check.sh`**, run when present. No TOML layering at all, and it reuses the hook pattern and its trust story exactly; loses the `timeout_secs` / `enabled` keys (or turns them into env vars).

Recommendation: 1 (scoped, no new ADR) or 3 (no layering). Not 2 in this ticket.

## Acceptance Criteria

- [x] `[checks]` can be configured per repository, and the repo's value wins over the user-global one.
- [x] A repo with no repo-local check keeps today's behaviour: the user-global `[checks]`, or no check at all.
- [x] The chosen trust story is stated in `docs/configuration.md`, including whether a repo-local file is read by default.
- [x] An unknown or invalid key in a repo-local file fails loudly, naming the file.
- [x] Tests cover: repo value overrides user value; the user value applies when the repo sets none; no config anywhere is a no-op; an invalid repo-local file is an error.
- [x] `docs/configuration.md` §`[checks]` is updated, and the "the only file jjfx itself owns" sentence at the top no longer contradicts it.
- [x] `mise run check` is green.

## Out of Scope

- Generalising the layer to sections other than `[checks]` (option 2).
- Changing how the check itself runs (welded tree, shell, timeout, failure semantics).

## References

- `src/config.rs` (`config_path`, `load`, `load_from`, `ChecksConfig::validate`)
- `src/checks.rs`, `src/forge.rs` (`Step::Check`)
- `docs/configuration.md` §`[checks]`
- `src/engine/workspace.rs` (the `.jjfx/` hook paths)
- `.scratch/opinionated-loop/issues/18-forge-build-check.md`

## Comments

- 2026-10-16: Filed from the `mono` workspace-provisioning change. `mono` wants `./gradlew :app:backend:compileKotlin` as its forge check (its own merge-side rule), and there is no way to set it for `mono` alone - the global file would then run Gradle in every other repository.
- 2026-10-16: Resolved as option 1, read by default. `src/config.rs` gains `RepoConfig` (`<repo>/.jjfx/config.toml`, with `deny_unknown_fields` so only `[checks]` is settable and any other section is a loud error naming the file) and `Config::checks_for`, which the forge settings are built from in `src/lib.rs`. A repository's `[checks]` replaces the user-global one wholesale; with no repository file the user config is unchanged. Trust: read by default, because the file sits next to `setup.sh` / `teardown.sh`, which jjfx already executes when it creates a workspace in the repository - so it grants no capability the repository did not already have. Documented in `docs/configuration.md` (§"Repository-local checks") and pointed at from `docs/workspace-hooks.md`. `config::tests` covers repository-over-user, user-when-the-repository-sets-none, unconfigured-is-a-no-op, a repository disabling a global check, and the three rejection paths (unknown section, unknown checks key, blank command). `momo add` records the change.
