# Epic D - Interfaces and cutover

Type: epic
Status: tracking

## Goal

Deliver the Rust implementation through a compatible wsg CLI and the jjfx TUI, prove coexistence and parity, release both binaries from jjfx, and deprecate the Go repository without deleting its history.

## Entry preconditions

Epics A through C must provide the shared Repository, Worker Pool, Run, Worker action, Direct Dispatch, and Dispatch Group capabilities.

## Execution order

1. **14-restore-wsg-workspace-and-pool-cli** restores script-facing Workspace, Worker Pool, status, and version commands.
2. **15-restore-wsg-dispatch-and-session-cli** restores Dispatch, Follow-up, review, logs, completion, and orchestration commands.
3. **16-integrate-dispatch-into-jjfx** tracks the interactive Worker Pool and Dispatch integration, delivered in child tickets 19 through 23.
   - **19-enable-writable-worker-pool-in-jjfx** establishes the command/event controller and writable Pool management.
   - **20-add-jjfx-ticket-dispatch** adds selected-Worker, bulk, and Ready Ticket Dispatch.
   - **21-show-dispatch-progress-and-logs** adds Dispatch Group progress and structured logs.
   - **22-add-jjfx-worker-session-actions** adds Send, Review, Session outcomes, and Worker actions.
   - **23-share-and-harden-jjfx-tui** shares startup and completes hardening and end-to-end coverage.
4. **17-prove-go-rust-conformance** runs automated mixed-implementation conformance before release.
5. **24-package-jjfx-and-wsg-release** packages both binaries and stages them without cutover.
6. **25-run-manual-acceptance-and-record-cutover-evidence** performs owner validation against released artifacts.
7. **18-deprecate-the-go-repository** publishes the final transition after owner validation.

CLI work and jjfx integration can proceed in parallel once their shared library dependencies are complete. Cutover cannot begin until both interfaces pass end-to-end verification.

## How to work it

- Keep the wsg CLI thin and free of duplicated lifecycle rules.
- Reuse the jjfx TUI instead of porting Bubble Tea.
- Preserve stdout for machine-readable output and stderr for human messages.
- Treat version and installation behavior as explicit compatibility surfaces.
- Do not remove or archive the Go repository automatically.
- Follow the landing gate in `CLAUDE.md` for every focused change.

## Definition of done

- The jjfx Cargo workspace releases working `jjfx` and `wsg` binaries.
- Existing wsg scripts and pools continue to work with the Rust binary.
- jjfx can manage the Worker Pool, Dispatch Tickets, inspect logs, and continue Agent Sessions.
- Mixed Go and Rust conformance, process cleanup, and restart recovery scenarios pass.
- The owner approves the final deprecation notice and installation cutover.
- `mise run check` is green.
