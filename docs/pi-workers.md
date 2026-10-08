# Pi Worker actions, Direct Dispatch, and ticket discovery

The shared Worker action layer supports Pi 0.84.x for Direct Dispatch,
persistent Dispatch Group orchestration, fresh and resumed Follow-ups, and
interactive kitty mounts. The `pi` executable must be on `PATH`. Configure the
repository's default Agent Runtime profile before dispatching:

```bash
jjfx pool profile pi --provider <provider> --model <model>
jjfx pool list
jjfx dispatch <TICKET>
```

The same command accepts `claude`, `codex`, or `opencode`; provider and model are
optional for runtimes that support provider-managed selection.
`dispatch --provider` and `dispatch --model` override model selection for that
invocation and are forwarded to detached orchestration. A persisted Dispatch
Group profile remains authoritative after restart, so an explicitly selected Pi
Run never falls back to Claude or Codex. Invalid or incomplete Pi setup fails
before Pool, Reservation, assignment, or Workspace mutation.

The jjfx Pool view displays the selected runtime for Pool capability, Workers,
Dispatch outcomes, orchestration progress, sessions, activity, failures, and
terminal results. It deliberately omits provider/model values and Pi adapter
configuration from presentation output.

## Pinned adapter

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

## Preflight probe

Before any Worker reservation, Pool growth, assignment persistence, or Workspace
preparation, jjfx starts a bounded isolated Pi RPC probe. The probe loads only
the pinned adapter and a private inspection extension, then requires active
`linear_get_issue`, `linear_update_issue`, and `linear_create_comment` tools with
compatible schemas. Missing provider/model, package, tool, or schema support
fails with sanitized setup guidance and does not fall back to Claude or Codex.

## Run policy

Pi Worker runs and mounts use the repository-owned `.jj/pool/pi-sessions`
directory. Direct Dispatch and Follow-up ignore inherited extensions, skills,
prompt templates, themes, context files, and project trust, explicitly load only
the pinned Linear adapter, disable approval prompts, and allow the fixed
`read,bash,edit,write,grep,find,ls` tools plus the three Linear tools.
Interactive Mount retains the fixed built-in coding-tool policy.

These policies are not filesystem confinement: Pi runs with the host user's
permissions, so use an operating-system sandbox when the Workspace needs a
stronger boundary.

Pi core does not provide aggregate budget limits or per-tool approval dialogs,
so those Direct Dispatch choices are rejected instead of silently weakened.

## Ticket discovery helper

Pi has no native Linear ticket discovery. For read-only Ready Ticket and
dependency discovery, set `JJFX_PI_LINEAR_HELPER` to a dedicated helper
executable. jjfx runs it directly from the repository root with a 30-second
timeout, sends one versioned JSON request on stdin, and expects one versioned
JSON result or typed error on stdout. The helper owns credential lookup and must
provide read-only Linear access; credentials are never placed in the request or
command arguments.

Protocol version 1 accepts these requests:

```json
{"version":1,"operation":"ready_tickets","label":"ready-for-agent","status":"Todo"}
{"version":1,"operation":"dependency_graph","parent":"AMBA-40","repository":"owner/repo"}
```

A success envelope is `{"version":1,"result":{...}}`. An error envelope has this
shape:

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
