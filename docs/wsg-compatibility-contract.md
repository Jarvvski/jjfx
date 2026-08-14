# wsg compatibility contract

Status: source-validated

This document records the persisted Workspace Dispatch contract shared by Go
wsg and the Rust implementation in `wsg-core`. The Go compatibility peer is
commit `b85c8e8b24fdf5c5c39e7ceb6941cf045e8b3a10` from the wsg repository.
The fixtures in `crates/wsg-core/tests/fixtures/compatibility/` are representative
Go-compatible documents.

jj-wsx is not part of this contract. It is an obsolete predecessor to jjfx and
does not read Worker Pool state.

## Persisted surfaces

### Workspace cache

`.jj/ws-cache` contains UTF-8 lines in `name<TAB>path<LF>` order. Names and
paths are not trimmed. Empty, malformed, and missing files remain distinct
inputs. A missing cache is an empty optional read surface.

### Worker Pool

`.jj/pool.json` is a UTF-8 JSON object with these fields:

- `size`: integer Worker count
- `gh_repo`: GitHub repository identifier
- `workers`: stable-order array of Worker identifier strings
- `created_at`: timestamp string
- `foreground`: optional boolean, omitted when unset
- `agent`: optional Agent Runtime string, omitted when empty
- `names`: optional Worker-to-alias object, omitted when empty

A missing pool file is not an empty pool. Readers report absence separately
from malformed state.

### Worker

`.jj/pool/<worker>.json` is a UTF-8 JSON object with these fields:

- `status`: Worker Status string
- `agent`: Agent Runtime string or `null`
- `ticket`: Ticket string or `null`
- `pid`: integer or `null`
- `started_at`: timestamp string or `null`
- `completed_at`: timestamp string or `null`
- `log_file`: path string or `null`
- `branch_name`: bookmark string or `null`
- `exit_code`: integer or `null`
- `error`: string or `null`

All pointer-backed Worker fields are emitted explicitly as a value or `null`.
An empty branch name is canonicalized to `null`. Unknown fields survive a
read-modify-write through both cooperating implementations.

### Dispatch Group

`.jj/pool/dispatch-<lowercase-parent>.json` is a UTF-8 JSON object with:

- `parent`: Parent Ticket identifier
- `created_at`: timestamp string
- `gh_repo`: GitHub repository identifier
- `sub_issues`: object keyed by Ticket identifier
- `opts`: Dispatch Group options object

Each Sub-issue contains `title`, `status`, `blocked_by`, `worker`, `branch`,
`dispatched_at`, `completed_at`, optional `skip_reason`, and `retries`.
Unassigned Worker, branch, and timestamp values are explicit `null`.
`skip_reason` is omitted when absent. Options contain optional `agent` and the
required `model` string.

## Persisted vocabulary

Worker Status values currently written by Go are `idle`, `busy`, `done`, and
`failed`. Sub-issue Status values are `pending`, `dispatched`, `done`, `failed`,
and `skipped`. Agent Runtime values currently written by Go are `claude` and
`codex`; Rust additionally writes the canonical `pi` value when a pool is
configured for the Pi runtime.

Rust persistence keeps these values as open strings so an additive runtime
value does not make the document unreadable. Lifecycle modules interpret the
known values separately.

## Pi Worker actions

Rust supports Pi 0.84.x as an additive Worker runtime. Follow-up and Mount
resolve a prior Pi Session only from a valid v3 Pi session header and otherwise
report the existing explicit fresh-session reason. Runtime identity remains the
canonical `pi` Worker value. Provider and model are typed action inputs and are
not added to the Go-compatible Pool or Worker documents.

Pi commands use `.jj/pool/pi-sessions`, suppress inherited extensions, skills,
prompt templates, themes, context files, and project trust, and select the
fixed `read,bash,edit,write,grep,find,ls` tool allowlist. These controls do not
sandbox the filesystem. Aggregate budgets, per-tool approval dialogs, and Pi
ticket discovery are unsupported by this contract and never fall back to a
different Agent Runtime.

Run supervision remains provider-neutral. Pi uses the same Reservation, PID,
process-group cleanup, Reset, stale-finalization, and terminal persistence
rules as Claude and Codex.

## Lock protocol

Lock files are stable sidecars because atomic replacement changes the state
file inode.

- Pool state uses `.jj/pool/.dispatch.lock`.
- Worker state uses `.jj/pool/<worker>.json.lock`.
- Dispatch Group state uses
  `.jj/pool/dispatch-<lowercase-parent>.json.lock`.
- Rust Workspace preparation and Reset restoration additionally serialize on
  `.jj/pool/<worker>.workspace.lock`. This additive operation lock is never a
  state lock and is not held by the Go compatibility peer.

A single-Worker mutation takes that Worker's lock. A Pool mutation takes the
Pool lock. A Pool operation that also decides from Worker state takes the Pool
lock first, then all affected Worker locks in deterministic filename order.
A Dispatch Group mutation takes the Pool lock first, then its Dispatch Group
lock. Pool destruction uses the same ordering and leaves lock sidecars in
place.

Every mutation reloads its target after acquiring the required locks. Rust
then compares the exact loaded bytes with its opaque expected revision. A
mismatch is a conflict, not permission to overwrite newer state. No Agent
Runtime, network, or Workspace command runs while a state lock is held.

## Atomic replacement

Writers serialize the complete next document before touching the target. They
create a uniquely named temporary file in the target directory, set mode
`0644`, write all bytes, sync and close the file, then rename it over the
target. Documents use two-space pretty JSON with a trailing newline.

Serialization, temporary creation, write, sync, close, and rename failures
leave the previous valid target intact. Temporary files are cleaned up on
failure. Parent-directory sync and network-filesystem lock semantics are not
part of the current contract.

Readers do not take locks. Atomic rename ensures they see a complete previous
or next document. Aggregate snapshots are observational and may combine state
from adjacent commits, so lifecycle decisions must use repository commits
rather than snapshots.

## Process and output rules

Worker process identifiers are liveness hints, not Run identity. Go wsg starts
an Agent Runtime in its own process group, probes liveness with signal 0, sends
`SIGTERM` to the group, waits one second, and sends `SIGKILL` if its leader
remains live. Rust uses safe standard-library and rustix interfaces for the
same Unix behavior.

The compatibility CLI reserves stdout for machine-readable values and stderr
for human messages. Help and version output succeed on stdout. Repository or
state failures write contextual errors to stderr and return a non-zero status.

## Conformance

Rust tests exercise state only through public repository `load` and `commit`
operations. They cover missing and malformed state, Go fixture round trips,
explicit null and omission semantics, unknown fields, stale revisions, failed
replacement, and identifier safety.

Bounded subprocess tests prove independent Rust writers detect lost updates.
The mixed suite uses the Go test binary from the source-validated peer and
proves both implementations honor all three lock sidecars and read each
other's Pool, Worker, and Dispatch Group writes.
