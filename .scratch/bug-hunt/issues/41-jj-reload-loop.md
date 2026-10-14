# A self-triggering `.jj` reload loop storms `jj` and `kitten @ ls`

Status: ready-for-agent

Source: external report (`/tmp/jjfx-bug-kitty-ls-storm.md`, 2026-10-14), confirmed
against the tree at `215602d`. Evidence reproduced below because the report is
ephemeral.

## Problem Statement

The TUI watches `<repo>/.jj` **recursively** and re-projects the Workspace list on
**any** filesystem event. The projection shells out to `jj workspace list`, and
that command **snapshots the working copy by default** - writing
`.jj/working_copy/working_copy.lock`, `.jj/working_copy/.tmp*`, and (in a
colocated repo) `.jj/repo/git_import_export.lock`. Those writes fire jjfx's own
watcher, which re-projects, which shells out again: an infinite loop.

The host terminal becomes unusable (kitty pegged at ~90-105% CPU for the whole
lifetime of the TUI) and hundreds of stray `jj` and `kitten` processes
accumulate. The only recovery is to kill jjfx and reap its children, or restart
the terminal - which destroys every in-terminal session.

The `kitten @ ls` flood is a **downstream symptom**: every reload also nudges the
dispatch poller, which calls `KittyTerminal::ls()`. Ticket 40 bounded and
single-flighted that query, which is why the flood is now capped - but the reload
loop that drives it is untouched.

## Root cause

`src/engine/workspace.rs:1164` hand-rolls the projection read **without**
`--ignore-working-copy`:

```rust
fn workspace_names(root: &Path) -> Result<Vec<String>, WorkerWorkspaceError> {
    let output = Command::new("jj")
        .args(["workspace", "list", "-T", "stringify(name) ++ \"\\n\""])
        .current_dir(root)
        .output()
        ...
```

The chain: `src/watch.rs::watch_repo` (recursive `.jj` watch, `Msg::Reload` on any
event) → `App::reload` → `Runtime::reload_store` → `Store::load` →
`engine::Workspaces::projection` → this function.

**A correct implementation already exists and the reload path does not use it.**
`src/jj.rs:84` is built on `read_at_repo`, whose doc comment names this exact
hazard: `--repository <repo_root> --ignore-working-copy` keeps it a pure read,
because "jjfx must never snapshot the working copy (that would churn commits and
ping its own watcher, ADR 0006)". The engine has a second, divergent copy, and
the two drifted.

### Why it is repo-dependent

A clean, non-colocated repo may write nothing on `jj workspace list`, so the loop
does not sustain. A **colocated** repo writes `git_import_export.lock` on
essentially every `jj` invocation, and a **constantly-dirty working copy** (agents
and dev servers writing files) forces a snapshot each time. Either guarantees a
write per invocation and a self-sustaining loop. Both observed incidents were in
a colocated repo.

## Evidence (controlled repro, isolated from the terminal)

A fake `kitten` on `PATH` logs argv and returns instant JSON; an empty
`XDG_CONFIG_HOME` makes jjfx drive the inherited kitty, so every call is
intercepted.

| Observation | Value |
| --- | --- |
| `kitten @ ls` invocations | 742 in 10s (~74/s), then 1604 in 12s (~134/s) - every argv identical |
| Concurrent `jj workspace list` processes | 460 at one instant, all children of the jjfx pid |
| `.jj` filesystem events | ~14/s, all `working_copy.lock`, `working_copy/.tmp*`, `repo/git_import_export.lock` |
| Against an *instant* fake kitten | still storms - a genuine tight loop, not a latency-feedback effect |

The live argv matched exactly:
`/opt/homebrew/bin/jj workspace list -T stringify(name) ++ "\n"` - no
`--ignore-working-copy`, no `--repository`.

## Solution

Make the engine's projection read a **pure read**: add `--ignore-working-copy`
(and consider `--repository <root>`, matching `jj::read_at_repo`). `--ignore-working-copy`
means the projection will not observe *unsnapshotted* working-copy edits, which is
irrelevant here - this reads workspace **names** only.

Better, but a wider change: delete the engine duplicate and share one
implementation with `jj::read_at_repo`, so the two cannot drift again. The engine
is the layer the `wsg` binary builds on, so whichever way this goes, the shared
helper has to stay engine-safe.

## Acceptance Criteria

- [x] The Workspace projection does not snapshot the working copy: a dirty
      working copy is left untouched by a projection, and `jj workspace list`
      runs with `--ignore-working-copy`.
- [x] A regression test fails without the flag and passes with it
      (`workspace_names_does_not_snapshot_the_working_copy`).
- [x] The other engine `jj` invocations are checked for the same omission - see
      the Comments.

## Out of Scope

- The `kitten @ ls` flood - already bounded and single-flighted by ticket 40.
- The `Cmd::run` timeout and the `ls` cache - already landed with ticket 40.
- Making the watcher ignore its own churn (`working_copy/`, `*.lock`, debouncing).
  Worth doing as defence in depth, but the root fix removes the churn, and every
  other jjfx-initiated `jj` write is an intended repository change where a reload
  is correct.
- The two `jj git remote list` reads below - a separate, non-looping defect.

## Comments

**Filed 2026-10-14.** Ticket 40 (`kitty-ls-storm`) was filed from this same report
and fixed the *symptom* - the unbounded, un-deduplicated `kitten @ ls`. The
report's **primary** finding, the reload loop, was not ticketed. This ticket is
that finding, and the report's own secondary recommendations are already done.

**The check of the other engine `jj` invocations (2026-10-14).** Probed directly
in a fresh colocated repo, each command run against a freshly-dirtied working
copy and the working-copy commit id compared before and after:

| Command | Result |
| --- | --- |
| `jj workspace list -T 'stringify(name) ++ "\n"'` | **SNAPSHOTS** - the loop |
| `jj git remote list` | **SNAPSHOTS** |
| `jj config get <key>` | pure read |
| `jj log -r @` | **SNAPSHOTS** |

So the projection was not the only offender. `jj git remote list` is run twice
from the engine - `direct_dispatch.rs:590` and `pool.rs:1428`, both dispatch
identity checks - and each one snapshots the working copy, which ADR 0006
forbids for the same reason: it churns commits and pings the watcher. Neither
loops, because neither is on the reload path, so they are **not** fixed here;
folding a second defect into this change would blur what it attributes. They
want their own one-flag fix and a test.

The remaining engine calls are all intentional mutations (`workspace add`,
`workspace forget`, `new`, `git init`) or working-copy-independent reads
(`--version`, `config get`), where a snapshot is either wanted or cannot happen.
