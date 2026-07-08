# Workspace actions: new / open / delete via kitty

Status: done

## Parent

epics/B-triage-and-actions.md

## What to build

The interactive mutations, behind a `Terminal` trait with a `KittyTerminal` impl
(v1 kitty-only). `n` creates a workspace (updating the store + cache mirror) and
spawns a kitty tab with the claude + shell split layout; `enter`/`o` opens or
focuses the workspace's tab (foreground / background); `d` deletes a workspace
(confirmation, removes the workspace + closes its tab). All kitty calls go
through the trait so the multiplexer is swappable later.

## Acceptance criteria

- [x] `n <name>` creates a workspace, writes it through to the store + `.jj/ws-cache`, and opens a kitty tab running claude alongside a shell. (Verified: `n` opens a name-entry mode; on enter `create_workspace` runs `jj workspace add --name <name> <repo>-<name>`, persists the path to the ws-cache, reloads, and calls `terminal.open(.., focus=true)` which launches a `--type=tab` running claude + a `--location=vsplit` shell. jj-add + cache reconcile verified in an isolated repo; the kitty tab+vsplit verified live via `kitten @`.)
- [x] `enter` focuses an existing tab or opens one; `o` opens in the background without stealing focus; re-opening an existing workspace focuses rather than duplicating. (Verified: `open_selected` checks `terminal.is_open` first - enter focuses an existing tab, o leaves an existing one untouched, and a missing tab is opened with focus=enter; `enter_focuses_existing_tab_and_o_opens_background` test. Background focus-restore verified live: focus returned to the prior window.)
- [x] `d` asks for confirmation, removes the workspace, and closes its kitty tab; the default workspace cannot be deleted. (Verified: `d` enters `ConfirmDelete`; `y` closes the tab, `jj workspace forget`s it, removes the dir - guarded to never touch the repo root - and drops it from the cache; the default is refused before confirmation - `d_on_default_is_refused_without_confirmation` test; `close-tab` with an anchored title match verified live.)
- [x] All terminal operations route through the `Terminal` trait; no `kitten @` calls leak into unrelated modules. (Verified: `grep kitten|kitty src/` outside `terminal.rs` finds only doc-comment mentions; `App` holds a `Box<dyn Terminal>` and calls only trait methods.)

## Blocked by

- issues/03-skeleton-store.md
