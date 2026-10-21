# Changelog

All notable changes to this project are documented here, newest first, managed
by [momo](https://github.com/Jarvvski/momo).

Unreleased changes live under `## [Unreleased]`, grouped by kind:

- `### Breaking` - an incompatible change
- `### Added` - new functionality, backwards compatible
- `### Fixed` - a backwards-compatible bug fix

Record a change with `momo break|add|fix "<description>"`; momo owns the heading
and the section it lands in. `momo status` derives the next version from the
unreleased changes and the latest tag, and the release workflow runs `momo
release` then `momo tag`, so **do not bump the version by hand**.

`jjfx` is pre-1.0, so the minor position is the breaking slot (Cargo's `^0.52.0`
means `>=0.52.0, <0.53.0`): `break` and `add` both raise the minor, `fix` raises
the patch. That is momo's zero-aware rule, and it matches the convention this
project used before adopting momo.

Entries here are versioned by `jjfx` alone; it is the only binary this
repository builds and releases.

Sections released before the momo adoption keep the old `(break)`/`(add)`/`(fix)`
markers on their entries; they are preserved as history and carry no meaning now.

## [Unreleased]

### Breaking

- jjfx status --json now emits an object (`workers` plus `waiting`) instead of a bare array, so the waiting set a capped Dispatch held back is machine-readable; consumers reading the array must read `.workers[]`.
- the forge opens one Pull Request per change against the default branch instead of assembling base-chained PR stacks and rewriting bodies with a `## Stack` section; a repository with no forge (Pull Requests off, or no origin remote) now forges cleanly rather than reporting a skip
- jjfx review no longer ingests an AI reviewer's findings: the `[review] reviewer` setting and its CodeRabbit and opencode-action adapters are removed, so a config that set `reviewer` must drop it; `[review] max_passes` remains as the tool-enforced re-review bound

### Added

- jjfx dispatch --json prints one machine-readable object per Ticket - its id, outcome, Worker and detail - including the Tickets a Dispatch held back or failed, so an agent reads a batch result instead of parsing prose.
- Every command has a help page: `jjfx help <command>` prints a command's usage, arguments and flags (derived from the same definitions the parser reads, so the two cannot drift), an unknown command fails naming it, and the top-level help now lists the Lifecycle group (`hooks install` / `hooks status`).
- jjfx dispatch can take a Ticket handed in directly - `--text`, `--from-file <path>`, or stdin (`-`) - with an optional `--id`, so an ad-hoc Dispatch consults no Ticket source and works in a repository with no `[tickets]` configuration, no Linear key, and no `.scratch/` tree.
- jjfx dispatch --all bases a dependent Ticket on its blocker's branch (resolved from the blocker's bookmark), so a chain of Tickets lands as a real stack instead of parallel branches off trunk; a dependent whose blocker has no branch is stated and based on trunk rather than left silent.
- jjfx dispatch --all names every Ticket it skipped and why when the ready set is empty - a blocked Ticket reports its blocker and that blocker's status, a Ticket with the wrong Status: reports it, and a malformed file keeps its diagnostic - on both the prose and --json paths, so an empty batch reads as nothing to do or nothing can be done instead of a bare [].
- jjfx wait blocks until Workers reach a condition - done, failed, terminal (default) or ticket-resolved - and carries the answer in its exit code (0 reached, 1 timed out, 2 terminal-but-unsatisfied), so an agent waits on a Worker instead of polling with sleep.
- jjfx sync fetches (when the repository has a remote) and moves the working copy in the current directory onto trunk - jj new for an empty @, jj rebase when it carries changes - reporting conflicts rather than resolving them, and jjfx status, where and root now warn when that working copy has fallen behind trunk, naming how far behind and the fix; the undocumented sync alias for refresh is gone, so sync no longer rebuilds the workspace cache.
- jjfx dispatch <TICKET> reports why a Ticket named by hand is not ready - a blocked Ticket names its blocker and that blocker's status, a Ticket with the wrong Status: reports it - then dispatches it anyway, because naming a Ticket is the override; the reason comes from the same readiness seam --all uses, so the two paths cannot drift apart.
- Worker Pool state is persisted a directory per pool - `.jj/pools/<name>/` holds the pool document, its Workers, Run Records, the waiting set, sessions and its locks - while repository-scoped state (the ws-cache, occupancy and undo) stays at `.jj/`; an existing repository converts automatically on first access, copying, verifying and only then removing the old layout, and refuses loudly, leaving the old layout intact, if it cannot verify the result.
- jjfx reads `[tickets] source` from a Repository's own `.jjfx/config.toml`, so a cloned Repository discovers its own local Tickets without the user config being changed for it; the section may select a configured source and may never define one, so a repository-local attempt to set a source's details or credentials is a loud error naming the file.
- jjfx pool create declares a named Worker Pool in the repository's [pools] config and jjfx pool destroy removes it, so lanes live in the committed declaration rather than a parallel state; jjfx pool list (and its --json) names every pool, including an implicit default, and jjfx pool <N> still sizes default. Removing a pool drains: it refuses while a Worker is busy, naming them, and refuses work that is neither pushed nor merged, enumerating each Worker's branch and its state before anything moves; --force overrides both and is the only path that discards work, saying so. Removal takes the pool's state directory and its Workers' Workspaces with the declaration, and touches only that pool's Workers - never another lane's, unlike clean.
- jjfx dispatch fills a named Worker Pool: --pool <name> selects the lane, a Repository with one pool implies it (so every existing single-pool invocation is unchanged), and a Repository with several refuses an unqualified Dispatch, naming the pools that exist, instead of silently choosing one; --text and --from-file follow the same rule, and Worker reservation, Dispatch Eligibility and the pool's size all apply to the selected pool.
- jjfx status and jjfx pool list report every Worker Pool the Repository holds: each Worker row names its pool, the Waiting: line names the pool that held each Ticket, and jjfx status --json gains a pool field on every workers[] entry and a waiting[] entry carrying its ticket and pool, so jq '.workers[] | select(.pool == "bugs")' reads one lane; a sibling pools key carries each pool's size and recorded spend against its own cap and window - a subtotal under the Repository-wide total - and a pool directory no declaration claims is named as an orphan rather than adopted.
- jjfx dispatch --all routes each Ticket to the Worker Pool whose routing label it carries: a pool declares the label that sends work to it ([pools.bugs] label = "bug"), a Ticket matching no pool is reported with what it carried rather than dropped, an explicit --pool still overrides routing to mean only that lane, and the ready label is per pool - --label overrides the pool's label, which overrides ready-for-agent. The local markdown convention gains a Labels: line, and a Ticket written without one carries the ready label alone, so a Repository that never adopted labels discovers exactly the work it always did.
- jjfx dispatch now enforces a Worker Pool's own spend cap alongside the Repository-wide ceiling: both limits bind, so a Dispatch proceeds only when every limit has room, and a refusal names the one that ran out - the pool when its own cap is the tighter constraint, the Repository when the ceiling is, because that is the dial the owner turns; jjfx dispatch --all holds a cap-blocked Ticket visibly in waiting (which jjfx status and jjfx status --json show) while an explicit Dispatch refuses loudly, and a pool's window and on_exceed default to the global [spend] values so a declaration states only what differs.
- A declared Worker Pool's state now materialises on first use: dispatching into a freshly declared [pools.<name>] lane provisions the Workers the declaration names - exactly as jjfx pool <N> does for default - so a committed lane works on a fresh clone with nobody provisioning it by hand; the lane's state is created once and never re-provisioned, and a removed lane stays removed.
- jjfx clean gains an optional --pool <name> scope that removes only that Worker Pool's non-default Workspaces, so one lane's tidy-up leaves another lane's work in progress alone; the Repository-wide sweep stays the explicit everything and stays the default, so every existing invocation is unchanged, and an unknown pool name is refused loudly, naming the pools that exist.
- jjfx dispatch draws each Worker Pool's work by that pool's own ready status: a pool may declare the `Status:` a Ticket must be in for its lane (`[pools.review] ready_status = "ready-for-review"`), defaulting to the user-global value, so a review lane draws `ready-for-review` while a lane that declares none keeps drawing the global status - from the same `.scratch/` tree; the status filters that pool's discovery exactly as its label routes it, for the `--all` fan-out and an explicit `--pool`, and a Ticket named by hand reports its readiness against the selected pool's status.
- jjfx dispatch --all --json explains an empty ready set instead of printing a bare []: the batch now carries the search as structure - the label it looked for and the root it looked under - as a skipped entry with a null ticket, alongside the per-Ticket diagnostics it already reported, so a caller can tell a wrong root or a label that matched nothing from a Repository with no work; the single-pool, --pool and fan-out paths all carry it and the prose output is unchanged.
- jjfx wait resolves its target Worker Pool the way dispatch does: --pool <name> selects the lane, a Repository with one pool implies it so --pool stays optional, and a Repository with several refuses an unqualified wait, naming the pools that exist, instead of acting on default alone; --all then means every Worker in the selected pool.
- jjfx's Worker Pool view is lane-aware: it names the Worker Pool it acts on and discovers Ready Tickets through that lane's own routing label and ready status, the same seam dispatch uses, so a lane that declares a label or its own ready_status draws its own work; w chooses the lane, and a Repository holding several refuses to show one until a lane is chosen, naming them, instead of silently showing default.
- jjfx status --json carries a queue array - the durable work queue of every step jjfx has queued, claimed, run and settled, each with its identity, state, when it was claimed and by what, its outcome, and the in-flight record a resumption needs - so interrupted or abandoned work is visible as structure rather than only as live Worker state, and a repeated step is recognisable instead of silently re-run.
- A dispatched Worker's opencode session is created as a child of the session that dispatched it - the OPENCODE_SESSION_ID opencode puts in that session's shell - whenever that session is already anchored to the Worker Workspace, and as a root session anchored to the Worker Workspace otherwise (opencode anchors a child to its parent's directory, so a Dispatch from an agent's own shell still launches), so the dispatcher can enumerate and collect it instead of polling state files; the opencode plugin reports each child's parentage across its whole lifecycle, the fold tracks many sessions per workspace while a child can never displace the agent that owns the workspace, and `jjfx children [<session>] [--json]` lists a session's children with their folded state, their title and their directory - defaulting to the session that ran it, so a parent asks from its own shell.
- jjfx resume re-runs the work an interruption left in flight - every step the durable queue records as unfinished (accepted, claimed, failed or abandoned) is re-opened and dispatched again, a step whose Run is still live is left running, and a step that reached done is left alone rather than redone - so a killed driver's in-flight children are recovered explicitly instead of being lost or silently repeated; the queue is reconciled first, and the resumption obeys the same capacity governor, spend limits and occupancy exclusions a Dispatch does, naming each Ticket it resumed and each it could not.
- The concurrency cap is a governor the tool enforces, not a number a prompt asks for: [dispatch] max_concurrent bounds the Runs in flight across every Dispatch and every orchestration wave - counting the Runs already running from an earlier Dispatch, not just the batch in hand - and the excess Tickets are held back visibly, each naming the setting and what has to change (a Run must finish) rather than being fired; jjfx status and jjfx status --json report the governor - the limit, how many Runs are in flight, and how many Tickets are held, as a governor object with limit, running and held - so the ceiling is readable before it binds, while a Pool's size stays what it always was: how many Workers exist, a capacity declaration rather than a ceiling.
- The review loop now drives every pass through the pool's decision log: a pass already recorded replays instead of reaching the reviewer, so a repeated review dimension and pass is avoided rather than only detected, and jjfx status --json shows the replayed step's detail so an operator can tell a replay from a pass that ran.
- jjfx ticket list lists every Ticket the configured source discovers - its Status, its Blockers and whether it is ready - as prose or stable --json, without dispatching; the blocked reason is the same line a dispatch --all prints, and the command is read-only, reconciling no Run and writing no Ticket.
- A repository can bound its own dispatch concurrency: a [dispatch] max_concurrent in <repo>/.jjfx/config.toml tightens the user-global ceiling (never widens it), so an agent can pace one backlog without editing the user's global config, and jjfx status --json reports the resolved limit as governor.limit.
- jjfx deliver performs the delivery the Dispatch prompt promises - weld onto trunk, run the repository check, push, and open one Pull Request - the same one forge path the TUI f key drives, now reachable from the shell so an agent lands without running gh pr create itself; the prompt, jjfx help and the skill name the command, rebase and open-pr state their narrower scope, and a repository with no forge still delivers cleanly.
- jjfx review drives the bounded re-review loop over a Pull Request's unresolved review threads: each pass reads them through `gh`, sends what it finds to the Worker, and stops on `[review] max_passes` or a stalled round - escalating unresolved findings for a human rather than reporting success.

### Fixed

- The `jjfx-dispatch` skill is corrected for 0.55.0 and made deterministic to drive: it reads terminal state from `jjfx status --json` (status, stuck, exit_code, error) rather than prose, documents both ticket sources (`[tickets] source = "linear" | "local"`), and covers `hooks install`, `--fg`/`--bg`, `absorb`, `resolve`, `undo`, and the repository build check the forge runs before pushing.
- The jjfx-dispatch skill stops transcribing the CLI's flag surface: it points agents at `jjfx help <command>` for a command's flags and reads what a Dispatch did from `jjfx dispatch --json` (one object per Ticket, including the ones it did not dispatch), so a flag or outcome the CLI changes cannot leave the skill teaching a stale one.
- A Run finalized by reconciliation now appends its Run Record - exactly once - and an opencode Run records the cost and tokens its own event log reports, so `jjfx status` shows real spend instead of `$0.0000 across 0 Run(s)`.
- jjfx dispatch works in a repository with no `main` bookmark - a Worker Workspace bases on `main` (then `master`, then the default Workspace's working copy) instead of failing - and a repository with no `origin` remote fails naming the fix (`jj git remote add origin <url>`, or the pool's `gh_repo`) instead of a bare owner/name format error about an empty slug.
- A finished Worker's local Ticket is now resolved from the durable Run Record store rather than live Worker state, so a linear dependency chain no longer deadlocks at claimed: Dispatch reconciles before it discovers ready work, jjfx status and jjfx pool list reconcile on read, and jjfx ticket reconcile resolves finished Tickets on demand with no Dispatch and no ready Ticket.
- jjfx dispatch exits non-zero when every Ticket failed, so a scripting caller can tell an all-failed batch from a successful one; a batch with a dispatched Ticket, or one deliberately held back under a cap, still exits zero, and the summary line now counts only Tickets actually dispatched.
- A background `jjfx dispatch <parent>` now prepares the Parent synchronously, so a failure - an unresolvable Ticket, a missing Pool, a rejected discovery - exits non-zero and reports the error instead of printing `orchestrating` for a background child that dies at once; a started runner names its log in the dispatch result and the JSON detail.
- jjfx logs stops following when the Worker's Run ends - it re-checks the terminal state on every pass and returns cleanly (printing why) instead of following a finished Worker forever, and it ends cleanly if the Run log or the Worker itself disappears mid-follow.
- jjfx dispatch <feature> orchestrates a local feature's Tickets in dependency order: a local feature Parent is a directory rather than a Ticket file, so it now resolves to no details of its own instead of failing as a missing Ticket, and a failed Ticket-details fetch names the configured source - the local root, or Linear - instead of always Linear.
- jjfx help now answers for every spelling the parser accepts - the short aliases (d, rb, pool create) and the pool synonyms - so the recovery hint command is documented instead of rejected as an unknown command, and the top-level list documents jjfx help itself.
- jjfx dispatch no longer grows the pool when the capacity prompt cannot be answered: with no terminal (a script) or an empty answer the pool stays at its size and the batch dispatches to the available Workers, so the expensive option is never the default.
- jjfx version now reports a -dev suffix for a build from a checkout, so a binary built from main after a release cannot be mistaken for the released version; the release archives and a crate installed from crates.io report the clean version.
- jjfx logs stops following a Worker that has gone idle, so it no longer blocks forever on the log its last Run left behind: logs follows the current Run rather than the Worker, and a caller that wants the next Run re-attaches or uses jjfx wait.
- jjfx pool resize <N> --pool <name> now reaches a materialised lane: it provisions or detaches the lane's Workers under the existing busy-Worker safety and moves the declaration to agree, so the size a user sets is the size jjfx status reports; previously it edited only the declaration, so a lane that had already materialised kept its old Workers and the edit looked like it worked but did nothing.
- jjfx pool <N> now moves a declared [pools.default]'s size with the lane it describes: sizing default still provisions and detaches its Workers under the existing busy-Worker safety, and when the Repository declares [pools.default] the declaration is moved to agree, so the size a user sets is the size jjfx status reports; a Repository that declares no [pools.default] is untouched, so sizing an implicit default still writes state only and never grows a declaration.
- jjfx pool list and jjfx status no longer write an unnamed 'pool state is absent' to stderr once per pool that has no state directory - a declared lane with no directory is normal, materialising on first use, and the WORKERS column already shows the absence - so a successful listing is silent.
- jjfx clean writes its confirmation prompt to stderr and, when stdin reaches EOF without an answer, refuses loudly with a non-zero exit instead of silently removing nothing and exiting 0
- Name the missing origin remote when orchestrating a single Ticket, instead of failing on an empty repository slug
- A malformed Worker Pool document is now reported naming the pool it belongs to (for example `Worker Pool alpha: cannot decode Worker Pool: ...`), so a user with several lanes can tell which one is corrupt; an absent pool document still prints nothing.
- jjfx absorb no longer rewrites trunk: it pins the absorb destination to the Worker's own commits above the mainline, refuses outright when the repository has no resolvable trunk (no fetched `main@origin`), naming the fix, and rolls the absorb back with `jj undo` if the mainline moved anyway - so a Worker whose change is already committed is a no-op instead of a silent rewrite of `main`.
- jjfx rebase, absorb and open-pr resolve the Worker's bookmark by Ticket (the prefixed name the agent actually created) instead of the bare Ticket id in Worker state, and fail before touching the Workspace when it is missing; a push that matches no bookmark is reported as a failure rather than a silent success.
- `jjfx pool profile <runtime>` works on a Repository with no Worker Pool state yet: it provisions the empty Pool document it needs - and no capacity - instead of failing with a bare `state is missing`, so the documented profile-then-capacity order is a working one; when the state is genuinely gone it refuses naming the missing state and the `jjfx pool <N>` command that creates it.
- A non-GitHub `origin` (a filesystem path or another forge) is now refused by name - `origin is not a GitHub remote` - before any Worker Pool state is written, and again when Dispatch resolves the repository identity, instead of persisting a slug guessed from its last two path segments and failing much later with a `gh` error naming a repository that never existed.
- A local Ticket is resolved only when the repository's [checks] command ran against its Run's change and passed - not merely when the agent's Run reported success. A Run whose check failed, could not run, or had none configured leaves the Ticket not-done and the writeback names why, so a repository with no [checks] never silently claims verification.
- jjfx dispatch, jjfx send and jjfx status --json report an opencode background Run by its session rather than a pid: the Run now starts by prompting its session through the daemon API with no client process, so an opencode Worker is busy with no pid, a dispatch's pid field is null and its detail names the session, and a send to a Worker with no session to resume delivers to a fresh session instead of spawning a client.
- The review loop is bounded by progress, not just the pass cap: a round that finds nothing new or retires nothing stops the loop and escalates its unresolved findings, instead of buying another pass.
- open-pr, review and send refuse a Worker Pool's persisted gh_repo when it is the slug a non-GitHub origin fabricated - the value a Pool created before the earlier origin fix still holds - reusing the existing 'origin is not a GitHub remote' refusal before any gh call, instead of looking up a repository that never existed and failing with a GraphQL error naming it; a gh_repo that is not that fabricated value is untouched.
- jjfx colocation status answers without snapshotting the working copy, so the read can no longer block silently behind another jj process holding the repository's lock
- jjfx's two remaining snapshotting reads - resolving a Ticket's bookmark (the landing path's source of truth for which bookmark carries a Ticket's work) and checking whether a dependent's revision still exists - now read with --ignore-working-copy, so they answer instead of blocking silently behind another jj process holding the repository's working-copy lock
- The forge's own slug derivation refuses a non-GitHub `origin` - a filesystem path or another forge - by name, `origin is not a GitHub remote`, instead of mining its last two path segments into an `owner/name` slug and handing it to `gh`; the Pull Request step reports that refusal and reaches no network call, and the Pull Request read behind `jjfx status` treats it as unknown rather than a confident empty, while a legitimate GitHub `origin` is unchanged.
- jjfx absorb no longer folds a stacked Worker's change into its Blocker's commit: the commit a dependent Workspace is materialised on is recorded as its stack base and excluded from the absorb destination the same way trunk is, so a landing verb cannot rewrite a change that belongs to another Ticket's pull request
- jjfx help answers for a multi-word command group: `jjfx help ticket` prints the group's subcommands (and a group with its own page, like `pool`, also lists them) instead of reporting the group as an unknown command.
- a Dispatch issued from inside an opencode session launches a Worker: the Run's session is anchored to the Worker Workspace as a root session whenever a child session could not be (opencode anchors a child to its parent's directory), and a launch that fails the anchor check removes the session it created instead of leaving an orphan for `jjfx children` to report
- jjfx absorb, rebase and open-pr land a freshly dispatched Worker's bookmark without a manual push first - a local bookmark whose remote-tracking ref does not exist yet is no longer counted twice and read as ambiguous - and a dependent whose Blocker has no usable branch is reported once, naming the Blocker's bookmark state (missing, ambiguous, or trunk) instead of a bare 'no branch'.
- a re-dispatch of a Ticket whose queue step had settled failed now supersedes the stale failure, so the durable queue reports the latest outcome and resume no longer re-runs work that has landed
- jjfx review reads a pull request's failing checks from the field a current gh returns - `bucket` (or `state`) instead of the removed `conclusion` - so it runs on gh 2.102 instead of aborting before any Run; a gh whose checks output jjfx cannot read now fails naming the command and quoting gh's own output rather than a bare serde error.
- jjfx dispatch reports a Ticket with no free Worker as `waiting` rather than `failed` - `dispatch --all` and a named `dispatch` agree on the same capacity gap, and the Ticket is queued for a later Dispatch - and a Ticket held back by the concurrency cap names the in-flight count that actually bound the batch (the count `jjfx status --json` reports), not the pre-launch `0` a first Dispatch used to print
- jjfx rebase reports a conflicted rebase as a named outcome instead of a push failure whose rollback implied the rebase was undone
- A finished Worker is returned to the pool automatically: a Worker whose Run has finished (done or failed) is reclaimed to idle on the next Dispatch, so a Pool of N serves a backlog larger than N across successive rounds without a manual jjfx pool reset.
- jjfx logs follows a session-delivered Run's opencode session - reading the session's messages and printing its activity - instead of tailing a process log that stays empty, and says plainly that there is no process log and names the session when the daemon cannot be reached
- jjfx deliver tracks a fresh Worker bookmark on the push remote before pushing, so it delivers on a clean jj config - jj git push --revisions refuses to create a remote bookmark for a local one that is not tracked, which only a developer remotes.origin.auto-track-bookmarks setting masked.
- jjfx review reads a pull request's review threads on every invocation instead of replaying a pass recorded by an earlier run: a review thread opened after a converged pass is now seen, so the loop no longer reports convergence while the pull request has an unresolved thread.

## [0.55.0] - 2026-10-16

### Breaking

- Remove the Pi Agent Runtime: a persisted `"agent": "pi"` pool profile or Worker is now rejected with a message naming the removal, and a Worker recorded as `pi` is reconciled to a terminal state instead of being stranded busy; claude, codex, and opencode are unchanged.

### Added

- The ticket source Dispatch discovers work from is now configured with a `[tickets]` section (`source = "linear" | "local"`) rather than implied by the Agent Runtime, so an opencode pool can pull work through a configured source; `source = "linear"` is unchanged.
- Ingest the richer Claude/Codex lifecycle hooks: needs-input, a distinct quota-blocked state, and subagent/batch progress
- Send an opencode Follow-up through its session and answer a pending permission request from the Pool view
- Attention-first workspace list: a needs-you lane with a per-state glyph vocabulary, a reserved amber needs-you colour, and the idle lane folded by default
- Ticket discovery can now read the repository's own local markdown tickets with `[tickets] source = "local"`: it discovers `.scratch/<feature>/issues/<NN>-<slug>.md` ready tickets with no Agent Runtime, Linear, or Pi helper, resolves `Blocked by:` into the existing Dispatch Group, inlines the ticket body into the Worker prompt, and writes `Status: claimed` / `Status: resolved` back with a dated comment.
- Record each finalized Run's cost, usage, duration and turns in an append-only store at `.jj/pool/runs.jsonl`, so Run history survives past the next Run
- Run `jj fix` over a workspace's just-welded chain before the forge pushes, honouring the user's own `[fix.tools]`; a formatter that fails is a typed error, not a silent pass.
- Show the Pool's aggregate cost and usage in the Pool view and `jjfx status`, and gate Dispatch on an optional `[spend] cap_usd` budget with `block` or `warn`
- Configure an AI reviewer (`[review] reviewer = "coderabbit"`) and feed its normalised findings into the review prompt
- Bound the re-review loop with a pass cap, so `jjfx review --fg` iterates review -> address -> review without looping to zero
- Review feedback that belongs in a base commit now goes through `jj new <commit>` + `jj absorb` instead of a hand-split
- Add `jjfx absorb <worker>`, which folds a Worker's working-copy edits into their base commits and pushes the rewritten bookmark
- Flag migration-order inversions across the open workspaces with `jjfx check migrations` and a leading TUI notice: read each workspace's working copy for files matching a configurable `[migrations] pattern` (default `**/V*__*.sql`) and report two workspaces adding distinct versions (an inversion, listed in merge order) or the same version (ambiguous).
- Undo a jjfx-initiated Forge, weld, tidy or Reset from the tool: jjfx records the operation the repository was at before each mutation and restores it with `jj op restore` from `jjfx undo <worker>` or `u` in the Pool view, refusing when anything has happened since so a repository-wide restore cannot silently discard another Workspace's work
- Surface a stalled Run as stuck, and pace a Dispatch batch under an optional `[dispatch] max_concurrent` cap that holds the excess Tickets back
- Show the Tickets a capped Dispatch held back in `jjfx status` and the Pool view, from jjfx's own `.jj/pool/waiting.json`
- Surface a conflicted change chain as a needs-you Work state naming the commit that owns the conflict, and resolve it from the tool with `jjfx resolve <workspace>` (or `C` in the list)
- Run the repo's own build check between Weld and Push, before the forge pushes: configure a command and timeout with `[checks]` (`command`, `enabled`, `timeout_secs`) and a change that does not build against merged trunk fails the Check step, naming the command and its output. With no check configured the pipeline is unchanged; a check that cannot run (missing command, timeout) fails loudly rather than passing silently.
- A repository can now own its forge build check: a `[checks]` section in `<repo>/.jjfx/config.toml` replaces the user-global `[checks]` for that repository, so a build command that is a property of one repository no longer runs in every repository jjfx is pointed at. Any other section in that file is rejected, and a missing file leaves the user config in charge.

### Fixed

- The forge refuses to push a conflicted chain by name instead of silently excluding it from the push revset

## [0.54.0] - 2026-10-14

### Breaking

- Workspace existence and paths now come from jj rather than the ws-cache: each Workspace's recorded path is read from jj instead of guessed from a naming convention, and a name jj no longer reports is no longer listed as a Workspace.
- The `wsg` compatibility binary and its release archive are removed; `jjfx` is the only binary this repository builds and releases.

### Fixed

- Workspace rows now say why a path is unusable: a recorded path whose directory is gone names the path it was at, and a path jjfx never knew reads `(path unknown)`.
- A Worker Pool member whose Workspace directory is gone is now reported as a missing Workspace, instead of passing for a healthy idle Worker that can never be dispatched.
- The `list` command distinguishes a recorded path whose directory is gone from a path jjfx never knew, instead of calling both missing.
- Refreshing the workspace cache no longer records a naming-convention guess as if it were a known path, which could pin a Workspace to the wrong directory.
- The TUI title and footer, the Worker detail label, and the emitted zsh completion now name `jjfx` instead of `wsg`.

## [0.53.0] - 2026-10-14

### Added

- 2026-10-14 - The Go `wsg` project is deprecated: it was archived on
  2026-08-19, and jjfx is now the authoritative home of both binaries. `wsg`
  users should move to **jjfx 0.52.1 or newer**, which builds the frozen
  `wsg 0.19.0` compatibility binary from this repository. Existing Go releases
  stay available, but receive no new development.

- 2026-10-13 - A `jj` on `PATH` whose major/minor differs from the `jj-lib`
  version jjfx reads the store with is now reported instead of silently
  degrading: a warning line atop the world graph, a TUI footer warning, and a
  `warning:` on stderr for CLI repository commands. `jj-lib`'s target version is
  pinned in source and asserted against `Cargo.toml` by a test, so the check
  cannot rot when the dependency is bumped.

- Worker Dispatch keeps a Workspace that is open in a terminal out of Worker
  selection: an occupied Worker is no longer treated as idle, and Dispatch
  refuses it (implicit first-idle selection skips it, and an explicitly chosen
  occupied Worker is rejected). `jjfx dispatch` and `send` fail closed when they
  cannot see the open tabs, and `--force` overrides the check. `review` is exempt
  so a waiting Worker can still be told to address its pull request. The Pool
  view marks open Workspaces and the Dispatch preview flags an open target, and
  jjfx records the occupied Workers in `.jj/pool/in-use.json`.

### Breaking

- 2026-10-13 - A provider-reported Run cost that jjfx cannot represent (negative,
  non-finite, or out of range) is dropped instead of failing the Run. It
  previously finalised a *successful* Run as `failed` with "Process exited
  unexpectedly", which fed orchestration retries. `RunLogParseError::InvalidCost`
  is removed.

### Fixed

- 2026-10-14 - The terminal query no longer storms or blocks: `kitten @ ls` runs through a bounded, single-flighted query with a short cache, so a slow or stuck kitty yields an error within seconds instead of hanging, and concurrent callers share one in-flight query rather than spawning a process each. The fail-closed occupancy check still refuses dispatch when the terminal cannot be read.

- 2026-10-14 - Stopping an opencode Run now stops its work: `reset` and Worker `destroy` interrupt the Run's session through the daemon instead of signalling a client process, and a Worker's liveness is decided from that session's outcome - or a provider terminal event in its Run log - rather than from a client pid that outlives it. Falls back to the existing process-group path whenever the API is unreachable or the session cannot be identified, and a Run that cannot record a session forgets the one a previous Run left behind, so a stale id can never finish the wrong Run. A `destroy` consults that session only when the Worker's Run is opencode, so claude, codex and pi always signal the Run's own process group.

- 2026-10-14 - An opencode Run is anchored to its Worker Workspace explicitly: jjfx creates the session with that `location` and confirms what opencode stored before the Run starts, so a Run can no longer execute - and commit - in the wrong worktree. If the opencode API is unreachable the Run proceeds on the process cwd exactly as before, and an unreachable or older API never fails a Run.

- 2026-10-14 - Every Run now owns its process group, so a failed foreground launch and a cross-process Reset end the whole agent tree instead of leaking descendants that keep writing to the Workspace. A foreground Run's terminal is handed to it and back by a wrapper inside its own group, so it can still read the terminal and Ctrl-C still reaches the agent.

- 2026-10-14 - A recorded Run pid is treated as live only while it leads a process group, so a recycled pid can no longer keep a Worker `busy` forever or aim a Reset or destroy signal at an unrelated group.

- 2026-10-14 - A Worker Reservation is released only once its untracked Run is confirmed gone; when the forced kill cannot confirm the exit the Worker stays busy instead of being reused while an agent may still be running.

- 2026-10-14 - A change-requested Pull Request is no longer masked when newer
  work is stacked above it: the Workspace reads as `PrOpen { ChangesRequested }`
  and surfaces as needs-you, where it previously read as merely "ready to forge"
  and the change request was invisible. Approved and undecided PRs are still
  masked by newer work.

- 2026-10-14 - A dependent whose Blockers mix a merged one with a real stacked
  branch now bases its Pull Request on the first unmerged branch, rather than on
  whichever Blocker happened to sort first by ticket id. Previously a merged
  Blocker sorting first opened the dependent's PR against trunk, silently
  swallowing an unmerged prerequisite's diff.

- 2026-10-14 - An in-flight Dependency (`In Progress`, `In Review`) no longer
  satisfies its dependents: a Dependency must be completed, delivered, or
  cross-repository before its dependents become eligible. A group waiting on one
  is a quiescent, visible stall rather than a silent premature dispatch. A
  terminal status that was never delivered (`Canceled`, `Duplicate`) blocks too,
  as a `Failed` Dependency already did.

- 2026-10-14 - A Claude `result` event's reported `usage` now populates the Run
  result, matching Pi and Codex. It was previously dropped, so a Claude Run's
  result carried duration, turns and cost but no usage.

- 2026-10-13 - A jj read failure no longer reads as a plausible success. `tidy`,
  `tidyws` and `lift` treated a broken revset as "nothing to do" (the count
  returned 0 on any failure); a broken repository read as "no workspaces"; a
  failed read reported "0 behind"; a failed diff showed as an empty diff; and a
  failed push could read as a successful no-op. Each is now a typed error, and
  the work list shows an unknown row rather than a plausible zero. jj semantics
  are classified from machine-readable state (jj templates, `jj log --count`, the
  Git-format diff, and remote-bookmark snapshots) instead of jj's English CLI
  text.

- 2026-10-13 - The Worker Pool growth prompt states the growth ("Grow pool by
  3?") rather than a size it did not match: it printed `idle + shortfall`, but
  the pool grows by the shortfall from its *current* size, so a 4-worker pool
  with 2 idle and 5 tickets was promised as "Resize pool to 5" and became 7.


- 2026-10-13 - A missing config base directory now means "no user config" rather
  than a relative `.config/jjfx/config.toml` resolved against the process CWD,
  which let a repository plant the user's config. An empty `HOME` is treated the
  same way.

- 2026-10-13 - The store handed to the UI after a Workspace deletion keeps the
  configured colocation setting instead of defaulting it back to on.

- 2026-10-13 - `?` can be typed into the worker alias and the diff filter. It
  used to open the help overlay from every mode, so neither buffer could contain
  a question mark; the diff *pane* still opens help, so the detail bindings stay
  reachable.

- 2026-10-13 - Creating a Workspace from the TUI now uses the same on-disk rule
  as the CLI and the Worker Pool - `<parent>/<repo>-workspaces/<name>`, or
  `$JJ_WS_DIR/<name>` when that is set. It previously used
  `<parent>/<repo>-<name>`, so the same operation landed in two different places
  depending on which frontend ran it. Existing Workspaces keep their real paths
  (they are cache-preserved), so only newly created ones move.

- 2026-10-13 - A diff longer than 65 535 lines scrolls to its end again. The
  diff and world-graph viewports recorded the content length as a `u16`, so the
  line count wrapped: a 70 000-line file could only be scrolled to line 4 464,
  and a file of exactly 65 536 lines could not be scrolled at all. The offset
  arithmetic saturates now as well, so the degenerate zero-height pane can no
  longer overflow it.

- 2026-10-13 - The terminal is restored when jjfx is killed by a signal. SIGINT,
  SIGTERM, and SIGHUP used to terminate the TUI with the alternate screen and
  raw mode still active, leaving the shell echoing nothing until `reset`. The
  panic hook is now installed before the terminal is touched too, so a panic
  during setup is restored as well.

- 2026-10-13 - A failed terminal input read no longer kills the input reader
  silently. An interrupted read (a signal arriving mid-read) is retried, and any
  other failure pins a footer message saying input has stopped - previously the
  thread exited on the first error and the UI kept redrawing while ignoring
  every key.

- 2026-10-13 - The atomic-replacement and file-lock primitives are now one
  implementation (`engine/atomic.rs`) instead of two divergent copies in the
  state repository and the Worker Workspace cache. The ws-cache write now
  honours the documented contract: a uniquely named temporary file at mode
  `0644`, synced and renamed, and removed if it fails - it previously used a
  predictable `ws-cache.<pid>.<thread>.tmp` that leaked on failure and could
  collide between processes.

- 2026-10-13 - The TUI no longer does needless work every frame and every tick:
  an idle screen stops emitting animation ticks (it used to wake ~7 times a
  second and re-canonicalise every workspace path), the world graph is laid out
  once per frame instead of twice, and agent lookups are hash probes rather than
  a `canonicalize` syscall per workspace.

- 2026-10-13 - Workspace names are quoted wherever they are interpolated into a
  jj revset, so a name containing a space or a revset metacharacter (`my work`)
  no longer fails to parse - which read as "no chain", left `behind` at 0, and
  made `lift` a silent no-op. The workspace list is also read through
  `stringify(name)`, so a name that needs quoting comes back raw instead of
  already-quoted and still matches its ws-cache entry.

- 2026-10-13 - The Workspace list is re-projected on a background worker. The
  filesystem watcher fires a reload on every repository change and the
  projection shells out to `jj workspace list`, so a reload no longer blocks the
  input fold. The store swap is now eventually consistent, and the
  create-then-open path waits for the fresh projection to decide whether the
  Workspace survived setup.

- 2026-10-13 - Opening or focusing a Workspace tab, and the `t`/`T` tidy sweeps,
  now run as background effects instead of inside the input fold. A slow
  `kitten @ ls` (which can sleep ~5s while a target starts) or a `jj rebase` no
  longer freezes the TUI.

- 2026-10-13 - A failed `gh pr list` is no longer reported as "no Pull
  Requests". The read now surfaces the failure, and a pushed Workspace whose
  Pull Request state cannot be read shows as unknown instead of claiming it
  still needs a forge. A local-only change stays dirty.

- 2026-10-13 - Dispatch occupancy is now a live query: the orchestration runner
  asks a provider for the excluded Worker Workspaces on every claim, so a
  Workspace a human opens (or closes) while a Dispatch Group is running changes
  the next decision instead of the run's opening snapshot (ADR-0011).

- 2026-10-13 - A `.cleanup` marker whose name is not a valid Worker id no longer
  aborts cleanup discovery, which previously broke Pool resize and destroy for
  every Worker because of one junk file.

- 2026-10-13 - A Ticket can no longer be dispatched twice: Ready Ticket
  discovery drops a repeated Ticket with a diagnostic, and a Dispatch batch
  naming the same Ticket twice is rejected before any Worker is reserved.

- 2026-10-13 - The lifecycle event log is now read line-by-line and lossily, so
  one torn line (a rotation slice cut mid-character) no longer wipes every
  agent's state. The tailer starts from the replay's own offset, so a hook line
  appended during startup is neither lost nor replayed twice. The log is also
  rotated while the TUI runs, so a long-lived session bounds `events.jsonl`, and
  a rotated shrink is skipped rather than re-emitted as duplicates.

- 2026-10-13 - A failed Dispatch Group retry now persists the group before
  resetting the Worker, matching the Done path. A save conflict can no longer
  leave a cleared Worker against a `dispatched` group, which made a restarted
  parent unrecoverable with `WorkerTicketMismatch`.

- 2026-10-13 - Forge no longer reports failures as clean skips: a failed
  `jj git push` fails the Push step (with jj's own message) instead of reading as
  "nothing to push", a weld that buries a conflict below `@` stops the pipeline
  before pushing, and a failed `gh` default-branch lookup is surfaced instead of
  reading as "no origin remote".

- 2026-10-13 - Dispatch prompts now inline untrusted Linear Ticket text (title,
  description, and blocker titles) inside a labelled data fence, with control
  characters stripped and length capped, and the Repository slug is validated to
  GitHub's character set. A Ticket can no longer smuggle instructions or a
  forged fence terminator into the agent's prompt, nor inject shell syntax into
  the interpolated `gh` command.

- 2026-10-13 - `send`, `review`, and `rebase` now take the Worker's
  Workspace-operation lock, matching Dispatch and Reset. A Follow-up can no
  longer start editing a working copy that a Reset is still restoring.

- 2026-10-13 - Resuming a Dispatch Group no longer rewrites a stacked
  dependency onto `main` when the operator's `jj user.name` is not `adam`.
  Branch resolution now matches on the Ticket id instead of a hardcoded
  namespace, so a dependent keeps its stacked base and its pull request is based
  on the prerequisite rather than pulling the prerequisite's diff in.

- 2026-10-13 - Pressing `?` over the diff-detail or full-screen graph view now
  shows the keybindings overlay instead of freezing the view. The overlay was
  opened from those modes but never drawn, and it swallowed every key until it
  was dismissed.

- 2026-10-13 - Resizing the Worker Pool down no longer risks stranding a Worker.
  The cleanup marker is now written after the Pool manifest, so a failed manifest
  write cannot leave a member carrying a marker that made every later Run on it
  fail to start.

- 2026-10-13 - A Dispatch Group whose dependency fails now reaches a terminal
  state instead of being watched forever. A `Failed` Sub-issue no longer leaves
  its dependents permanently `pending`, so `jjfx dispatch`/`orchestrate` and the
  TUI stop polling a group that can make no further progress, and the summary
  and Pool view report the unreachable Sub-issues as `blocked`.

- 2026-10-13 - Running `jjfx` from a secondary Jujutsu workspace no longer
  shadows the `default` workspace. The repository root is now normalised to the
  default workspace at discovery, so `default` always points at the repository
  and the store, filesystem watcher, and work poller all agree.
- The Workspace projection no longer snapshots the working copy, so the TUI stops re-triggering its own .jj watcher in a reload loop that stormed jj and kitten @ ls
- Dispatch identity checks no longer snapshot the working copy: the engine's git-remote read is pure, so dispatching stops committing the working copy as a side effect

## [0.52.1] - 2026-10-12

### Added

- 2026-10-12 - `jjfx` is now a single crate and publishes to crates.io:
  `cargo install jjfx` builds the binary from the registry. The former
  `wsg-core` crate was absorbed into jjfx as its `engine` module, so the `wsg`
  compatibility binary now builds from this repository alone (it is not
  published). (jjfx 0.52.0)

- 2026-10-12 - A `jjfx-dispatch` agent skill under `skills/`, installable into
  any agent with `bunx skills@latest add Jarvvski/jjfx`. It walks an agent
  through giving the Worker Pool capacity, choosing tickets, dispatching,
  watching, steering, landing, and reclaiming. (jjfx 0.51.0, wsg 0.19.0)

- 2026-10-12 - New workspaces are now colocated with Git: `jj workspace add`
  creates a Git worktree so each agent workspace has a working `.git`, which
  lets tools such as `gh stack` run inside it. Controlled by the new top-level
  `colocate` setting, on by default. (jjfx 0.51.0, wsg 0.19.0)

- 2026-10-12 - `jjfx colocation <enable|disable|status>` converts an existing
  repository to Git colocation in place through `jj git colocation`, so it can
  be switched (or checked) without hand-running jj. The decision is recorded in
  `docs/adr/0010-colocate-workspaces-with-git.md`. (jjfx 0.51.0, wsg 0.19.0)

- 2026-10-08 - `PageUp` and `PageDown` now jump the workspace-list selection
  straight to the first or last workspace (jjfx 0.50.0).

- 2026-09-18 - Window layouts are now configurable per display: named pane
  layouts under `terminal.layouts` are selected by `terminal.layout_by_display`
  (built-in display vs external monitor attached), and both workspace tabs and
  worker `mount` build from the selection. `JJFX_LAYOUT` forces a named layout.
  Without configuration jjfx still builds the existing 19% shell column with the
  agent filling the rest (jjfx 0.48.0, wsg 0.18.0).

- 2026-09-11 - The opened tab's first (top-left) pane can now run a configured
  command - e.g. a dev server or watcher - via the new
  `terminal.first_pane_command` setting, in both the TUI and `mount`. It runs
  in the login interactive shell and drops back to a shell when the command
  exits; unset leaves that pane a plain shell (jjfx 0.47.0, wsg 0.17.0).

- 2026-09-11 - Added OpenCode as a Worker Pool runtime with JSON Run logging,
  session resume, Linear discovery, interactive workspace lifecycle tracking,
  and an idempotently installed OpenCode lifecycle plugin (jjfx 0.46.0,
  wsg 0.16.0).

- 2026-09-01 - Workspace creation and removal now run repository-local
  `.jjfx/setup.sh` and `.jjfx/teardown.sh` hooks across CLI, TUI, and Worker
  Workspaces, with transactional setup and best-effort teardown (jjfx 0.45.0,
  wsg 0.15.0).

- 2026-08-25 - Worker task Send now opens a full-screen multiline editor with
  line numbers, natural editing controls, Shift+Enter newlines, bracketed
  paste, and an any-idle-Worker shortcut (jjfx 0.44.0).

- 2026-08-20 - New TUI workspaces now start from jjfx's canonical trunk
  revision instead of inheriting an unrelated `default@` parent, while the
  existing CLI revision option remains unchanged (jjfx 0.43.0).

- 2026-08-19 - Workspace deletion now runs in the background, keeping the TUI
  responsive for navigation and operations on other workspaces while the
  deleting row remains visible and disabled (jjfx 0.43.0).

- 2026-08-18 - New workspace configuration now reports elapsed time and the
  latest setup output in the footer, while the default setup hook only trusts
  the project's `mise.toml` and defers heavyweight dependency installation
  until it is needed (jjfx 0.42.0).

- 2026-08-17 - Redesigned workspace tabs in jjfx 0.41.0 with a full-width shell
  across the top 35% and an evenly split agent and shell across the bottom 65%.
  Every pane starts in the workspace directory, and focus finishes on the agent
  pane. The fixed layout requires kitty 0.36.0 or newer for `launch --bias`.

- 2026-08-17 - Completed persistent Pi Dispatch integration in jjfx 0.40.0 and
  wsg 0.14.0. Repository profiles now carry runtime, provider, and model through
  Dispatch Group restarts, Reservations, Worker Runs, detached orchestration,
  Follow-ups, and Mounts without falling back to another runtime. The shared
  CLI adds atomic `pool profile` configuration plus provider-aware dispatch,
  help, completions, and runtime output. The jjfx Pool view shows
  provider-neutral runtime capability, progress, activity, failures, and
  terminal outcomes while keeping provider and adapter details out of the TUI.

- 2026-08-16 - Added the preflighted Pi Direct Dispatch profile in jjfx 0.39.0
  and wsg 0.13.0. Provider-qualified models now reach Dispatch and
  orchestration, while a pinned `pi-mcp-adapter` 2.11.0 profile validates the
  exact direct Linear tools and schemas before Worker, Pool, assignment, or
  Workspace mutation. Pi Runs and Follow-ups explicitly disable inherited
  resources and approvals, retain a fixed coding-tool policy, and reject
  unsupported budgets without weakening the delivery contract or changing
  Claude and Codex behavior.

- 2026-08-15 - Added Pi 0.84.x interactive lifecycle tracking in jjfx 0.38.0.
  A jjfx-owned Pi extension now reports explicit session identity and
  working, waiting, and graceful shutdown events through the shared JSONL
  lifecycle log. Hook installation preserves unrelated Pi state, the TUI uses
  distinct violet Pi styling, and unsupported permission attention is never
  fabricated.

- 2026-08-14 - Added read-only Pi Ticket discovery in jjfx 0.37.0 and wsg
  0.12.0 through a configured, versioned helper protocol. Ready Ticket and
  dependency queries now use the existing typed validation and single retry
  without starting Pi sessions, reserving Workers, exposing credentials, or
  falling back to Claude or Codex.

- 2026-08-14 - Added Pi 0.84.x Worker Follow-up and interactive Mount support
  in jjfx 0.36.0 and wsg 0.11.0. Pi actions use explicit provider/model
  profiles, repository-owned sessions, and a fixed trusted Worker policy;
  filesystem sandboxing, aggregate budgets, approval dialogs, ticket
  discovery, and interactive lifecycle tracking remain separate capabilities.

- 2026-08-13 - Persisted the TUI's idle workspace fold state alongside the
  world-graph pane across launches.

- 2026-08-13 - Consolidated the user-facing command surface into the `jjfx`
  binary. Bare `jjfx` prints CLI help, `jjfx tui` opens the interactive TUI,
  and existing workspace, Pool, dispatch, and Worker actions are available as
  `jjfx` subcommands. The internal `wsg` target remains for migration
  conformance.

- 2026-08-12 - `wsg` no-argument interactive startup now enters the shared
  jjfx TUI, while non-TTY command behavior and independent version output remain
  unchanged in jjfx 0.34.0 and wsg 0.10.0.

- 2026-08-11 - Added Worker Rebase, Pull Request, alias, and confirmed Dismiss
  actions to the jjfx Pool view while preserving stable Worker identities and
  Go-compatible terminal lifecycle behavior.

- 2026-08-11 - Added confirmed Worker Reset with independently reported
  Workspace restoration outcomes in the jjfx Pool view.

- 2026-08-11 - Added non-blocking Worker Send and Review actions to the jjfx
  Pool view. Follow-up outcomes show resumed Agent Sessions or the reason a
  fresh Session was required.

- 2026-08-10 - Added Dispatch Group progress, non-blocking Parent Ticket
  orchestration, and focused provider-neutral Worker logs to jjfx 0.32.0.
  Dependency waves, retries, usage, terminal outcomes, and unavailable logs
  remain visible without blocking Pool input.

- 2026-08-06 - Added selected-Worker, bulk, and Ready Ticket Dispatch to jjfx
  0.31.0. Ticket previews, typed capacity confirmation, ordered outcomes, and
  background execution keep the TUI responsive while reusing shared wsg-core
  rules.

- 2026-08-06 - Added writable Worker Pool management to jjfx 0.30.0 through
  an asynchronous Workspace Dispatch controller. Pool capacity changes,
  confirmed shrink and destruction, Worker diagnostics, and stable Worker
  metadata now work without replacing the existing Attention lifecycle.

- 2026-08-06 - Restored the wsg Dispatch, orchestration, Follow-up, Worker
  action, completion, and safe default command surfaces over shared typed Rust
  interfaces. The compatibility binary is now wsg 0.9.0.

- 2026-08-05 - Restored the compatible Workspace and Worker Pool command surface
  in the Rust `wsg` binary, including independent versioning (`wsg` 0.8.1 and
  `jjfx` 0.29.0) without changing the installed Go binary. An isolated
  `mise run wsg-test-install` task places the Rust binary under `target/`
  without touching the user's PATH.

- 2026-07-21 - Added read-only Worker Pool snapshot import to jjfx, including
  compatibility diagnostics, Worker metadata rows, and a visible read-only
  indicator (v0.28.1).

- 2026-07-21 - Added the shared Workspace Dispatch Rust foundation and a
  repository-independent `wsg` help/version skeleton (v0.28.0).

- 2026-07-20 - Configurable new-workspace setup (v0.27.0): set
  `[workspace] on_create = ["mise", "trust"]` to run a program and its
  arguments from each workspace created through `n`. jjfx runs it in the
  background before opening the tab, preserves the workspace on failure, and
  opens and focuses the agent pane only after setup succeeds.

- 2026-07-16 - Codex and claude wear their own colours (v0.24.0): the agent
  column now names the agent (`claude` / `codex`) while a session is live,
  derived per event from the payload's `transcript_path` (`~/.claude/` vs
  `~/.codex/` - no hook-config change needed). Each gets its own working
  animation: claude keeps the orange jj-wsx petals, codex fills a cyan
  hexagon slice by slice (nerd-font `md-hexagon_slice_1..6`, so a
  nerd-font-patched terminal font is assumed). Waiting/blocked marks are also per-agent
  (claude `✻`, codex an outline hexagon), coloured by state (yellow/red),
  and workspaces with no live session show `· -`.

- 2026-07-16 - Animated agent status glyphs (v0.23.0): each workspace row now
  carries a one-glyph agent status ahead of the label, hook-driven for both
  claude and codex. While a turn is in progress the glyph plays jj-wsx's
  "claude is working" animation - petal frames `❀ ✼ ✴ ✳ ✛ •` bounced at 150ms
  in Claude's spinner orange (the working label turns orange to match); a
  waiting agent shows a static yellow `✻`, one blocked on a permission a red
  `✻`, and no live session a dim `·`. The animation repaints only while an
  agent is actually working and the list is on screen.

- 2026-07-14 - Codex lifecycle tracking (v0.21.0): `jjfx hooks install` now
  also installs the append hook into codex's `~/.codex/hooks.json` (same
  events log, same payload shape), so codex sessions drive the agent status
  column exactly like claude ones. `jjfx hooks status` reports both agents.
  Codex has no `SessionEnd` hook, so a closed codex session shows `waiting`
  rather than `ended`.

- 2026-07-14 - Fetch keybinding (v0.18.0): `u` runs `jj git fetch` on a
  background task and reports the outcome in the footer. Remote-only changes -
  a PR merged on GitHub, its head branch deleted - now reach the work rows
  without running a full forge: fetching advances `trunk()` and propagates
  remote branch deletions, which is what lets a stale "merged" row resolve
  back to clean.

- 2026-07-14 - Inline world graph (v0.17.0): `w` on the home view now toggles a
  world-graph pane underneath the workspace list, so you can watch trunk and
  every workspace's chain without leaving the list. The pane sizes itself to the
  graph (capped at half the body, dropped on very short terminals), highlights
  the selected workspace's chain as you move with `j`/`k`, and scrolls with
  `J`/`K`. The full-screen world graph moved from `w` to `W`. The toggle
  persists across launches in a new jjfx-owned state file,
  `${XDG_STATE_HOME:-~/.local/state}/jjfx/ui.toml` (your `config.toml` is never
  written).

- 2026-07-09 - Native pull requests (v0.16.0): the forge's final step now opens
  and maintains PRs itself over `gh`, replacing the external `jj-spr` shell-out -
  jjfx now depends only on `jj` and `gh`, nothing else on your `PATH`. For each
  bookmark on a workspace's chain it creates a draft PR (base = the nearest open
  bookmark below it in the stack, or the repo default branch), titled from the
  commit description, then rewrites each PR body's `## Stack` navigation section
  (current PR flagged, merged ones struck through). A new `[forge]` config section
  tunes it: `pull_requests = false` stops the pipeline after push so you open PRs
  yourself, and `draft = false` opens new PRs ready for review instead of as
  drafts. Both default on.

- 2026-07-09 - Target terminal (v0.14.0): a new config file,
  `${XDG_CONFIG_HOME:-~/.config}/jjfx/config.toml`, lets you host workspace
  sessions in a *different* kitty instance than the one jjfx runs in. Set
  `[terminal] listen_on` to that instance's `listen_on` base (e.g.
  `unix:/tmp/kitty-visor`) and jjfx routes every `kitten @` call there via
  `--to`, resolving kitty's live pid-suffixed socket (`/tmp/kitty-visor-<pid>`)
  from the base at call time so it survives restarts. Set `launch_command`
  (program + args) and jjfx runs it to start the target when its socket isn't
  found, then waits for it to answer. With no config jjfx behaves exactly as
  before, driving the surrounding kitty. A malformed config is reported at
  startup before the TUI loads; a missing one is fine.

- 2026-07-09 - Version flag (v0.13.0): `jjfx --version` (or `-V`) prints
  `jjfx <version>` and exits without discovering a jj repo or opening a terminal,
  so it is safe to run anywhere and lets `release`/CI smoke-test the built binary.

- 2026-07-08 - Lift onto trunk (v0.12.0): press `r` to rebase the selected
  workspace's own stack onto the current trunk, or `R` to lift every workspace at
  once - a local rebase, no push, that works whether the workspace is empty or
  carries work. This is the remedy for a `↓N` "behind" workspace: it rebases onto
  `TRUNK_BASE` (the same base `behind` and the dirty/clean check use), so lifting
  zeroes the `↓`. `tidyws` (`t`) now rebases onto that same `TRUNK_BASE` too, so it
  also clears the indicator; forge is unchanged (it still welds onto the remote
  mainline for clean PRs). Unlike `tidyws`, which only touches idle *empty*
  workspaces, `r`/`R` lift a workspace regardless of its contents.

- 2026-07-08 - Commit graph (v0.11.0): press `w` for a full-screen "world" graph -
  `trunk()` plus every workspace's chain, each commit shown with its change id,
  summary, and bookmarks, and the selected workspace's chain highlighted.
  Recently-moved commits are shaded brighter (freshness). The per-workspace
  detail view (`→`/`l`) gains a graph strip on the right showing just that
  workspace's chain from `trunk()` up to `@` plus one commit beyond it; the strip
  is dropped automatically on narrow terminals so the diff stays readable. The
  graph is read directly from the on-disk jj store via `jj-lib` (not by scraping
  `jj log`), and refreshes on its own as revisions change (new commits, fetch,
  forge). `j/k`/`PgUp`/`PgDn`/`g`/`G` scroll the world graph; `esc` closes it.

- 2026-07-08 - Diff detail view (v0.10.0): press `→`/`l` on a workspace to open a
  full-screen, two-pane detail - a changed-file list with per-file `+`/`-`
  magnitude bars on the left, the selected file's diff from `trunk()` on the
  right. The diff is highlighted in-process with `syntect` (no `bat` process),
  with the `+`/`-` gutters preserved and unknown languages degrading to plain
  text. Type to fuzzy-filter the file list; `↑`/`↓` pick a file; `→`/`tab` focus
  the diff and `j/k` + `PgUp`/`PgDn` scroll it; `esc` returns to the list. The
  diff is read on a background thread, and highlighting is incremental - each
  file is syntect-highlighted only as far down as the viewport has scrolled, so
  switching between files (or opening a large diff) never highlights the whole
  patch up front and navigation stays responsive.

- 2026-07-08 - Help overlay (v0.9.0): press `?` for a centered, bordered
  keybindings menu (action left, key right) drawn over the dimmed list; `?` or
  `esc` closes it. The footer no longer carries the full key list - in normal
  mode it now shows only `j/k move  ? help  q quit`.

- 2026-07-08 - Forge pipeline (v0.7.0): drive a workspace toward merge from the
  list. `f` forges the selected workspace, `F` forges every eligible one, and `g`
  forges the default - each running fetch -> weld -> push -> spr natively (ADR
  0005) with the workspace-safe revsets ported from `jj-forge` (weld scoped to
  `::@`, push excluding `trunk()`/`conflicts()`, `jj-spr` handed a scoped
  `JJ_SPR_REVSET`). Every mutating step runs in the workspace's own directory, so
  forging one workspace never rebases another's chain. The pipeline runs on a
  background task and streams real step state (`⚒ f✓ w✓ p… s·`) onto the row - not
  scraped stdout; a clean run clears the overlay and the work axis advances. A
  conflicted working copy is skipped with a visible reason, and a locked GPG
  signing key is detected up front and aborts cleanly (no pinentry inside the
  alt-screen) rather than corrupting the terminal. The `jj-forge` bash tool is
  untouched and still usable standalone.

- 2026-07-08 - Maintenance: tidy, tidyws, and the behind indicator (v0.6.0):
  native versions of the two maintenance aliases (ADR 0005). `t` runs `tidyws` -
  rebasing every idle, empty, undescribed workspace working-copy onto latest
  `trunk()` (non-destructive, no confirmation); `T` runs `tidy` after a
  confirmation - abandoning junk mutable empties (undescribed, unbookmarked,
  untagged, never `@`). Both report how many changes they touched and are no-ops
  when nothing is eligible. Each row now also shows how far behind `trunk()` its
  base is (`↓N`, highlighted once far enough behind to warrant a reset). The
  proven revsets are ported from the `jj tidy` / `jj tidyws` aliases, which
  remain untouched and usable standalone.

- 2026-07-08 - Attention triage (v0.5.0): the list is now organized around the
  derived Attention badge (ADR 0008). Each workspace shows one signal - needs
  you / working / ready to forge / idle - derived from its (agent, work) pair,
  and the list is grouped in that order with the idle group foldable (`c`).
  needs-you distinguishes a Waiting agent over a changes-requested PR from an
  idle Clean workspace. A live state change (agent Stop, PR review) re-sorts the
  workspace into the right group with no manual refresh; selection follows a
  workspace by name as it moves.

- 2026-07-08 - Workspace actions (v0.4.0): drive workspaces from the list -
  `n` creates one (a new `jj` workspace in a `<repo>-<name>` sibling dir,
  persisted to the ws-cache) and opens a kitty tab running claude beside a
  shell; `enter` focuses its tab (or opens one), `o` opens in the background
  without stealing focus, and re-opening focuses rather than duplicating; `d`
  deletes after a confirmation (closing the tab and forgetting the workspace),
  and the default workspace is undeletable. All terminal control goes through a
  `Terminal` trait (kitty-only for now), so the multiplexer is swappable.

- 2026-07-08 - Work lifecycle (v0.3.0): each workspace row now also shows its
  work state - clean / dirty (with +/- LOC from trunk) / pushed / pr#N (with
  review verdict) / merged. jj state is read via CLI `-T` revsets relative to
  whatever `trunk()` resolves to (never assumed `main`); PR state comes from
  `gh --json`, with the PR derived by matching its head branch to a bookmark on
  the workspace's own change chain. A background poller refreshes every 15s and
  on any repo change; a missing `gh` or jj read degrades the row to unknown
  rather than crashing.

- 2026-07-08 - Agent lifecycle (v0.2.0): each workspace row now shows its live
  agent state - working / waiting / needs-attn / ended - event-sourced from
  Claude Code hooks. `jjfx hooks install` adds a dumb append-only hook to
  `~/.claude/settings.json` (idempotent, non-destructive) that appends each
  event to a global JSONL log; `jjfx hooks status` reports whether it is
  installed. The TUI replays the log on startup to reconstruct state, tails it
  for live transitions keyed by `cwd`, and bounds log growth with size-based
  rotation.

- 2026-07-08 - Walking skeleton: `jjfx` launches in a jj repo and renders a
  keyboard-driven workspace list (default + named). It reconciles the
  authoritative in-memory store from jj plus `.jj/ws-cache`, writes the cache
  through atomically (`name\tpath`) so the bash tools stay consistent, and
  watches `.jj/` so a shell-created workspace appears without a restart. `q`/esc
  quit and a restore-on-panic guard keeps the terminal intact. `--list` dumps the
  reconciled store headlessly.

### Changed

- 2026-10-07 - Lifting a workspace (`r`) and lifting all (`R`) now run in the
  background through a serial queue instead of blocking the UI, so several
  rebases can be kicked off in a row. Rows show `queued`/`lifting...` while their
  job is pending, and the footer tracks queue depth and failures (jjfx 0.49.0).

- 2026-09-11 - Redesigned the window-opening layout: opening a workspace and
  Mounting a Worker now both build a 19% shell column on the left, split into
  two stacked shells, with the agent taking the remaining 81% width, replacing
  the previous top-shell and three-pane layouts. Every pane still starts in the
  workspace directory and focus finishes on the agent pane (jjfx 0.47.0,
  wsg 0.17.0).

- 2026-07-17 - Agent launch uses one shell command (v0.26.0): configure
  `[agent] command = "cx"` to run any coding agent command, including aliases
  and commands with flags. It always runs through `$SHELL -l -i -c`, with
  `"claude"` as the default. Breaking: `[terminal] agent`, `[claude]`, and
  `[codex]` are removed; replace them with the single `[agent]` section.

- 2026-07-14 - World graph shows the connected trunk spine (v0.22.2): instead
  of eliding trunk history down to branch points, the world view now renders
  every trunk ancestor as a connected `◆` mainline (the `::trunk()` the
  owner's `jj log` shows), with each mutable fragment attached beside its
  branch point. Ordering now mirrors jj's `log-graph-prioritize`: the default
  workspace's chain leads, then the other workspaces, then fragments grouped
  where they branch - no more free-floating hooks between fragments.

- 2026-07-14 - World graph hides immutable history like jj does (v0.22.1):
  the world view's mutable set now matches jj's `immutable_heads()..` exactly -
  ancestors of trunk, tags, and untracked remote bookmarks (other people's
  branches) no longer render as orphaned mutable fragments. Previously the
  boundary was trunk-only and capped at 1,000 commits, so on a large repo
  thousands of already-landed commits leaked into the view as disconnected `○`
  chains; the immutable walk is now sized for whole-repo history (50k cap).

- 2026-07-14 - World graph is now the real jj log DAG (v0.22.0): the world
  view (full-screen `W` and the inline `w` pane) lays commits out like
  `jj log` - one topologically ordered graph with fork/merge connectors and
  elided history (`~`), drawn by sapling-renderdag (the renderer jj's own
  CLI uses) - instead of stacking each workspace's chain under a name header
  with the trunk floating disjointedly above. Every workspace's `@` is
  badged `name@`; the selected workspace's chain stays highlighted.

- 2026-07-14 - Selectable coding agent (v0.20.0): a new `[terminal] agent`
  config key picks which agent a workspace tab's left pane runs - `"claude"`
  (the default) or `"codex"`. Each agent has its own top-level section
  (`[claude]` / `[codex]`) whose `command` overrides the default login-shell
  wrap of the agent's binary. Breaking: `[terminal] claude_command` is
  removed - a config still setting it fails at startup naming the key; move
  the value to `command` under `[claude]`.

- 2026-07-14 - Footer status messages expire (v0.19.0): the transient result
  line after an action ("lifted feat onto trunk", "fetched", ...) now clears
  itself after 5 seconds instead of sitting in the footer until the next
  action. In-flight progress ("fetching…") still stays until its outcome
  arrives.

- 2026-07-09 - Workspace tab layout (v0.14.0): opening a workspace now builds
  three panes - claude on the left, and a right column split into two stacked
  shells - matching the `jj-wsx` layout. The splits are anchored to their window
  ids so they land correctly even when the tab is opened in the background, which
  now uses `--dont-take-focus` (so a background open never raises the target) and
  then focuses the claude pane.

- 2026-07-08 - Minimal restyle of the workspace list (v0.8.0): dropped the box
  border and the full-width reversed selection bar - the latter inverted every
  padded, coloured column into a solid block. Rows now carry a dim `·` bullet
  (the selected row a bright `▸`), only the selected name is boxed, and the
  attention column is padded past its widest heading so the columns line up
  across every row. Calmer and closer to a plain, minimal list.

### Fixed

- 2026-10-12 - Starting the Pi ticket discovery helper now retries briefly
  while the executable is momentarily open for writing, instead of failing the
  discovery with `Text file busy` (ETXTBSY). A helper being replaced while jjfx
  runs, or written by a concurrent process, used to fail the query as a setup
  error. (fix)

- 2026-10-12 - Pi ticket discovery now stops a helper that overruns its
  configured timeout, along with anything the helper spawned. Only the helper's
  own process used to be signalled, so a helper that had forked a long-running
  child kept the discovery command's output pipes open and the timeout did not
  bound the call. (fix)

- 2026-10-12 - Dispatching to a runtime that cannot read its Ticket no longer
  launches a Worker that stops immediately. jjfx fetches the Ticket's title and
  description through the pool runtime and inlines it in the Dispatch prompt,
  failing before any Pool or Workspace mutation when the runtime cannot reach
  Linear; status and comment updates are best-effort, and Pi-only tool guidance
  is no longer sent to Claude, Codex, or OpenCode. (fix)

- 2026-10-12 - `jjfx pool list` (and `status`) now show a failed Worker's reason,
  exit code, and Run-log path, with a `--json` form carrying the same fields;
  `jjfx logs <worker>` prints the Run log tail immediately and exits for a
  terminal Worker instead of following silently. (fix)

- 2026-10-12 - `jjfx path <short-id>` resolves the real `worker-<id>` Workspace
  instead of a non-existent path, `jjfx pool --help` exits 0 with usage, and
  top-level `jjfx --help` works outside a jj repository. (fix)

- 2026-10-12 - New workspaces no longer pass `--colocate` to a jj older than
  0.46: jjfx probes `jj --version` and either omits the flag or fails with a
  clear version error instead of jj's raw `unexpected argument`. (fix)

- 2026-10-09 - Workspace creation (`n`) now reports the real failure and cleans
  up after itself: a failed `jj workspace add` no longer leaves a stray
  `<repo>-<name>` directory behind, a create-step failure says `could not create
  '<name>'` instead of the misleading `created '<name>', setup failed`, signing
  failures surface an actionable GPG hint, and the full error text is appended to
  `${XDG_STATE_HOME:-~/.local/state}/jjfx/setup-errors.log` so a footer-truncated
  failure stays inspectable (jjfx 0.50.1).

- 2026-09-18 - Returning to an already-open Workspace now focuses its agent
  pane rather than just its tab. Panes are tagged with their role at launch
  (`jjfx_role`) so the agent can be found again, falling back to the configured
  agent command for tabs opened before the tag existed (jjfx 0.48.1).

- 2026-09-11 - The TUI now reads its Workspace list through the same
  cache-backed projection as the CLI and Worker Pool, so a Workspace whose
  directory is absent has no usable path: `jjfx --list` prints an empty path
  and forge or terminal actions refuse it instead of targeting a missing
  working copy. `wsg refresh` preserves cached ad-hoc Workspace paths instead
  of rewriting them to the named-Workspace convention (jjfx 0.46.1,
  wsg 0.16.1).

- 2026-08-13 - Home workspace rows no longer add placeholder-only Worker
  metadata lines for idle, unassigned Workers. Optional runtime, ticket, and
  activity fields are omitted when unavailable.

- 2026-07-27 - Worker Pool snapshots now read the actual Go wsg state layout
  (v0.28.2), including `.jj/pool/<worker>.json`, aliases, Agent Runtime,
  completion metadata, and partial diagnostics. Shared Pool, Worker, and
  Dispatch Group repositories now preserve compatible state and serialize
  concurrent Go and Rust writers through the same sidecar locks.

- 2026-07-20 - Workspace rows show the least-delivered change (v0.27.1): a
  local change now remains dirty, and a pushed change remains pushed, when a
  lower change already has an open PR. PR-only stacks show the lowest PR with
  the most blocking review verdict instead of whichever PR was returned first.

- 2026-07-17 - Default workspace stays pinned at the top (v0.25.2): `default`
  now remains above every attention group regardless of its current lifecycle.
  Folding the idle group keeps it visible, and initial selection starts there.

- 2026-07-17 - Completed forge pipelines no longer remain in workspace rows
  (v0.25.1): a skipped push or Pull Request step left its final progress
  snapshot visible forever. Finished pipelines now return to the normal work
  status while the skip reason remains briefly in the footer.

- 2026-07-16 - Needs-you no longer sticks after a permission is resolved
  (v0.25.0): no hook fires when a permission dialog is approved/denied, so a
  row stayed red until the turn's final Stop even though the agent had long
  resumed. The hook set now also registers `PostToolUse` (both agents), and
  a tool completing while a workspace is needs-attention folds it back to
  working. Rerun `jjfx hooks install` to register the new event; running
  agent sessions pick it up after a restart, and codex will re-prompt for
  hook trust.

- 2026-07-14 - Quitting no longer hangs on an in-flight poll (v0.17.1): pressing
  `q` used to leave the process lingering until any mid-flight `gh`/`jj` work
  snapshot finished, because dropping the tokio runtime joins every blocking
  task. The runtime now shuts down in the background on exit, so quit returns
  the prompt immediately.

- 2026-07-09 - Merged PRs read as merged in the work list (v0.16.1): a workspace
  whose PR `gh` reports with a `mergedAt` timestamp but a non-`MERGED` state (e.g.
  a squash-merge recorded as closed) now shows "merged" instead of falling back to
  "pushed". The work-list overlay and the forge's stacked-PR submission now share
  one PR model with a single mergedness rule (`state == "MERGED"` or a set
  `mergedAt`), so the two can no longer disagree about whether a PR has merged.

- 2026-07-09 - Honest forge (v0.16.0): a forge that pushed nothing no longer
  claims "forged". `jj git push` exits 0 even when it moves nothing (printing
  "Nothing changed."), so an undescribed or unbookmarked working copy used to
  sail through the push step and drop the progress overlay as if it had merged
  work. Push now reads its output and reports "nothing to push" when the push was
  a no-op, leaving the row honestly "ready to forge".

- 2026-07-09 - Claude pane inherits your shell PATH (v0.15.0): the left pane now
  launches claude through your login shell (`$SHELL -l -i -c claude`) instead of
  execing `claude` directly, so it sources your `.zprofile`/`.zshrc` and sees the
  same PATH as the two shell panes beside it. Previously a GUI-launched kitty
  handed claude launchd's bare environment, leaving it (and every command it ran)
  with an empty PATH. A new `[terminal] claude_command` config field overrides
  the left-pane command (program + args) if you want a different agent or to skip
  the shell wrap.

- 2026-07-09 - Shared-base work state (v0.14.0): a workspace's state
  (PR / pushed / dirty) is now computed only from the commits it *owns*, not from
  a base it shares with other workspaces. Previously a branch sitting on a common
  ancestor - e.g. several workspaces parked on or stacked above one feature branch -
  made every one of them claim that branch's PR (and look pushed). Each commit is
  now attributed to at most one workspace: a commit on a single chain belongs to
  it, and a commit several workspaces share belongs only to the one that uniquely
  *heads* it (a base nobody uniquely heads is owned by none). So an empty workspace
  parked on a pushed branch reads `clean`, a workspace with its own commit on top
  reads `dirty` (measured from its own base), and normal stacked PRs still show.

- 2026-07-08 - Behind indicator (v0.11.1): the `↓N` "behind trunk" count now
  measures against the same base as the dirty/clean classification -
  `TRUNK_BASE` (the latest of the remote mainline and the local
  `main`/`master`/`trunk` bookmarks) - instead of jj's raw `trunk()`
  (`origin/main`). Previously, when local `main` was ahead of `origin/main`, a
  workspace could read `clean` yet show no `↓` despite being several commits
  behind the base its cleanliness was judged against.

- 2026-07-08 - Stop the forge pipeline from spewing over the TUI (v0.8.2): the
  `fetch`, `weld`, `push`, and `spr` steps ran their subprocesses with bare
  `.status()`, which inherits the parent's stdout/stderr - so jj/jj-spr output
  (`Working copy (@) now at:`, `Nothing changed.`, `Added 0 files, modified 5
  files`) printed straight onto the alt-screen and corrupted the workspace list.
  Those steps now redirect both streams to `Stdio::null()`, matching the forge's
  design of modelling step state natively rather than scraping stdout.

- 2026-07-08 - Correct the diff base in never-pushed repos (v0.8.1): `trunk()`
  resolves to the root commit when no remote mainline bookmark exists yet, which
  made every workspace diff the entire history - so an empty workspace read as
  `dirty` and landed in "ready to forge". The work snapshot now measures a
  workspace's chain and diff against `trunk()` when it is a real commit, else the
  local `main`/`master`/`trunk` bookmark. Once main is pushed, `trunk()` wins and
  behaviour is unchanged.

[0.55.0]: https://github.com/Jarvvski/jjfx/compare/v0.54.0...v0.55.0
[0.54.0]: https://github.com/Jarvvski/jjfx/compare/v0.53.0...v0.54.0
[0.53.0]: https://github.com/Jarvvski/jjfx/compare/v0.52.1...v0.53.0
[0.52.1]: https://github.com/Jarvvski/jjfx/releases/tag/v0.52.1
