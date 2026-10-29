# `[agent] command` is not what runs, and the mismatch surfaces as an agent auth error

Status: ready-for-human
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
      a profile must name the runtime. **Answered, not implemented, in #204**:
      `[agent] command` is the *interactive pane* command, and a Run is launched
      headlessly by jjfx from the pool's profile. The two cannot be one string -
      the pane runs `opencode`, the Run runs `opencode run --format json ...` -
      so this criterion's premise does not hold. What was missing is the
      statement, now in the docs (below), not a code change.
- [x] When a profile *is* in play, `jjfx status` (or the dispatch output) says
      which runtime will run, so `command` and the profile cannot disagree
      silently. **Done in #204**: `status` prints `Profile (<pool>): <runtime>`
      for every pool with a profile, and the profile is now actually applied
      (finding 01), so the line is truthful.
- [ ] A Run that fails because its runtime cannot authenticate says which runtime
      failed, not only that authentication failed. **Open**: the failure text is
      the agent's own, and jjfx records it verbatim; naming the runtime on the
      failure line is a separate change.
- [x] The docs for `[agent] command` and `[profiles]` state which wins, with a
      test pinning it. **Done in #204**: `[agent]`'s key now says plainly that it
      does not choose a Run's runtime, a new `### Which runtime runs` section
      states the resolution order, and
      `a_declared_pool_profile_is_the_runtime_that_runs` pins that the profile is
      what decides.

## Comments

- 2026-10-29: #204 documents the precedence and makes `status` truthful. The
  cost of the gap was real - the drive spent a long detour in agent-auth
  troubleshooting because the runtime that ran was not the one configured, and
  nothing said so. The fix is the statement plus the actual application of the
  profile (finding 01); the "name the runtime on an auth failure" criterion is
  left open, since the message is the agent's own.
