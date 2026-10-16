# Local tickets

`source = "local"` discovers Ready Tickets from the repository's own markdown
tickets instead of Linear. It reads the convention this repository already uses
for its own work - `.scratch/<feature>/issues/<NN>-<slug>.md`, with `Key: value`
metadata lines ([issue tracker conventions](agents/issue-tracker.md)) - and needs
no Agent Runtime and no Linear.

```toml
[tickets]
source = "local"
```

## Layout

Every file matching `<root>/<feature>/issues/<NN>-<slug>.md` is a Ticket, where
`<root>` is `tickets.local.root` (default `.scratch`) and `<NN>` is the ticket
number. Files that do not match are ignored, so a `README.md` beside them is
harmless.

The header carries the metadata the convention defines:

```markdown
# Ship the widget

Status: ready-for-agent
Type: task
Blocked by: 01, 02

## Problem

The body is everything below the metadata lines.
```

| Line | Meaning |
| --- | --- |
| `# Title` | The Ticket's title. Required. |
| `Status:` | Its triage state. Required. |
| `Type:` | `task`, `research`, `prototype`, or `grilling`. Recorded, not used for readiness. |
| `Blocked by:` | Comma-separated sibling ticket numbers, or `none`. |

## Identity

A Ticket's id is path-derived and stable: `<feature>--<NN>-<slug>` - for example
`opinionated-loop--02-local-markdown-tickets`. The feature is joined to the file
stem with `--` because a Ticket id must be a single filename component (it names
Dispatch Group state files), so the `/` of the directory layout cannot appear in
it.

## Ready set

A Ticket is **ready** when its `Status:` is `ready-for-agent` and every
`Blocked by:` number resolves to a sibling in the same feature whose `Status:` is
`resolved`. A Ticket with an unresolved Blocker is not ready; resolving the
Blocker makes it ready on the next discovery.

Naming a Ticket is the override. `jjfx dispatch <ticket-id>` reports why the
Ticket is not ready - the wrong `Status:`, or the unresolved Blocker and its
status - and dispatches it anyway; the reason comes from the same readiness
check `--all` reports with, so the two paths cannot disagree. It is a
diagnostic on stderr, alongside the dispatch result rather than in place of it.

## Dependencies

`Blocked by:` maps onto the existing Dispatch Group. The **Parent** of a group is
the feature directory, and its direct children are that feature's Tickets, so
`jjfx dispatch <feature>` orchestrates the whole feature in dependency order. A
Parent that is not a feature directory has no children, so orchestration falls
back to dispatching it directly - which is what `jjfx dispatch <ticket-id>` does.

## Prompt inlining

The file body below the metadata lines is inlined into the Worker's prompt the
same way a Linear title and description are, so a Worker needs no tracker tools.

## Writeback

jjfx keeps the files in step with the Worker Pool:

- A Worker that reserves a local Ticket sets its `Status:` to `claimed`.
- A Ticket whose Run finishes sets its `Status:` to `resolved` and appends a
  dated line under the file's `## Comments` heading, adding the heading when the
  file has none.

Only the `Status:` line and the appended comment are written, so the rest of the
file is preserved byte for byte. Both steps are idempotent: a repeated reconcile
never appends the comment twice.

The completion signal is the durable Run Record store (`.jj/pool/runs.jsonl`), not
live Worker state: a Run appends its record when it finalizes, and the record
names the Ticket, so a finished Run still resolves its Ticket after `pool reset`
cleared the Worker's Ticket association. Reconciliation runs before Dispatch
discovers ready work (so a resolved blocker readies its dependent in the same
Dispatch), on the state-reading commands (`jjfx status`, `jjfx pool list`), and
on demand with `jjfx ticket reconcile`, which needs no ready Ticket and no
Dispatch. A Ticket a live Worker is still running is in flight, not finished, so
it is never resolved from a stale record.

## Diagnostics

A file that breaks the convention - no `# ` title, no `Status:` line, or an
unparseable `Blocked by:` - is reported as a `DiscoveryDiagnostic` naming the
file, never silently skipped. A discovery that returns no Ready Tickets still
reports them, so an empty ready list is never indistinguishable from files that
were dropped.

## Ad-hoc tickets

Not every piece of work is a tracker item, and a repository that has never been
set up for jjfx should still be able to dispatch. `dispatch` takes a Ticket
handed in directly, with no source at all:

```bash
jjfx dispatch --text "Fix the login bug; it fails on Safari"
jjfx dispatch --from-file notes/plan.md      # any markdown file, wherever it lives
git log --format=%B -1 | jjfx dispatch -     # or stdin
```

An ad-hoc Ticket never consults `[tickets]`, so it works with no configuration -
the default `linear` source wants an API key a bare repository does not have.
It is dispatched straight to a Worker, the same Direct Dispatch path a resolved
Ticket takes: it has no Parent to orchestrate and no file to write back, so it is
always a single direct request.

`--id` names it explicitly. Without one the id is derived: `ad-hoc--<slug>`,
slugged from the file stem for `--from-file`, or the first words of the text
otherwise. The title and body are read the same way as a local Ticket's - the
first `# ` heading is the title and the rest is the body, so the Worker prompt is
identical whichever input was used.

