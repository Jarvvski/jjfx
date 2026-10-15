# Configuration

jjfx reads its own settings once at startup from
`${XDG_CONFIG_HOME:-~/.config}/jjfx/config.toml`. This file is distinct from jj
config (read through the `jj` CLI) and the lifecycle event log - it is the only
file jjfx itself owns. A missing file means defaults; a file that exists but
fails to parse is a startup error naming the offending key, surfaced before the
TUI takes over the screen. Unknown keys are rejected, so a typo fails loudly
rather than being silently ignored.

Every section is optional. This is a complete example:

```toml
colocate = true

[agent]
command = "claude"

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
```

## `[agent]`

| Key | Default | Meaning |
| --- | --- | --- |
| `command` | `"claude"` | The shell command run in a workspace's agent pane. It runs in your login interactive shell as `$SHELL -l -i -c <command>`, so aliases and your `PATH` are available and the value may name an alias such as `cx`. |

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
| `pull_requests` | `true` | Whether forging creates and updates PRs at all. Set false to stop after push and open PRs yourself. |
| `draft` | `true` | Open newly-created PRs as drafts. Set false to open them ready for review. |

### Formatting before push

The forge normalises a workspace's just-welded chain with `jj fix` before it
pushes, honouring whatever `[fix.tools.<id>]` you have configured in jj itself -
jjfx neither chooses nor ships formatters (see `jj help -k config`). With no
tools configured the step is a no-op. A formatter that fails is reported as a
forge error and stops the push, so a broken formatter never passes silently.

## `[tickets]`

Where `dispatch --all` and orchestration discover Ready Tickets. The source is a
config decision, independent of the Agent Runtime a Worker Pool runs, so a
runtime with no native tracker can still pull work through a configured source.

`source` is required when the section is present; an absent `[tickets]` section
means the default, `linear`. An unknown source fails at load, before the TUI
takes the screen or any Worker is reserved.

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
`.scratch/<feature>/issues/<NN>-<slug>.md` convention. No runtime, no Linear, and
no Pi helper are involved; see [Local tickets](local-tickets.md) for the layout,
identity, dependency, and writeback rules.

| Key | Default | Meaning |
| --- | --- | --- |
| `root` | `".scratch"` | Directory under the repository root holding `<feature>/issues/*.md`. |
| `ready_status` | `"ready-for-agent"` | The `Status:` value marking a Ticket ready for Dispatch. |
| `resolved_status` | `"resolved"` | The `Status:` value marking a Ticket resolved, and therefore a satisfied `Blocked by:` entry. |

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
Both Dispatch paths are gated: the Pool view's Dispatch and orchestration, and
`jjfx dispatch` (including its orchestration handoff). A cap that cannot be
evaluated because the record store is unreadable also refuses Dispatch - failing
closed is the only safe answer for a spend gate.

The current aggregate is always visible without the cap: the Pool view shows a
pool spend line, and `jjfx status` (and `jjfx pool list`) shows a `SPEND` column
and a pool-level summary.

## `[review]`

Which AI reviewer `jjfx review <worker>` runs, and how many times the re-review
loop may iterate. An absent `[review]` section means `reviewer = "none"`, which
leaves `review` exactly as it was: the prompt inlines the PR's review state,
failing checks and human comments, and nothing else runs.

| Key | Default | Meaning |
| --- | --- | --- |
| `reviewer` | `"none"` | Which reviewer produces findings: `none`, `coderabbit`, or `opencode-action`. |
| `max_passes` | `2` | The pass cap for the re-review loop; must be at least 1. |

```toml
[review]
reviewer = "coderabbit"
max_passes = 2
```

`coderabbit` shells out to the CodeRabbit CLI in agent mode. `opencode-action`
is accepted but not implemented yet: selecting it fails at load with a setup
error rather than silently skipping the review.

See [The review loop](review-loop.md) for what a configured reviewer does, how
the pass cap bounds it, and how it authenticates without leaking credentials.

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
