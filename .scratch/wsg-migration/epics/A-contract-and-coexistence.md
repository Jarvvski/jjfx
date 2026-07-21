# Epic A - Contract and coexistence

Type: epic
Status: tracking

## Goal

Establish the architectural decision, shared Rust foundation, exact compatibility contract, and first read-only integration. At the end of this epic, jjfx can show a Go-created Worker Pool without taking ownership of mutation or execution.

## Entry preconditions

None. This epic begins the migration.

## Execution order

1. **01-adopt-dispatch-domain** records the language and supersedes the temporary limit in ADR 0001.
2. **02-create-shared-rust-foundation** establishes the Cargo workspace, shared library, and skeletal wsg binary.
3. **03-lock-down-compatibility-contracts** captures the language-neutral wire and command contracts. It can begin after the shared test foundation exists.
4. **04-import-worker-pool-snapshots** consumes those contracts and surfaces read-only state in jjfx.

## How to work it

- Treat the Go implementation as an observable compatibility peer, not as code to translate.
- Keep existing jjfx behavior unchanged until the read-only pool integration lands.
- Use the vocabulary in `CONTEXT.md` and explicitly distinguish Worker, Agent, Session, Run, and Workspace.
- Keep CLI parsing and TUI rendering outside the shared library.
- Follow the landing gate in `CLAUDE.md` for every focused change.

## Definition of done

- The domain and ADR changes are accepted in the repository.
- The Cargo workspace builds the existing jjfx binary, a shared library, and a skeletal wsg binary.
- Golden compatibility fixtures cover every persisted wsg state format.
- jjfx displays existing Worker Pool and Run state without mutating it.
- `mise run check` is green.
