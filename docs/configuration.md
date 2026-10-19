# Configuration

jjfx reads its own settings from two files. The **user config**,
`${XDG_CONFIG_HOME:-~/.config}/jjfx/config.toml`, is read once at startup and
applies everywhere. The **repository config**, `<repo>/.jjfx/config.toml`, is
read per repository and may set only the settings that are a property of the
repository - [`[checks]`](#checks), [`[pools]`](#pools) and
[`[tickets]`](#repository-local-ticket-source). Both are distinct from jj config
(read through the `jj` CLI) and the lifecycle event log. A missing file means
defaults; a file that exists but fails to parse is an error naming the offending
key and file, surfaced before the TUI takes over the screen. Unknown keys are
rejected, so a typo fails loudly rather than being silently ignored.

Every section is optional. This is a complete example:

```toml
colocate = true

[agent]
command = "claude"

[profiles.fast]
runtime = "claude"
provider = "anthropic"
model = "claude-sonnet-4-20250514"

[terminal]
listen_on = "unix:/tmp/kitty-visor"
launch_command = ["kitty", "--detach", "-o", "listen_on=unix:/tmp/kitty-visor"]
first_pane_command = "dev-server"

[terminal.layout_by_display]
laptop = "narrow"
external = "wide"
default = "wide"

[terminal.layouts.wide]
[[terminal.layouts.wide.panes]]
role = "first"
[[terminal.layouts.wide.panes]]
role = "agent"
location = "vsplit"
bias = 81
[[terminal.layouts.wide.panes]]
role = "shell"
location = "hsplit"

[forge]
pull_requests = true
draft = true

[tickets]
source = "linear"

[tickets.linear]
status = "Todo"

[tickets.local]
root = ".scratch"
ready_status = "ready-for-agent"
resolved_status = "resolved"

[spend]
cap_usd = 25.0
window = "week"
on_exceed = "block"

[dispatch]
max_concurrent = 4

[watchdog]
stuck_after_secs = 1800
```

## `[agent]`

| Key | Default | Meaning |
| --- | --- | --- |
| `command` | `"claude"` | The shell command run in a workspace's agent pane. It runs in your login interactive shell as `$SHELL -l -i -c <command>`, so aliases and your `PATH` are available and the value may name an alias such as `cx`. |

## `[profiles]`

Named Agent Runtime Profiles: the runtime, and the provider and model its Runs
launch with. A repository-local [`[pools]`](#pools) declaration may **reference**
one by name, but never define one - a profile is executable-affecting, so it is
granted here, user-globally, and a Repository may only select among the profiles
already granted. See
[ADR 0014](adr/0014-repository-local-pool-declarations.md).

```toml
[profiles.fast]
runtime = "claude"
provider = "anthropic"
model = "claude-sonnet-4-20250514"

[profiles.deep]
runtime = "codex"
model = "gpt-5"
```

| Key | Default | Meaning |
| --- | --- | --- |
| `runtime` | required | The Agent Runtime: `claude`, `codex`, or `opencode`. |
| `provider` | unset | The provider identifier the runtime needs alongside the model, e.g. `anthropic`. Requires `model`. |
| `model` | unset | The model identifier, e.g. `claude-sonnet-4-20250514`. Unset leaves the runtime to pick its own default model. |

## `[terminal]`

Where jjfx opens workspace session tabs and how it builds them.

| Key | Default | Meaning |
| --- | --- | --- |
| `listen_on` | unset | The target kitty's `listen_on` *base* value, e.g. `unix:/tmp/kitty-visor` (exactly what you pass kitty, not the live socket). kitty appends `-<pid>` to a unix path, so jjfx resolves `/tmp/kitty-visor-<pid>` at call time and routes every `kitten @` there. Unset uses the inherited `KITTY_LISTEN_ON` - the kitty jjfx runs inside. |
| `launch_command` | `[]` | Program plus arguments jjfx runs to start the target when its socket is not found, e.g. a detached kitty invocation with a matching `listen_on`. It should return promptly; jjfx then polls `listen_on` until the instance answers. Empty never auto-launches - jjfx reports the target as not running. |
| `first_pane_command` | unset | A shell command run in an opened tab's first (top-left) pane - a dev server or watcher, say. Like `agent.command` it runs through the login interactive shell, and the pane drops back to a shell when the command exits. Unset leaves that pane a plain shell. Applies to both TUI-opened tabs and `mount`. |
| `layouts` | `{}` | Named pane layouts a tab can use, keyed by name. See below. |
| `layout_by_display` | unset | Which named layout to build for each display condition. See below. |

`listen_on` is also how jjfx tells which Workspaces are open: Dispatch and `send`
skip (or refuse) a Worker whose tab is open, because an open Workspace is not
idle capacity. When the target cannot be queried - unset and no inherited
`KITTY_LISTEN_ON`, or a configured socket that is not running - jjfx fails closed
rather than guess, and names `--force` as the override. Point `listen_on` at the
kitty that hosts your Workspaces so an agent or script dispatching from another
terminal sees the same tabs you do.

### Named pane layouts

A layout is an ordered plan of panes. The first entry is the tab's own window;
every later entry splits from an earlier one, so a layout is a tree rooted at
index 0. Panes are built in order.

```toml
[terminal.layouts.wide]
[[terminal.layouts.wide.panes]]
role = "first"
[[terminal.layouts.wide.panes]]
role = "agent"
location = "vsplit"
bias = 81
[[terminal.layouts.wide.panes]]
role = "shell"
from = 1
location = "hsplit"
```

Each pane placement accepts:

| Key | Default | Meaning |
| --- | --- | --- |
| `role` | required | What the pane runs: `first` (the configured first-pane command, else a shell), `agent`, or `shell`. |
| `from` | `0` | Index of the earlier pane this one splits from. The first pane cannot split from another. |
| `location` | required after the first pane | Split direction: `vsplit` (side by side) or `hsplit` (stacked). Must be absent on the first pane. |
| `bias` | `50` | Percentage of the split given to this new pane (kitty's `--bias`). Must be between 1 and 99. |

The plan is validated when config loads, so an invalid layout fails before
anything opens rather than mid-open.

### Display mapping

`layout_by_display` picks a named layout for the current display condition:

```toml
[terminal.layout_by_display]
laptop = "narrow"    # only the built-in display attached
external = "wide"    # at least one external display attached
default = "wide"     # detection unavailable, or a condition has no entry
```

An unmapped condition falls back to `default`, then to jjfx's built-in layout.
The built-in layout is the historical one: a left column split into the first
pane over a shell, with the agent filling the rest. Setting `JJFX_LAYOUT=<name>`
forces a named layout regardless of display, overriding `layout_by_display`.

Both workspace tabs and worker `mount` build from the same selection.

## `[forge]`

How the forge pipeline's final step manages pull requests over `gh`.

| Key | Default | Meaning |
| --- | --- | --- |
| `pull_requests` | `true` | Whether forging opens Pull Requests at all. Set false to forge without a forge - welding, checking and pushing only - and open PRs yourself. |
| `draft` | `true` | Open newly-created PRs as drafts. Set false to open them ready for review. |

One Pull Request per change, based on the repository's default branch: the forge
never assembles base-chained stacks and never rewrites a PR body beyond the
change's own description. A repository with no forge - `pull_requests = false`,
or no `origin` remote to open one against - forges cleanly: the Pull Request
step does not apply, and it does not taint an otherwise successful run.

### Formatting before push

The forge normalises a workspace's just-welded chain with `jj fix` before it
pushes, honouring whatever `[fix.tools.<id>]` you have configured in jj itself -
jjfx neither chooses nor ships formatters (see `jj help -k config`). With no
tools configured the step is a no-op. A formatter that fails is reported as a
forge error and stops the push, so a broken formatter never passes silently.

### Build check before push

After welding onto trunk and before pushing, the forge runs the repo-owned
command configured in `[checks]` - if any. Weld has already rebased the workspace
onto trunk by then, so the check sees what `main` will actually be; a change that
does not build against it fails here rather than on the next person to rebuild.
With no `[checks]` command configured the step does not apply and the pipeline is
unchanged. See [`[checks]`](#checks) below.

## `[tickets]`

Where `dispatch --all` and orchestration discover Ready Tickets. The source is a
config decision, independent of the Agent Runtime a Worker Pool runs, so a
runtime with no native tracker can still pull work through a configured source.

`source` is required when the section is present; an absent `[tickets]` section
means the default, `linear`. An unknown source fails at load, before the TUI
takes the screen or any Worker is reserved.

A Ticket handed in directly (`jjfx dispatch --text`, `--from-file`, or stdin)
never consults this section at all, so an ad-hoc dispatch works in a repository
with no `[tickets]` and no `.scratch/`; see
[Ad-hoc tickets](local-tickets.md#ad-hoc-tickets).

| Key | Default | Meaning |
| --- | --- | --- |
| `source` | `"linear"` (absent section) | Which tracker provides Ready Tickets: `linear` or `local`. |

### `[tickets.linear]`

Discover Ready Tickets through the Agent Runtime's native Linear access - the
historical path, unchanged. Claude, Codex and opencode each render the discovery
prompt their own way; the runtime is the transport, not the source.

| Key | Default | Meaning |
| --- | --- | --- |
| `status` | `"Todo"` | The Linear workflow status a Ready Ticket must be in. |

### `[tickets.local]`

Discover Ready Tickets from local markdown under the repository - the
`.scratch/<feature>/issues/<NN>-<slug>.md` convention. No runtime and no Linear
are involved; see [Local tickets](local-tickets.md) for the layout,
identity, dependency, and writeback rules.

| Key | Default | Meaning |
| --- | --- | --- |
| `root` | `".scratch"` | Directory under the repository root holding `<feature>/issues/*.md`. |
| `ready_status` | `"ready-for-agent"` | The `Status:` value marking a Ticket ready for Dispatch. |
| `resolved_status` | `"resolved"` | The `Status:` value marking a Ticket resolved, and therefore a satisfied `Blocked by:` entry. |

### Repository-local ticket source

A Repository may **select** which configured Ticket source it uses, in its own
`.jjfx/config.toml`, so a cloned repository can discover its own local Tickets
without the user config being changed for it:

```toml
# <repo>/.jjfx/config.toml
[tickets]
source = "local"
```

The selection is a **reference, not a grant** (ADR 0014). A Ticket source is one
of the capabilities a repository-local file may select among and may never
define: `source` names a source the user config already provides, so the local
root, the Linear status and any credentials stay user-global. An absent
`[tickets]` section leaves the user-global selection in place, so a repository
that says nothing behaves exactly as before.

| Key | Default | Meaning |
| --- | --- | --- |
| `source` | the user-global value | Which configured Ticket source this Repository uses: `linear` or `local`. Required when the section is present. |

Everything that *defines* a source is refused here, as a loud error naming the
file: a `[tickets.local]` or `[tickets.linear]` table, a `root` or `status`
key, a credential, and an unknown source name such as `jira` are all rejected at
load rather than silently ignored. Only `source` is accepted. This is the same
capability rule [`[pools]`](#a-declaration-may-select-but-never-grant) keeps,
and it is what makes it safe to *commit* the selection: teammates share which
source a repository reads, never a credential.

## `[spend]`

An optional cap on the aggregate spend Dispatch may run up. The aggregate is the
sum of the durable per-Run cost records jjfx writes at finalization (see
[Run records](run-records.md)), so the cap is enforced from what Runs actually
cost rather than from a per-Run hint. An absent `cap_usd` means no cap and no
gate.

| Key | Default | Meaning |
| --- | --- | --- |
| `cap_usd` | unset | The cap in US dollars. Must be a positive, finite amount; an absent key means no cap. |
| `window` | `"week"` | The window the cap totals over: `today`, `week` (the last seven days), or `total` (every recorded Run). |
| `on_exceed` | `"block"` | What Dispatch does once the aggregate reaches the cap: `block` refuses it, `warn` lets it through and reports the overage. |

The cap fires at or above its limit - a Run at exactly the cap would push the
total past it. A blocked Dispatch is refused before any Worker is reserved or
launched, with a message naming the cap, the current total and the setting that
overrides it:

```text
recorded Run spend $27.4000 over the last 7 days exceeds the $25.0000 spend cap;
raise or remove spend.cap_usd, or set spend.on_exceed = "warn", to override
```

To override, raise or remove `spend.cap_usd`, or set `spend.on_exceed = "warn"`.
Both Dispatch paths are gated: the Worker Pool view's Dispatch and
orchestration, and `jjfx dispatch` (including its orchestration handoff). A cap
that cannot be evaluated because the record store is unreadable also refuses
Dispatch - failing closed is the only safe answer for a spend gate.

The current aggregate is always visible without the cap: the Worker Pool view
shows a pool spend line, and `jjfx status` (and `jjfx pool list`) shows a
`SPEND` column and a Repository-wide summary. When the Repository holds several
pools, each pool's recorded spend is reported against its own
[`cap_usd`](#pools) - a subtotal under that total, in the `Pools:` table and in
the `pools` array of `jjfx status --json`.

## `[dispatch]`

An optional ceiling on how many Runs Dispatch may keep in flight at once.
`pool N` sizes `default` and `dispatch --all` fires the whole batch, so a large
Worker Pool plus a large batch can trip a provider's rate limit in one go; the
cap paces the batch instead. Absent `max_concurrent` means no cap and Dispatch
behaves exactly as it did before the governor existed: the whole batch fires at
once.

| Key | Default | Meaning |
| --- | --- | --- |
| `max_concurrent` | unset | The most Runs that may be in flight at once. Must be at least 1; an absent key means no cap. |

```toml
[dispatch]
max_concurrent = 4
```

The cap is a ceiling on concurrent Runs, not a batch size. A batch fires its
first `max_concurrent - in_flight` Tickets, where `in_flight` counts the Workers
the Worker Pool already records as busy, and holds the rest back. Repeated
Dispatches therefore stay under the same limit rather than each firing a fresh
batch of `max_concurrent`.

A held Ticket is neither reserved nor launched. It comes back as a waiting
outcome in input order, named in the CLI and the Worker Pool view:

```text
  ENG-42 waiting: concurrency cap 4 reached (4 Run(s) in flight)
Held 2 ticket(s) back under the dispatch cap; re-run once a Run finishes.
```

The waiting set is recorded in jjfx's own
`.jj/pools/<name>/waiting.json`, inside the pool's own state directory, and
shown by `jjfx status` (a `Waiting:` line naming each Ticket's pool, and the
`waiting` array in `jjfx status --json`, where each entry carries its `ticket`
and the `pool` whose Dispatch held it) and the Worker Pool view, so the backlog
is visible after the Dispatch that held it. It is jjfx-owned presentation state,
kept out of the pool and Worker documents, and it is rewritten by every Dispatch
- including an uncapped one, which clears it - so it always describes the last
Dispatch rather than accumulating Tickets you have since dispatched by hand.

## `[watchdog]`

How long a Run may make no progress before jjfx surfaces it as stuck. A Worker
whose agent has wedged - waiting on a prompt it will never get, or looping -
stays `busy` with no signal beyond elapsed time, so the owner finds out by
reading the list and guessing.

| Key | Default | Meaning |
| --- | --- | --- |
| `stuck_after_secs` | `1800` | Seconds without progress before a busy Run is surfaced as stuck. `0` disables the watchdog. |

```toml
[watchdog]
stuck_after_secs = 1800
```

Progress is the later of the Run's start and its Run log's last write, so a Run
that just started is not stuck and a log still being written is progress by
definition. A Run whose recorded process is gone is reconciliation's problem, not
the watchdog's: it becomes failed, never stuck. A Run with neither a start time
nor a readable log has no evidence either way and is never flagged on a guess.

Stuck is derived, never persisted - like occupancy it is a view of live Run
state, so the pool and Worker documents are untouched. It shows up
in three places:

- the home list, where a stuck Run sorts into the `needs you` lane instead of
  reading as `working`;
- the Worker Pool view, as a `[stuck]` marker beside the Worker's persisted
  `busy` status;
- `jjfx status`, as a per-Worker `stuck: no progress for 42m 0s` line and a
  `, N stuck` count in the pool summary.

Set `stuck_after_secs = 0` to turn the signal off entirely, leaving the list
exactly as it was before the watchdog existed. Nothing is ever re-dispatched
automatically: a stuck Run is surfaced, and the human decides.

## `[review]`

Which AI reviewer `jjfx review <worker>` runs, and how many times the re-review
loop may iterate. An absent `[review]` section means `reviewer = "none"`, which
leaves `review` exactly as it was: the prompt inlines the PR's review state,
failing checks and human comments, and nothing else runs.

| Key | Default | Meaning |
| --- | --- | --- |
| `reviewer` | `"none"` | Which reviewer produces findings: `none`, `coderabbit`, or `opencode-action`. |
| `max_passes` | `2` | The pass cap for the re-review loop; must be at least 1. The loop also stops early when a round makes no progress, so this is a ceiling, not a target. |

```toml
[review]
reviewer = "coderabbit"
max_passes = 2
```

`coderabbit` shells out to the CodeRabbit CLI in agent mode. `opencode-action`
is accepted but not implemented yet: selecting it fails at load with a setup
error rather than silently skipping the review.

See [The review loop](review-loop.md) for what a configured reviewer does, how
the pass cap and the progress rule bound it, and how it authenticates without
leaking credentials.

## `[migrations]`

How the cross-workspace migration-order check finds migrations. Parallel Workers
can each add an ordered migration and be individually correct yet wrong as a
*set*: two workspaces adding `V189` and `V190` land out of order if the higher
version merges first, which Flyway rejects on the next deploy when `outOfOrder`
is unset - and CI never sees it, because CI applies the whole set in version
order on a fresh database.

`jjfx check migrations` reads each open workspace's working copy - the files on
disk, including a migration an agent has written but not committed - and reports
any inversion before either merges, exiting non-zero on a hazard so an agent can
gate on it. The check is read-only; it never reorders or rewrites. The same
report also leads the TUI's workspace list as a notice.

| Key | Default | Meaning |
| --- | --- | --- |
| `pattern` | `"**/V*__*.sql"` | The glob a migration file's repo-relative path must match. `**` matches zero or more path segments, `*` any run of characters within one, `?` one. Set it to your own layout so a non-Flyway project is not forced into Flyway's shape. |

Two migrations in the same directory are one ordering space, so a per-module
project keeps each module's sequence separate. The `default` workspace's files
are the baseline, so a migration every workspace inherits is not an addition and
cannot invert; two workspaces adding the *same* version are reported as
ambiguous rather than silently ordered.

## `[checks]`

The repo-owned build check the forge runs against the welded tree, before it
pushes. A Run is `done` purely from the agent runtime's own terminal event, so
the whole "this ticket is finished" signal is the agent's report that it is - and
two changes can each be green, mergeable and at zero open threads while **not
compiling together**. Weld has already rebased onto trunk when the check runs, so
it sees what `main` will actually be, which a check on the un-welded tree would
miss.

jjfx never guesses the command: an unconfigured repo is a no-op, not a default.

| Key | Default | Meaning |
| --- | --- | --- |
| `command` | unset | The command the repository owns for a build check, run through the user's shell in the workspace: `cargo check`, `./gradlew :app:backend:compileKotlin`, `bun run build`. Unset means no check. |
| `enabled` | `true` | Whether the check runs. Set false to keep the command configured but skip the check. |
| `timeout_secs` | `600` | How long the check may run before jjfx kills it and fails the step. |

```toml
[checks]
command = "cargo check"
timeout_secs = 600
```

A check that fails **stops the push** - an unbuildable change is not a change,
and pushing it is what makes the pair invisible. So does a check that cannot run
at all: a command that does not exist (a shell reports exit 127) or one that
outruns `timeout_secs` is a loud failure, never a silent pass. The forge names the
command and the last line of its output.

The same check is the ground truth for a **local Ticket's done status**. A Run
is recorded as `done` from the agent runtime's own terminal event - the agent's
report that it finished - so a local Ticket is resolved only when the Run's
change passed this check, and a Run whose check failed, could not run, or had
none configured leaves the Ticket not-done with the reason named. See
[Local tickets](local-tickets.md#done-means-verified-not-the-agent-said-so).

### Repository-local checks

`[checks]` is one of the three settings a repository may own. When the
forge runs, jjfx reads `<repo>/.jjfx/config.toml`, and a `[checks]` section
there **replaces** the user-global one for that repository - a repository either
owns its check or inherits the user's. Any other section in that file is
rejected, so a repository cannot silently configure the agent, the terminal, or
anything else. The other repository-owned sections are [`[pools]`](#pools) and
[`[tickets]`](#repository-local-ticket-source).

```toml
# <repo>/.jjfx/config.toml
[checks]
command = "./gradlew :app:backend:compileKotlin"
timeout_secs = 1200
```

A check is a property of the repository's build, and the user config is read once
and applied to every repository jjfx is pointed at - so a check set there would
run in all of them, and in a repository whose build does not understand it that
is a loud failure, not a no-op.

The file is read by default, and it sits next to the `setup.sh` / `teardown.sh`
hooks, which jjfx already executes when it creates a workspace in the repository -
so a repository-local check grants no capability the repository did not already
have. Track it in jj (`jj file track root:.jjfx/config.toml`) so it is present in
a fresh clone.

## `[pools]`

A Repository may run several named Worker Pools, each its own lane: its own size,
its own Agent Runtime Profile, its own Ticket source, and its own spend cap under
the Repository-wide ceiling. They are declared in the repository config,
`<repo>/.jjfx/config.toml`, in a `[pools]` section beside `[checks]`:

```toml
# <repo>/.jjfx/config.toml
[checks]
command = "cargo check"

[pools.bugs]
size = 2
profile = "fast"
source = "local"
label = "bug"
cap_usd = 5.0

[pools.features]
size = 4
profile = "deep"
source = "linear"
label = "feature"
ready_status = "Todo"
```

### A declaration may select, but never grant

The repository config is read by default because it grants no capability the
repository did not already have. A pool declaration keeps that rule: it may
**reference** an Agent Runtime Profile and a Ticket source the user config
defines, and may set a size, a spend cap, a routing label and a ready status. It
may **not** define an agent command, a model, credentials, a runtime or a new
source kind. A `[pools]` entry that tries to is a loud error naming the file,
never a silent acceptance - which is what makes it safe to *commit* a lane
definition: teammates share lanes, never running Workers. See
[ADR 0014](adr/0014-repository-local-pool-declarations.md).

### Keys

Every key is optional but the pool's name (the table key).

| Key | Default | Meaning |
| --- | --- | --- |
| `size` | unset | The number of Workers the pool holds. Absent leaves the Repository's configured size in place. |
| `profile` | unset | The name of an Agent Runtime Profile the user-global [`[profiles]`](#profiles) defines. Naming one the user config does not define is a loud error. Absent leaves the Repository default in place. |
| `source` | unset | Which configured Ticket source discovers this pool's work: `linear` or `local`. Absent means the pool has **no** Ticket source and is reachable only by an explicit `--pool`. |
| `label` | unset | The Ticket label that routes a Ticket to this pool, which `jjfx dispatch --all` fans out over. A pool that declares none is reachable only by an explicit `--pool`. Two pools declaring the same label is an error at load, naming both. |
| `ready_status` | the global value | The `Status:` value a Ticket must be in to be ready for this pool's discovery, defaulting to the user-global value. |
| `cap_usd` | unset | The pool's spend cap in US dollars, under the Repository-wide ceiling. Must be a positive, finite amount; absent means the ceiling is the only limit. |
| `window` | the global value | The window the pool's cap totals over, defaulting to `spend.window`. |
| `on_exceed` | the global value | What Dispatch does once the pool's cap is reached, defaulting to `spend.on_exceed`. |

### A declared lane materialises on first use

The declaration is the source of truth and `.jj/pools/<name>/` is state, so a
declared pool with no state directory is normal, not a disagreement. The first
Dispatch into such a lane provisions the Workers its `size` names - exactly as
`jjfx pool <N>` does for `default` - and the lane is created once, never
re-provisioned by a later Dispatch. That is what makes a committed declaration
work on a fresh clone with nobody provisioning the lane by hand. An implicit
`default` has no declaration to materialise from, so sizing it stays
`jjfx pool <N>`.

### The implicit `default` pool

With no `[pools]` section, a Repository has one implicit pool named `default`,
and behaves exactly as it did before pools could be named: `jjfx pool <N>` sizes
`default`, and nothing else changes. A Repository always has at least one pool,
so `default` is not removable - resize it to zero instead. `jjfx pool <N>` keeps
sizing `default` even when other pools are declared.

### Dispatch fills one pool

A Dispatch has to know which pool it fills, and the rule keys off how many the
Repository has, not off Ticket sources. A Repository that declares none has one
implicit `default`, so `jjfx dispatch <TICKET>` implies it and works unchanged.
Declaring any named pool makes `--pool <name>` required - `default` still exists
beside the named lanes - and an unqualified Dispatch is refused loudly, naming
the pools that exist, rather than silently choosing one. `--text` and
`--from-file` follow the same rule.

Naming a pool the Repository does not hold is refused the same way. Worker
reservation, Dispatch Eligibility and the pool's size are all evaluated against
the selected pool, so `jjfx dispatch --pool bugs` fills `bugs` and leaves every
other lane alone.

### Routing a Ticket to its pool by label

`--pool` on every Dispatch makes lanes capacity buckets a human has to route by
hand, so a pool may declare the **label that routes to it**
(`[pools.bugs] label = "bug"`). With one declared, `jjfx dispatch --all` fans out
across the pools that declare a label, discovering each pool's ready work by its
own label and dispatching each Ticket to the pool whose label it carries. The
ready label is per pool - `--label` overrides the pool's declared label, which
overrides the global `ready-for-agent` - because each pool's source may select
work by a different label.

The **ready status** is per pool as well. A pool may declare the `Status:` a
Ticket must be in for that lane (`[pools.review] ready_status =
"ready-for-review"`), defaulting to the user-global value, so a review lane
draws `ready-for-review` while a lane that declares none keeps drawing the
global status - from the same `.scratch/` tree. The status filters that pool's
discovery exactly as its label routes it: the `--all` fan-out draws each lane by
its own status, an explicit `--pool` uses the selected lane's, and a Ticket
named by hand reports its readiness against the selected pool's status.

Routing is by declaration, never by a magic `pool:<name>` string, so a lane can
adopt a vocabulary the Repository already uses. A pool that declares no label is
not routable: it is reachable only by an explicit `--pool`, and a Repository
whose pools declare no labels keeps the unqualified refusal above rather than
guessing.

Two rules keep the convenience from becoming a silent choice:

- **A Ticket that matches no pool is reported, not dropped.** A Ticket whose
  labels no pool routes is named on stderr with what it carried, and in a
  `--json` batch as a `skipped` entry, so an unroutable Ticket is never
  indistinguishable from one that was never filed.
- **An explicit `--pool` still wins, and still means "only this lane".** It
  overrides label routing entirely rather than narrowing the fan-out.

A local Ticket declares its labels with a `Labels:` line; one written before
that line existed carries the ready label alone, so a Repository that never
adopted labels discovers exactly the work it always did. See
[local tickets](local-tickets.md).

### The verbs edit the declaration

The declaration is the source of truth, so `jjfx pool create` and
`jjfx pool destroy` write the `[pools]` section rather than keeping a parallel
state - there is no second description to reconcile.

- `jjfx pool create <name>` declares a pool, writing its `[pools.<name>]` table.
  It takes one flag per key - `--size`, `--profile`, `--source`, `--label`,
  `--ready-status`, `--cap-usd`, `--window` and `--on-exceed` - all of which
  `jjfx help pool create` lists. A create for a pool that is already declared
  is refused, so a typo cannot clobber a lane's settings.
- `jjfx pool destroy <name>` removes that declaration. It refuses `default`,
  because a Repository always has at least one pool; resize `default` to zero
  instead.
- `jjfx pool <N>` sizes `default`; a named pool is sized explicitly with
  `--pool`, as in `jjfx pool resize 4 --pool bugs`.
- `jjfx pool list` (and `pool list --json`) lists every pool, naming `default`
  when it is implicit.

The write preserves the rest of the file - comments, key order and formatting -
so the verbs are safe to use on a hand-maintained config. A declaration the
reader would reject (an unknown profile, say) is refused before anything is
written.

### Pools in `status` and `pool list`

`jjfx status` (alias `jjfx pool list`) names every pool the Repository holds -
including an implicit `default` - with its size, its recorded spend against its
own cap, and the window that spend is stated over. Each Worker row names its
pool, and so does each Ticket on the `Waiting:` line.

`jjfx status --json` carries the same machine-readably. The flat `workers[]`
gains a `pool` field per Worker, `waiting[]` gains one per held Ticket (each
entry an object with `ticket` and `pool`), and a sibling `pools` key carries
`{name, size, workers, implicit, spend, cap, window}` per pool - a subtotal
under the Repository-wide `Spend:` line, never a restructure. The documented way
to read one lane is:

```bash
jjfx status --json | jq '.workers[] | select(.pool == "bugs")'
```

A **pool directory with no declaration** (`.jj/pools/<name>/` with no
`[pools.<name>]` table) is an orphan: jjfx names it in the `Orphaned pool
directories:` line and in the `orphans` array, and leaves its state in place. It
is never adopted as a pool and never blocks another one; removing it is the
operator's call.

### Validation is loud

A file that parses but is wrong fails at load, naming the file - the same rule as
`[checks]`. A pool naming a profile the user config does not define, two pools
declaring the same routing label, a size that cannot be honoured, and a cap that
is not a positive finite amount are all errors at load, never a silent fallback
or a precedence rule.

**A Repository with no user-global `[spend] cap_usd` is exposed to whatever size a
cloned Repository declares.** The declaration is committed, so a teammate's lane
definition arrives with the clone; declare a ceiling to bound it.

## Legacy `[workspace]`
Earlier releases configured new-workspace provisioning with a `[workspace]`
section (for example `on_create = ["mise", "trust"]`). That moved into
repository-owned lifecycle hooks - see [Workspace hooks](workspace-hooks.md).
The section is still accepted and ignored so an existing config keeps parsing.

## Workspace colocation

New workspaces are colocated with Git: `jj workspace add` creates a Git worktree
so every agent workspace has a working `.git`, which lets tools such as `gh
stack` run inside it. Colocation is on by default; set the top-level `colocate`
key to `false` to create non-colocated workspaces instead.

```toml
colocate = false
```

A repository that is not itself colocated is never colocated, whatever this says.
Run `jjfx colocation enable` (or `jj git colocation enable`) to colocate the
repository itself first; `jjfx colocation disable` reverses it.
