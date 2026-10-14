# Pi manual acceptance - evidence

Superseded 2026-10-14: ticket 06 is `wontfix` - the owner uses opencode now, so
this matrix will not be run. Kept as the record of the step-1 review.

Parent: `issues/06-run-pi-manual-acceptance.md`

This file is the record for ticket 06. Step 1 (the review below) is done. The
matrix is blank and waits for the owner's session against the released candidate.

## How to use this

Fill every matrix row with **expected** and **observed**, then the cleanup and a
status. Rules carried from the ticket's Decision Document:

- Use a **released candidate artifact with recorded checksums**, not a
  development build.
- **Never** copy real credentials, tokens, or private prompt contents into this
  file. Record the configuration *source* and the capability outcome only.
- An unavailable capability is an explicit, owner-signed-off **accepted
  difference** - never a silent fallback to Claude or Codex.
- A **failed cleanup or a stale process is a blocking result** until resolved
  and re-tested.
- No existing pool, workspace, session, hook file, or provider installation is
  reset, deleted, or migrated without owner approval for that operation.
- Re-check representative Claude **and** Codex paths afterwards, even though
  their automated tests are green.

## Step 1 review (2026-10-14, before touching a live workspace)

### Candidate artifact

| What | Command |
| --- | --- |
| Stage both binaries off `PATH` | `mise run install-candidate` |
| Bundle + sha256 for a target | `mise run release` → `dist/jjfx-<v>-wsg-0.19.0-<target>.tar.gz` |
| Verify a bundle | `mise run release-smoke` |

`install-candidate` is documented as never touching the installed Go `wsg` or
the `qwe` alias. Record the archive path and its `shasum -a 256` output here.

### Versions and configuration sources

| Tool | Command | Recorded |
| --- | --- | --- |
| jjfx | `jjfx version` | |
| Pi | `pi --version` | |
| Claude Code | `claude --version` | |
| Codex | `codex --version` | |
| jj | `jj --version` | |
| gh | `gh --version` | |
| Terminal | `echo "$TERM"` / `kitty --version` | |
| Discovery helper | `echo "$JJFX_PI_LINEAR_HELPER"` | |
| Pool profile | `jjfx pool list --json` | |

Also record the operator, date, platform, and the **configuration sources** (not
their contents): the repository's pool profile, the Linear MCP server entry, and
whether the pinned `pi-mcp-adapter` package is installed.

### What the product actually exposes

The CLI surface for the matrix, from `jjfx --help`:

- Pool: `jjfx pool <N>`, `pool list [--json]`, `pool rm`, `pool reset`,
  `pool profile <runtime> --provider --model`, `pool destroy`.
- Dispatch and sessions: `jjfx dispatch <TICKET>...` (`--provider/--model/--force`),
  `send <worker> <prompt>`, `review <worker>`, `logs <worker>`, `mount <worker>`,
  `rebase <worker>`, `open-pr <worker>`.
- Observability: `jjfx status` (alias for `pool list`), `jjfx version`.

Pi-specific surfaces, from `docs/pi-workers.md`:

- Runtime selection: `jjfx pool profile pi --provider <provider> --model <model>`.
- Discovery: `JJFX_PI_LINEAR_HELPER`, a read-only helper run from the repository
  root with a 30-second timeout, one versioned JSON request on stdin and one
  versioned JSON result or typed error on stdout.
- Session storage: the repository-owned `.jj/pool/pi-sessions` directory.
- Run policy: ignores inherited extensions, skills, prompt templates, themes,
  context files and project trust; loads only the pinned adapter; disables
  approval prompts; allows `read,bash,edit,write,grep,find,ls` plus the three
  Linear tools.

### Known differences to expect (not to discover)

From the capability matrix in `spike-01-contract-findings.md` - these are
**expected** results, so a row that hits one is a pass with a recorded
difference, not a bug:

- Per-tool approval popup: **unsupported in Pi core**. jjfx rejects the Direct
  Dispatch choice rather than silently weakening it.
- Aggregate budget limits: **unsupported in the Pi CLI**. Rejected, not ignored.
- Filesystem sandbox: **unsupported in Pi**. Pi runs with the host user's
  permissions; jjfx constrains cwd only.
- Native Linear/MCP: **unsupported**. Discovery needs the helper, or a
  preflighted optional adapter whose exact tool names and schemas check out.
- Optional MCP direct tools: **conditional** - must fail visibly when absent.
- Claude/Codex discovery fallback: **rejected** - must never happen silently.

### Discrepancies found in this review

1. **`jjfx hooks status` does not exist.** The ticket's commit 3 says to
   "verify `jjfx hooks status` and install behavior", but `jjfx --help` has no
   `hooks` subcommand, and `docs/workspace-hooks.md` documents hooks as
   repository files rather than a CLI surface. The matrix below therefore
   exercises hook *behaviour* (a hook firing during a Run) and confirms no
   hook file is modified - there is no status command to call. The ticket text
   should be corrected.
2. Nothing else in the matrix was found to reference a command or capability
   that jjfx does not expose. The remaining inputs above are all reachable.

## Acceptance matrix

| # | Scenario | Action | Expected | Observed | Cleanup | Status |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | Non-destructive baseline | Capture a before/after inventory of pool, workspaces, `.jj/pool/pi-sessions`, and hook files | Identical before and after except the rows explicitly exercised | | | |
| 1 | Candidate artifact | Stage with `install-candidate`; checksum the bundle | Versions match the recorded manifest; installed Go `wsg` and `qwe` alias untouched | | | |
| 2 | Pi capability preflight (configured) | `jjfx pool profile pi --provider --model`, then `jjfx pool list` | Profile persisted; Pool view shows `pi`; no Claude/Codex substitution | | | |
| 3 | Pi capability preflight (missing setup) | Dispatch with the adapter/helper absent | Fails with sanitized setup guidance **before** any Pool, Reservation, assignment, or Workspace mutation | | | |
| 4 | Interactive lifecycle | Launch Pi interactively; observe working / waiting / ended | Only observed states are shown; TUI identity correct; attention not invented from a quiet stream | | | |
| 5 | Session switching | Switch between Pi sessions | Identity follows the session, not cwd/filename/timestamp/pid | | | |
| 6 | Ticket discovery (success) | Ready-ticket discovery via `JJFX_PI_LINEAR_HELPER` | One versioned JSON result; discovery stays out of the worker transcript | | | |
| 7 | Ticket discovery (failure classes) | Helper returns unsupported / authentication / timeout / malformed | Explicit typed failure; **never** an empty ticket list and never a Claude/Codex fallback | | | |
| 8 | Fresh Direct Dispatch | `jjfx dispatch <TICKET>` on Pi | Runs with the fixed tool policy; structured activity/result log; terminal outcome; state finalized | | | |
| 9 | Dispatch failure cleanup | Force a failing Dispatch | Failure recorded in the log; Worker returned to a usable state; no stray descendants | | | |
| 10 | Dispatch Group | Dispatch a group (when enabled) | Runtime identity, progression, restart, completion and failure handling all correct | | | |
| 11 | Follow-up / send | `jjfx send <worker> <prompt>` | Same runtime and session; delegation rules preserved | | | |
| 12 | Session resume + fresh fallback | Resume a session, then force the fallback | Resume preserves identity; fallback starts a *new* session rather than silently continuing | | | |
| 13 | Interactive mount | `jjfx mount <worker>` | Mounts with the built-in coding-tool policy; unmounts cleanly | | | |
| 14 | Observability | `jjfx logs`, `jjfx status`, current activity, usage/cost | Activity and terminal result visible; provider/model and adapter config deliberately omitted | | | |
| 15 | Reset + cleanup | `jjfx pool reset <worker>` | Worker idle; **every descendant gone**; Workspace restored and usable; terminal intact | | | |
| 16 | Hooks (behaviour) | Trigger a Run in a repo with a hook configured | Hook fires as documented; no hook file is modified by jjfx | | | |
| 17 | Claude regression | Repeat the interactive, Dispatch, log, follow-up, mount and reset paths on Claude | Unchanged behaviour | | | |
| 18 | Codex regression | The same paths on Codex | Unchanged behaviour | | | |

## Accepted differences

| Difference | Why it is acceptable | Owner sign-off |
| --- | --- | --- |

## Blockers

Any failed cleanup or stale process goes here and blocks acceptance until it is
resolved and re-tested.

## Cleanup confirmation

Confirm the pool, workspaces, sessions, hook files, and provider installations
are as they were before the session, and that no test ticket or workspace is
left behind.

## Owner sign-off

- [ ] Matrix, evidence, and accepted differences reviewed
- [ ] Any remaining non-blocking limitation explicitly accepted
