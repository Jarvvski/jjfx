# Pi support contract findings

## Scope and evidence

This spike characterizes the installed Pi executable and documented Pi 0.84.x interfaces without changing production jjfx code, user Pi state, credentials, dependencies, or infrastructure.

| Item | Finding |
| --- | --- |
| Package | `@earendil-works/pi-coding-agent` |
| Version | `0.84.1` |
| Tested executable | `/Users/<user>/.local/share/mise/installs/node/26.4.0/lib/node_modules/@earendil-works/pi-coding-agent/dist/cli.js` |
| SHA-256 | `840d1e8e689ed9e4937bcb00b9a810e02a8567d9afb10a47097f11ca93ea1521` |
| Test provider | Deterministic temporary `spike` provider extension, no network access |
| Test project | Temporary project with read sentinel and write sentinel |
| Isolation | Temporary `HOME`, XDG directories, `PI_CODING_AGENT_DIR`, session directory, `TMPDIR`, and `PI_OFFLINE=1`; no credentials passed |

The executable was invoked directly for evidence. The repository's `pi` wrapper was not used as evidence for the installed executable contract.

## CLI and invocation contract

Pi documents these modes and the spike observed them as follows:

| Invocation | Result | Contract |
| --- | --- | --- |
| `pi --version` | Exit 0 and `0.84.1` | Version is queryable before a run. |
| Interactive `pi` in a PTY | TUI starts and exits 0 on Ctrl-D | Interactive mode requires PTY handling; terminal scraping is not a worker protocol. |
| `pi --print ...` | Exit 1 without credentials and reports no API key | Print mode is headless but still requires a selected provider credential. |
| `pi --mode json ...` | Emits JSONL session/event records, then exits 1 without credentials | JSON mode is the worker stream contract. Startup and provider failures must be parsed separately. |
| `pi --mode rpc ...` | Starts without a model; `get_state` returns a structured response; EOF exits cleanly | RPC is the long-lived control and interactive seam. Keep stdin open while an agent is active. |
| Unknown option | Exit 1 with `Error: Unknown option: ...` | Capability probing must fail explicitly for unsupported flags. |

The tested supported flags include provider/model selection, system prompt and append prompt, JSON/RPC mode, session/resume/fork selection, session directory, no-session, display name, tool allowlist/denylist, extension selection, project-resource suppression, trust, offline startup, and thinking level.

## JSON and session records

JSON output is newline-delimited JSON. A run can contain multiple records for one turn and must not be reduced to the last line only. The sanitized fixtures under `fixtures/json/` cover:

- v3 session startup with `id`, `timestamp`, and `cwd`;
- agent and turn start/end records;
- assistant thinking, text, and tool-call content;
- tool execution start/end with arguments, content, and `isError`;
- tool-result messages;
- provider usage and cost fields;
- provider failure with `stopReason: "error"` and `errorMessage`;
- successful terminal assistant output; and
- malformed and unknown records for parser tolerance.

Observed assistant usage contains input, output, cache-read, cache-write, total-token, and cost values. Cost values are provider-reported accounting data. The adapter must preserve them when available and must not manufacture values when absent.

Recommended parser behavior:

1. Read one line at a time and tolerate unrelated or malformed lines according to the run policy.
2. Recognize the session header and retain the session ID before processing activity.
3. Convert assistant, tool-call, tool-result, usage, cost, and failure fields into private Pi adapter values.
4. Treat `agent_end`, terminal provider errors, and process exit as separate facts. A process exit without a terminal event is incomplete.
5. Preserve provider-neutral jjfx run and activity values at the public boundary.

## Session identity and resume

Pi session files are JSONL under a project-derived directory. The documented current session format is version 3. The first record identifies the session and includes the working directory.

Observed behavior:

- A fresh session writes a v3 header with a UUID-like `id`, timestamp, and stored `cwd`.
- Resuming by explicit session path preserves the session ID, session file, and stored working directory.
- Resuming by a unique partial ID resolves the existing session.
- Forking an existing session creates a new ID and records `parentSession`.
- An explicit missing session path creates a new session.
- A missing session ID fails with a non-zero status.
- Interior malformed JSONL lines were tolerated by Pi session loading.
- An unknown session version was accepted by the tested executable. The adapter must not assume that acceptance means semantic compatibility; it should report an unsupported version when required fields cannot be interpreted.

Implementation rule: use Pi's session selection flags and session header identity. Do not derive identity from the timestamp, working-directory folder name, transcript text, or process ID. Resume and fork are different operations and must remain different in jjfx state.

## Extension lifecycle

The throwaway TypeScript extension recorded events without retaining private prompts or credentials. Fixtures under `fixtures/extensions/` contain sanitized event shapes and orderings.

Observed lifecycle events include:

- `session_start` with startup reason;
- `input` with source values `interactive` or `rpc`;
- `before_agent_start` with prompt and effective system prompt;
- `agent_start` and `agent_end`;
- `turn_start` and `turn_end`;
- `message_start`, `message_update`, and `message_end`;
- `tool_execution_start` and `tool_execution_end`;
- `agent_settled`; and
- `session_shutdown` with reason `quit`.

The normal non-interactive order is broadly:

```text
session_start -> input -> before_agent_start -> agent_start -> turns/tool events -> agent_end -> agent_settled -> session_shutdown
```

Interactive Ctrl-D with no prompt produced `session_start -> session_shutdown`. An RPC stdin EOF during an in-flight tool produced `session_shutdown` before `agent_settled`; a controlled RPC run that kept stdin open until `agent_settled` produced the normal order. An RPC client must keep stdin open until it has received the completion signal when it needs complete results.

Mapping for jjfx:

| Pi evidence | jjfx interpretation |
| --- | --- |
| `agent_start`, `turn_start`, tool execution, message updates | Working |
| `session_start` before a prompt, or `agent_settled` after work | Waiting |
| `session_shutdown` and process exit | Ended |
| No observed native permission or attention event | Unsupported, not NeedsAttention |

The extension event envelope does not supply a new provider-specific session identity or a cwd in every event. Obtain identity from the session manager/header and retain the extension only as the lifecycle transport. Do not infer lifecycle state from transcript silence or a guessed session path.

## Execution policy

The policy fixtures under `fixtures/policy/` record the black-box probes.

### Model and prompt

- `--provider spike --model spike-model` selected the expected provider and model in RPC state.
- `--name spike-session-name` was reported as `sessionName` in RPC state.
- `--system-prompt` replaced the default system prompt.
- `--append-system-prompt` appended text separated by blank lines, followed by Pi's current-working-directory context.
- Thinking levels are `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, and `max`; the observed default state was `medium`.

The host must select the model and prompt explicitly. It must not let project resources or inherited global settings silently change the worker contract.

### Tools and filesystem

- `--tools read,grep,find,ls` allowed the read-only probe and a provider-requested `write` returned `Tool write not found` without changing the target.
- `--tools write` allowed a write inside the project.
- A relative `../outside-sentinel.txt` write also succeeded. Pi does not sandbox filesystem paths.

The worker adapter must use a static allowlist, a constrained working directory, and any required OS-level sandbox. Tool names are not a security boundary by themselves if write-capable tools or a broad cwd are supplied.

### Trust and approval

- `--no-approve` ignored a project-local extension.
- `--approve` loaded the same project-local extension.
- `--approve` and `--no-approve` are project-resource trust controls. No native per-tool approval event or permission dialog was observed in JSON or RPC runs.

The host must decide trust before launch and must implement any human approval flow outside Pi. Do not interpret a tool failure as a permission state.

### Budget

Assistant events expose usage and cost, but the CLI has no aggregate token or cost budget flag. `--max-budget 1` failed as an unknown option. A host integration must enforce aggregate token, cost, timeout, and cancellation budgets from streamed events and explicit process control.

## Linear discovery decision

Pi core has no built-in MCP or Linear contract. The optional installed `pi-mcp-adapter` version `2.11.0` can register an `mcp` proxy and configured direct tools, but it is a separate extension and configuration capability.

Three approaches were compared:

1. **Pi-native MCP**: unsupported. Do not assume an `mcp` tool exists merely because Pi is installed.
2. **Configured `pi-mcp-adapter`**: conditionally supported. In an isolated fixture, the adapter discovered `linear_list_issues` and `linear_get_issue` and a direct `linear_list_issues` call returned bounded JSON. It registered no Linear tools when no server was configured. Proxy search is model-mediated, so discovery must use preflighted direct tool names and schemas, not free-form model search.
3. **Dedicated read-only helper**: preferred host seam. A helper can speak the configured Linear API or MCP transport directly and return schema-bound JSON without spending a Pi model turn or trusting model-generated discovery text. No such helper is part of this spike.

For issue 03, Linear discovery is a separate capability from Pi worker execution. A user's normal Pi setup may already supply the optional adapter and a Linear connector, but this spike deliberately did not inspect or use that global configuration or its credentials. Use a configured read-only helper when supplied. A configured Pi MCP adapter may satisfy the capability only when the host has preflighted the named Linear server, exact direct tools, schemas, timeout, and credentials. Otherwise return an explicit unsupported-capability error. Missing configuration, an unregistered direct tool, unavailable server, authentication failure, timeout, or malformed response must never become an empty ticket list. The adapter must never fall back to Claude or Codex commands.

Required discovery properties:

- read-only `list_issues` and `get_issue` capability;
- explicit static direct-tool or helper command configuration;
- credentials supplied through the configured transport and never embedded in prompts or command arguments;
- bounded timeout and retry classification;
- response schema validation before provider-neutral ticket conversion; and
- sanitized, actionable diagnostics.

## Capability matrix

| Capability | Status | Implementation contract |
| --- | --- | --- |
| Version probe | Supported | Query `--version`; reject unexpected versions if required by the adapter. |
| Headless worker run | Supported | Use JSON mode and parse JSONL records. |
| Long-lived control | Supported | Use RPC with stdin kept open through completion. |
| Interactive TUI | Supported as a separate UX | Use PTY and Pi UI; do not scrape it for worker results. |
| Fresh session | Supported | Capture v3 header identity and cwd. |
| Resume | Supported | Use explicit session path/ID and preserve identity. |
| Fork | Supported | Expect a new ID and parentSession. |
| Extension lifecycle | Supported with explicit extension | Load a pinned, trusted extension and map observed events. |
| Model/system prompt/name | Supported | Pass explicit flags and retain state for diagnostics. |
| Static tool policy | Supported | Use `--tools`/`--exclude-tools`; treat the result as policy, not sandboxing. |
| Project trust | Supported | Use `--approve` or `--no-approve` explicitly. |
| Per-tool approval popup | Unsupported in core | Implement approval in host code or expose only pre-approved tools. |
| Aggregate budget | Unsupported in CLI | Enforce from usage/cost/lifecycle data in host code. |
| Filesystem sandbox | Unsupported in Pi | Constrain cwd and OS permissions externally. |
| Native Linear/MCP | Unsupported | Require helper or configured optional adapter capability. |
| Optional MCP direct tools | Conditional | Preflight exact names and schemas; fail visibly when unavailable. |
| Claude/Codex discovery fallback | Rejected | Never substitute another runtime silently. |

## Rejected assumptions for issues 02-04

- Do not scrape interactive terminal output for worker activity or results.
- Do not expect one JSON record per prompt or assume the final record contains all usage and tool data.
- Do not treat `agent_settled` as process termination or process exit as proof of a complete agent turn.
- Do not infer Waiting from a quiet stream, or NeedsAttention from a provider/tool error.
- Do not assume every extension event contains session ID or cwd.
- Do not use timestamp, cwd, transcript filename, or process ID as session identity.
- Do not assume malformed interior session lines are fatal, or assume unknown session versions are semantically supported.
- Do not assume `--approve` means a per-tool approval policy.
- Do not assume Pi prevents `..` or absolute filesystem writes.
- Do not assume Pi has a total budget flag, built-in Linear access, built-in MCP, sub-agents, or permission popups.
- Do not inherit global extensions, skills, context files, themes, trust, credentials, or model selection into a worker run.
- Do not silently fall back to Claude or Codex when a Pi capability is absent.
- Do not treat an unavailable Linear capability as an empty Ready Ticket response.

## Implementation guidance for remaining tickets

### Issue 02: Pi worker runtime

Use the installed/configured Pi executable in JSON mode with explicit provider, model, session directory, session selection, system prompt, trust, resource, and tool-policy flags. Keep Pi-specific record and session types private to the adapter. Parse the stream incrementally, preserve usage/cost when present, surface startup/provider/tool failures, and combine terminal records with process status. Use session header identity for resume and distinguish fork. Bound process lifetime and enforce budgets in the host.

### Issue 03: Pi ticket discovery and dispatch

Keep discovery out of the worker transcript. Inject a dedicated read-only helper or a preflighted optional MCP adapter direct-tool capability. Validate structured results and classify unsupported, authentication, timeout, transport, and malformed-response failures. Never invoke another runtime as an implicit fallback.

### Issue 04: Pi interactive lifecycle

Use an explicit extension or another observed event transport, preferably with RPC for the long-lived control channel. Keep stdin alive until `agent_settled` and separately observe shutdown/process exit. Map only the observed Working, Waiting, and Ended states. Mark attention/approval unsupported unless a future extension supplies a real event, and keep host UI policy separate from Pi's tool execution.

## Fixture inventory and validation

Sanitized fixtures are under:

- `fixtures/cli/capabilities.json`
- `fixtures/json/`
- `fixtures/sessions/`
- `fixtures/extensions/`
- `fixtures/policy/`
- `fixtures/linear/`

They use placeholder paths, IDs, prompts, and issue data. No live Linear account or credential was used. The temporary harness and all temporary configuration remain outside the repository.
