# Adopt Workspace Dispatch as an orchestration layer

Status: accepted

## Context

ADR 0001 deliberately postponed Worker Pool orchestration while jjfx focused
on observing Workspace and Agent lifecycles. The wsg migration now requires
execution slots, Ticket routing, and restart-safe orchestration to live beside
that observation model. Reusing terms such as Worker, Agent, Agent Session, Run,
and Workspace without defining their relationships would make the shared Rust
implementation ambiguous.

The migration PRD requires jjfx and the compatibility `wsg` binary to share one
implementation while remaining compatible with the existing Go-created state.
The new layer must therefore add orchestration without replacing the existing
Workspace model or changing persisted compatibility surfaces prematurely.

## Decision

jjfx adopts **Workspace Dispatch** as an orchestration layer over its existing
Workspace, Agent, Agent Session, Work, and Forge model.

- jjfx may own active orchestration as well as observation. Workspace Dispatch
  includes the Worker Pool, Reservations, Runs, Direct Dispatch, and
  dependency-aware Dispatch Groups described in the migration PRD.
- The Agent and Work lifecycles remain orthogonal. Workspace Dispatch adds a
  separate Worker Status lifecycle for execution capacity; it does not collapse
  these axes into one state.
- A Worker is a reusable execution slot backed by one Worker Workspace. A
  Worker is not an Agent, Agent Session, Workspace, Run, or operating-system
  process.
- A Run is one execution attempt by an Agent Runtime. An Agent Session may
  continue across several Runs, including Runs that use replacement processes.
- A Worker Workspace remains a Workspace and remains visible through jjfx's
  existing Workspace model. Worker-specific capacity and reservation rules stay
  in the Workspace Dispatch layer.
- The shared Rust library will own the domain rules and compatibility-sensitive
  persistence, locking, process reconciliation, and runtime coordination. CLI
  parsing and TUI rendering remain outside that library.
- Migration remains incremental. Existing Go wsg state and processes remain
  compatibility peers until conformance is proven; adopting this domain does
  not authorize destructive state conversion or a behavior cutover.

The terms and distinctions are recorded in the repository glossary in
`CONTEXT.md`. The migration scope and compatibility promise are defined by
`.scratch/wsg-migration/PRD.md`.

## Consequences

jjfx can now add Worker Pool and Ticket orchestration without treating a Worker
as another name for an Agent or Workspace. The shared library has a clear seam
for persistence and execution rules, while jjfx keeps ownership of presentation
and message-driven TUI state.

The existing observation decisions remain useful. ADR 0003 continues to govern
the two orthogonal Agent and Work lifecycles. ADR 0005 continues to require a
self-contained native Rust implementation that coexists with the shell tools.
ADR 0007 continues to govern when jj-lib is appropriate; this decision does
not replace every jj CLI operation with jj-lib.

The next migration step is the shared Rust foundation and skeletal `wsg`
binary, followed by compatibility contracts before any mutating Worker Pool
integration.
