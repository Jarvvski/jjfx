# Workspace actions: new / open / delete via kitty

Status: ready-for-agent

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

- [ ] `n <name>` creates a workspace, writes it through to the store + `.jj/ws-cache`, and opens a kitty tab running claude alongside a shell.
- [ ] `enter` focuses an existing tab or opens one; `o` opens in the background without stealing focus; re-opening an existing workspace focuses rather than duplicating.
- [ ] `d` asks for confirmation, removes the workspace, and closes its kitty tab; the default workspace cannot be deleted.
- [ ] All terminal operations route through the `Terminal` trait; no `kitten @` calls leak into unrelated modules.

## Blocked by

- issues/03-skeleton-store.md
