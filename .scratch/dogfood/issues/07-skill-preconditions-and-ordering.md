# The dispatch skill's preconditions and ordering do not match the tool

Status: resolved
Type: docs
Blocked by: none

## Problem

`skills/jjfx-dispatch/SKILL.md` is the documented way to drive jjfx, and a
first-hand drive found it wrong or silent in four places:

- **Ordering.** "Before dispatching" lists the runtime profile ahead of pool
  capacity, but `jjfx pool profile` fails until the pool exists (see
  `.scratch/dogfood/issues/04-pool-profile-before-the-pool-exists.md`).
- **It never says to pin the binary.** A stale `jjfx 0.54.0` sits first on
  `PATH` on the drive's machine, and the skill's central claim - that
  `jjfx help <command>` prints the parser-derived flag surface - is false
  against it, producing misleading results.
- **The Land section presents `absorb`/`rebase`/`open-pr` as routine.** It
  should say what each requires to be safe: an immutable trunk, a resolvable
  bookmark, a GitHub remote.
- **It does not say the opencode runtime has no lifecycle plugin** installed,
  or what that costs (a Worker waiting on you still reads `busy`).

## Acceptance Criteria

- [x] The checklist order matches what the commands actually require.
- [x] It says to pin the binary, and how to check which one is running.
- [x] Each landing verb names its precondition.
- [x] The lifecycle-plugin gap is stated, with what it costs.

## References

- `skills/jjfx-dispatch/SKILL.md`
- `.scratch/dogfood/issues/01-absorb-must-not-rewrite-trunk.md`
- `.scratch/dogfood/issues/02-resolve-the-worker-bookmark-before-mutating.md`
- `.scratch/dogfood/issues/03-open-pr-refuses-a-non-github-origin.md`

## Resolution

Every claim was checked against the tool, not taken on trust. Two had moved.

**Claim 1 (ordering) - no longer true; the order was not changed.**
`dogfood/04` made `jjfx pool profile <runtime>` provision the empty Pool
document before writing the profile (`WorkerPool::set_profile` calls
`load_or_create`; `StateStore::set_pool_profile` then edits it), so the drive's
failure is gone. Ran it in a fresh colocated repo with no Pool state:
`jjfx pool profile opencode` prints `Pool profile set to opencode` and exits 0,
creating only `.jj/pools/default/pool.json` with `size = 0` and no Workers.
Both orders work, so the checklist order was left as it is; item 3 gained one
sentence saying naming a runtime provisions the empty Pool, so it is safe
before `jjfx pool <N>`.

**Claim 2 (pin the binary) - verified live.** `which -a jjfx` puts
`/Users/jarvis/.cargo/bin/jjfx` (0.54.0) first, ahead of the mise shim (also
0.54.0), while the repository builds 0.55.0-dev. The stale binary does not know
`absorb` at all - `jjfx help absorb` prints the generic top-level page and
`jjfx absorb --help` says `Unknown command: absorb` - so the skill's promise
that `jjfx help <command>` is the parser-derived surface was false against it.
Added a paragraph after that promise: check with `command -v jjfx` and
`jjfx version`, pin the binary you mean, and re-check after any upgrade.

**Claim 3 (landing preconditions) - two of the three moved.** Read from the
code and the tests:

- `absorb` needs `trunk()` to resolve (a fetched `main@origin`) and refuses by
  name when it does not (`dogfood/01`); resolves the Worker's bookmark by
  Ticket (`dogfood/02`); and - since `dogfood/09` landed today - excludes the
  recorded stack base, so it cannot fold a stacked Worker into its Blocker's
  commit. It pushes only when it absorbed something. The precondition is a
  *resolvable* trunk, not an immutable one: the destination pin makes the fold
  safe whether or not trunk is immutable.
- `rebase` needs the Worker's bookmark to resolve by Ticket; it rebases onto
  `main` and pushes, and a push matching no bookmark is a failure.
- `open-pr` needs the bookmark to resolve and the Pool's `gh_repo` to be a real
  GitHub repository (`dogfood/03`, `10`, `11`); a non-GitHub `origin` is
  refused by name.

The Land section now names each verb's precondition. It also corrects the
`open-pr` description: it does not weld or run the build check.
`open_pr_command` -> `WorkerActions::open_pull_request` -> `gh -R <repo> pr view
<branch> --web`, and the test
`open_pull_request_still_reaches_gh_with_a_configured_gh_repo` pins that exact
invocation; the weld, `jj fix`, check and push are the forge, started from the
TUI, not this verb. The old sentence contradicted the code while agreeing with
the `open-pr` help *summary* - a hand-written string, unlike the parser-derived
usage and flags - so that summary is worth a separate look.

**Claim 4 (lifecycle plugin) - verified live.** `jjfx hooks status` reports
`opencode: missing: lifecycle plugin` (there is no `~/.config/opencode/plugins/`
on this machine), while claude and codex have hook entries. The bullet now names
the opencode plugin at `~/.config/opencode/plugins/jjfx-lifecycle.ts` and says
that with none installed an opencode Worker waiting on you still reads `busy` -
the cost the bullet already carried.

**Could a claim be mechanically tested?** The central one could, cheaply: a test
could assert that every flag on a `CommandHelp` page is accepted by the parser
(and every accepted flag appears on the page) - exactly the "cannot drift"
promise - which would also catch a hand-written page like the `open-pr` summary.
The precondition claims are already held by existing tests
(`absorb_refuses_when_trunk_does_not_resolve_and_leaves_main_alone`,
`absorb_never_folds_a_stacked_workers_change_into_its_blockers_commit`,
`rebase_and_open_pull_request_resolve_the_prefixed_bookmark_the_agent_created`,
`open_pull_request_refuses_a_legacy_fabricated_gh_repo_before_calling_gh`).

Docs-only change, so it skips `mise run check`. Every command above was run
against the repository's built 0.55.0-dev binary.
