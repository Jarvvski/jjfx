# wsg compatibility contract

Status: provisional

This document is the language-neutral contract for the Workspace Dispatch
migration. It is intentionally limited to the surfaces named by the migration
PRD and the fixtures in `crates/wsg-core/tests/fixtures/compatibility/`.

The Go wsg and jj-wsx sources are not present in this repository, so the
fixtures currently express the PRD's provisional contract. Ticket 03 must be
revisited against the current Go implementation before Rust writes or mutates
any Worker Pool state.

## Persisted surfaces

- `.jj/ws-cache`
  - Contract: UTF-8 lines in `name<TAB>path<LF>` order. Paths and names are
    not trimmed. Empty, malformed, and missing files are distinct input cases.
  - Missing state: a missing file is treated as an empty optional surface by
    readers.
- `.jj/pool.json`
  - Contract: a UTF-8 JSON object with a numeric `version` and a `workers`
    array. The empty pool is represented by `workers: []`.
  - Missing state: no pool file is not a valid empty write. Readers must report
    whether the file is absent.
- Worker state
  - Contract: a UTF-8 JSON object with `version`, `worker_id`, `workspace`,
    `status`, and execution metadata. `ticket`, `run`, `started_at`, and
    `last_activity_at` are explicit `null` when a value is known to be absent.
  - Missing state: legacy readers must tolerate an omitted `agent_runtime`
    field. Unknown fields must not be discarded by a future round trip.
- Dispatch Group state
  - Contract: a UTF-8 JSON object with `version`, `parent_ticket`, `status`,
    and a stable-order `sub_issues` array. Each Sub-issue has a Ticket, status,
    dependency list, Worker reference, and Run reference.
  - Missing state: an unassigned Worker or Run is represented by `null`.
    Status values remain strings until the typed model is introduced.

The fixture matrix covers empty and populated pools, idle/busy/done/failed
Workers, Claude Code and Codex Agent Runtimes, all six named Dispatch Group
statuses, explicit `null` values, a tolerated legacy omission, and malformed
or whitespace-sensitive `ws-cache` lines.

## Status values

The provisional fixture vocabulary is:

- Worker Status: `idle`, `busy`, `done`, and `failed`.
- Dispatch Group status: `pending`, `dispatched`, `retried`, `done`, `failed`,
  and `merged`.
- Agent Runtime: `claude` and `codex`.

These values describe persisted observations only. They do not authorize a
mutation or imply that a Worker is an Agent, Agent Session, Run, or Workspace.
A Worker remains an execution slot backed by a Worker Workspace. A Run remains
one attempt by an Agent Runtime, while an Agent Session may span multiple Runs.

## Replacement and locking rules

The compatibility implementation must preserve these rules:

1. A state replacement writes a temporary file in the same directory as the
   target, flushes and closes it, then renames it over the target. Readers see
   either the previous complete document or the next complete document, never a
   partial document.
2. Pool mutations acquire one repository-wide pool mutation lock, reload the
   current `.jj/pool.json` after acquiring it, validate the mutation, and
   replace the file while holding the lock.
3. Worker mutations acquire the sidecar lock for the affected Worker, reload
   that Worker's state after acquiring it, and replace only that Worker's state.
4. Dispatch Group mutations acquire the Dispatch Group sidecar lock, reload
   the group after acquiring it, and replace only that group's state.
5. Lock files remain sidecars. Renaming a state file must not change the inode
   used by a lock or allow two independent processes to mutate stale state.
6. A failed serialization, flush, or rename leaves the last valid target file
   in place.

The exact Go lock filenames and sidecar directory names are not discoverable
from the current repository and remain a required source-validation item
before production mutation work. No Rust mutation is enabled by this contract
change.

## Process and output rules

Worker execution state may include a PID and process-group identity. Readers
must distinguish a live process from a reaped process and must not infer a
successful Run from a stale PID. Reconciliation is derived state until the
persistence ticket explicitly enables a compatible write.

The command-facing compatibility surface separates machine-readable values
from human messages:

- `wsg --help` / `wsg -h`: help text on stdout, empty stderr, exit 0.
- `wsg --version` / `wsg -V`: `wsg <version>` on stdout, empty stderr, exit 0.
- `wsg` inside a Repository during the foundation stage: capability/status
  text on stdout, empty stderr, exit 0.
- Repository or state failure: no machine value on stdout, a contextual human
  error on stderr, and a non-zero exit.

The full command, alias, option, confirmation, completion, and exit inventory
for the Go implementation remains a follow-up to source validation. Later CLI
tickets must extend this table rather than define a second output contract.

## Fixture ownership

Contract tests load the fixtures through the `wsg-core` test seam and assert
field presence, explicit `null` versus omission, status coverage, byte order,
and malformed-line cases. The fixtures contain representative data only; they
must not copy Go implementation code. Once the Go source is available, update
the fixtures and this document in the same focused change, then add exact
round-trip assertions before enabling any mutation.
