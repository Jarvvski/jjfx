# wsg Go/Rust conformance

Status: migration-era

During the wsg migration, the Rust `wsg` binary (`crates/wsg`, over the shared
`wsg-core` library) is verified against the Go `wsg` implementation acting as a
temporary oracle. The Go compatibility peer is source-validated at commit
`b85c8e8b24fdf5c5c39e7ceb6941cf045e8b3a10` of the wsg repository.

This suite exists only while both implementations are live. When the Go
repository is deprecated (`.scratch/wsg-migration/issues/18-deprecate-the-go-repository.md`)
it is retired; the persisted contract it guards is recorded in
[`wsg-compatibility-contract.md`](wsg-compatibility-contract.md).

## Running

Conformance requires explicit paths to the Go oracle binaries. A missing oracle
is an actionable setup failure, never a passing skip - `mise run check` stays
self-contained and never claims parity on its own.

Build the oracle from the wsg repository, then run the suite from this one:

```bash
# in the (archived) Go wsg repository
go build -buildvcs=false -o /tmp/wsg-oracle/wsg ./cmd/wsg
go test -c -o /tmp/wsg-oracle/wsg.test ./cmd/wsg

# in jjfx
WSG_GO_BINARY=/tmp/wsg-oracle/wsg \
WSG_GO_TEST_BINARY=/tmp/wsg-oracle/wsg.test \
mise run conformance
```

- `WSG_GO_BINARY` - the public Go `wsg` CLI.
- `WSG_GO_TEST_BINARY` - the Go test binary exposing `TestStateLockSubprocessHelper`.

The Rust side is the `wsg` binary the harness builds (`CARGO_BIN_EXE_wsg`) plus
the Rust-only `conformance_lock_helper` test.

## Two layers

1. **Public layer** - both binaries run against temporary Repositories and their
   CLI outcomes are compared, in both implementation orders.
2. **Private deterministic layer** - the Rust `conformance_lock_helper` and the
   Go `TestStateLockSubprocessHelper` reproduce lock barriers, interrupted
   writes, and fake Agent Runtime process trees that public CLI inputs alone
   cannot make deterministic.

## Strict versus semantic comparison

**Strict** (bytes or exit status must match): machine-readable stdout, the
stdout/stderr channel roles, success/failure exit outcomes, persisted state
documents (`.jj/pool.json`, `.jj/pool/<worker>.json`, Dispatch Group documents),
lock sidecars, and unknown-field preservation across a read-modify-write.

**Semantic** (contract-aware normalization): human-facing terminal text. The
recorded accepted differences are:

- Worker contention wording may be `No idle workers` or `No more idle workers`.
- A concurrent-mutation conflict may read `conflicted with another process` or
  `persisted state changed`.
- Help text is compared by required command presence (`wsg add`, `wsg pool`,
  `wsg dispatch`), not byte equality.
- Pool and status summaries are compared by required substrings (`idle`,
  `(N total)`), not full formatting.

## Scenarios

- Workspaces created by one binary are listed and removed by the other.
- Pool growth, resize, and destruction round-trip between binaries.
- Mixed concurrent pool mutation under the shared lock: no double-reserve, no
  deadlock, no lost state, no malformed JSON, no mismatched Workspace membership.
- CLI contract inventory over both adapters: `version`/`--version`, `root`,
  `where`/`info`, `add`/`a`, `ls`, `remove`, `sync`/`refresh`, the `pool` and
  `status` aliases, `completion`, an unknown command, and `path`.
- Runtime process leaders and descendants are reconciled and cleaned across the
  implementation boundary.
- Dispatch Group progress is created by one binary and resumed by the other,
  including dependency waves and persisted assignments.
- Interrupted Pool and Worker replacement preserves valid old-or-new state,
  unknown fields, and stale-revision protections.
