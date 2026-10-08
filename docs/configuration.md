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

## Legacy `[workspace]`

Earlier releases configured new-workspace provisioning with a `[workspace]`
section (for example `on_create = ["mise", "trust"]`). That moved into
repository-owned lifecycle hooks - see [Workspace hooks](workspace-hooks.md).
The section is still accepted and ignored so an existing config keeps parsing.
