# `[agent] command` is not what runs, and the mismatch surfaces as an agent auth error

Status: needs-triage
Type: bug
Blocked by: none

## Problem

The user-global `[agent] command` names "the shell command run in a workspace's
agent pane" and defaults to `"claude"`. This machine sets it to `"opencode"`. The
Runs launched `claude` anyway, and because Claude's OAuth session is expired here
the only thing the operator saw was:

```
reason: Failed to authenticate: OAuth session expired and could not be refreshed
```

That message is about credentials. The actual problem was configuration: the
runtime that ran was not the runtime the config named. It cost a long detour
through agent auth before the configuration was even suspected.

This is the user-global half of
[01](01-a-declared-pool-profile-is-ignored.md), which covers the repository-local
half. Together they say: when a profile is in play, `[agent] command` does not
decide what runs, and nothing says so.

## Evidence

User config:

```toml
[agent]
command = "opencode"
```

The launched Run's own log is a Claude Code session - it carries
`"claude_code_version":"2.1.247"`, `"model":"claude-opus-5[1m]"`, and a
`permissionMode`, none of which `opencode` emits:

```
$ head -c 200 .jj/pools/ledger/worker-bea63d.log
{"type":"system","subtype":"hook_started",...,"hook_name":"SessionStart:startup",...}
```

and the terminating record:

```json
{"type":"result",...,"error":"authentication_failed","is_api_error_message":true,
 "result":"Failed to authenticate: OAuth session expired and could not be refreshed"}
```

There is no profile declared in the user config and none on the pool, so nothing
in the configuration explains why `claude` ran.

## Acceptance Criteria

- [ ] With `[agent] command = "<runtime>"` set and no profile declared anywhere,
      that command is what runs - or the config is rejected with a message saying
      a profile must name the runtime.
- [ ] When a profile *is* in play, `jjfx status` (or the dispatch output) says
      which runtime will run, so `command` and the profile cannot disagree
      silently.
- [ ] A Run that fails because its runtime cannot authenticate says which runtime
      failed, not only that authentication failed.
- [ ] The docs for `[agent] command` and `[profiles]` state which wins, with a
      test pinning it.
