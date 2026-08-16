# jjfx

A terminal TUI for working with [Jujutsu (jj)](https://jj-vcs.github.io/jj/)
workspaces alongside coding agents - one surface where you drive your VCS and
the agents editing it, instead of switching between them.

**Status:** `jjfx` provides the CLI and an explicit interactive TUI entrypoint.
The compatibility `wsg` target remains available for migration conformance.

## Build & run

Tooling is driven by [mise](https://mise.jdx.dev):

```bash
mise install     # pin the Rust toolchain
mise run run     # cargo run -p jjfx -- tui
mise run build   # cargo build
mise run test    # cargo test
mise run fmt     # cargo fmt --all
mise run lint    # cargo clippy --all-targets -- -D warnings
mise run check   # fmt + lint + build + test (the pre-land gate)
```

Plain cargo works too:

```bash
cargo run -p jjfx -- tui       # open the TUI
cargo run -p jjfx -- pool list # run a CLI command
```

## Interactive agent lifecycle setup

Run `jjfx hooks status` to inspect lifecycle integration for Claude Code, Codex,
and Pi. Install or update every integration with:

```bash
jjfx hooks install
```

For Pi, jjfx installs an owned, auto-discovered extension at
`${PI_CODING_AGENT_DIR:-~/.pi/agent}/extensions/jjfx-lifecycle.ts`. Installation
is additive and does not modify Pi settings, packages, sessions, project trust,
or unrelated extensions. A conflicting non-jjfx file at that path is reported
and never overwritten.

The extension maps Pi session start and settled events to waiting, active agent
and turn events to working, and graceful session shutdown to ended. Pi does not
expose a native permission or attention event, so jjfx does not fabricate
`NeedsAttention` from tool or provider failures. An abruptly terminated Pi
process may remain waiting until a later lifecycle event because jjfx does not
infer shutdown by polling or terminal scraping.

## Pi Worker actions, Direct Dispatch, and ticket discovery

The shared Worker action layer supports Pi 0.84.x for Direct Dispatch, fresh
and resumed Follow-ups, and interactive kitty mounts. The `pi` executable must
be on `PATH`, and callers must select an authenticated provider and model
explicitly through `AgentModel::new(model).with_provider(provider)`.

Pi Direct Dispatch and Follow-up require the pinned `pi-mcp-adapter` 2.11.0
package:

```bash
pi install npm:pi-mcp-adapter@2.11.0
```

Configure a Linear MCP server named `linear` and expose only the required
original tools as direct tools. Keep the server transport and credential lookup
in your MCP configuration rather than command arguments:

```json
{
  "mcpServers": {
    "linear": {
      "url": "<your Linear MCP endpoint>",
      "directTools": ["get_issue", "update_issue", "create_comment"]
    }
  }
}
```

Before any Worker reservation, Pool growth, assignment persistence, or
Workspace preparation, jjfx starts a bounded isolated Pi RPC probe. The probe
loads only the pinned adapter and a private inspection extension, then requires
active `linear_get_issue`, `linear_update_issue`, and
`linear_create_comment` tools with compatible schemas. Missing provider/model,
package, tool, or schema support fails with sanitized setup guidance and does
not fall back to Claude or Codex.

Pi Worker runs and mounts use the repository-owned `.jj/pool/pi-sessions`
directory. Direct Dispatch and Follow-up ignore inherited extensions, skills,
prompt templates, themes, context files, and project trust, explicitly load
only the pinned Linear adapter, disable approval prompts, and allow the fixed
`read,bash,edit,write,grep,find,ls` tools plus the three Linear tools. Interactive
Mount retains the fixed built-in coding-tool policy. These policies are not
filesystem confinement: Pi runs with the host user's permissions, so use an
operating-system sandbox when the Workspace needs a stronger boundary.

Pi core does not provide aggregate budget limits or per-tool approval dialogs,
so those Direct Dispatch choices are rejected instead of silently weakened. Pi
also has no native Linear ticket discovery. For read-only Ready Ticket and
dependency discovery, set `JJFX_PI_LINEAR_HELPER` to a dedicated helper
executable. jjfx
runs it directly from the repository root with a 30-second timeout, sends one
versioned JSON request on stdin, and expects one versioned JSON result or typed
error on stdout. The helper owns credential lookup and must provide read-only
Linear access; credentials are never placed in the request or command arguments.

Protocol version 1 accepts these requests:

```json
{"version":1,"operation":"ready_tickets","label":"ready-for-agent","status":"Todo"}
{"version":1,"operation":"dependency_graph","parent":"AMBA-40","repository":"owner/repo"}
```

A success envelope is `{"version":1,"result":{...}}`. An error envelope has
this shape:

```json
{
  "version": 1,
  "error": {
    "kind": "transient|authentication|unsupported|not_configured|permanent",
    "message": "sanitized guidance"
  }
}
```

Transient failures use the existing single discovery retry. Missing setup,
authentication, unsupported capabilities, and malformed protocol envelopes fail
without falling back to Claude or Codex or reserving a Worker.

## Contributing

Project conventions and agent guidance live in [`CLAUDE.md`](CLAUDE.md); see
[`CHANGELOG.md`](CHANGELOG.md) for what's landed.

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE).
