# Colocate workspaces with Git by default

Status: accepted

## Context

jj 0.44-0.46 changed `jj workspace add`: in a colocated repository it now
creates a Git worktree, so a new workspace carries a working `.git` (a worktree
pointer file). jjfx pinned the pre-0.46 non-colocated behavior while it upgraded
jj and jj-lib to 0.46.0, then had to decide what to do next.

A working `.git` inside each workspace is what lets Git-native tools operate
there - most importantly `gh stack`, which jjfx's forge pipeline otherwise had
to work around. A non-colocated workspace has no `.git`, so those tools cannot
run in it.

Two facts constrain the decision. Colocation is only meaningful when the
repository itself is colocated: a non-colocated repository has no Git directory
to share, and `jj workspace add --colocate` in one produces a workspace whose
Git state is not what a reader expects. And jjfx's own test repositories must
stay non-colocated: creating a Git worktree per test is the exact source of the
intermittent `git worktree add` failures the suite already hit.

## Decision

jjfx creates **colocated workspaces by default**, gated on the repository
actually being colocated.

- The top-level `colocate` config setting (default `true`) controls it. jjfx
  passes `--colocate` when the setting is on *and* the repository root has a
  `.git` directory, and `--no-colocate` otherwise. The gate is what keeps a
  non-colocated repository - including jjfx's own test repositories - from
  creating worktrees.
- The preference travels with the `wsg_core::Repository` (a `with_colocate`
  builder, read back as `colocate()`), so every creation path honors it: the TUI
  ad-hoc create, the Worker Pool provisioner, and the `wsg`/`jjfx` CLI `add` and
  `pool` commands.
- A repository that is not itself colocated is never colocated, whatever the
  setting says. `jjfx colocation enable` (a thin wrapper over `jj git colocation
  enable`) is the migration path; `disable` reverses it and `status` reports it.
- Teardown keeps `jj workspace forget`. jj removes the workspace's Git worktree
  registration as part of forgetting it, so no extra step is needed. jjfx does
  not use `jj workspace remove`: it snapshots the working copy into a commit
  first, which would commit a removed workspace's untracked files and is the
  wrong shape for a delete action.
- jjfx's existing "actually pushed" logic already accounts for colocation. The
  trunk resolver reads only the `origin` remote, and the chain and graph readers
  use `remote_bookmarks()` / `REMOTE_NAME_FOR_LOCAL_GIT_REPO` to exclude jj's
  colocated `git` pseudo-remote. No change was required there.

The `gh stack` workaround remains documented as a fallback for repositories that
choose not to colocate.

## Consequences

New workspaces in a colocated repository have a working `.git`, so Git-native
tooling runs inside them. Existing non-colocated repositories keep working
unchanged and opt in with `jjfx colocation enable`.

Colocation couples each workspace to the repository's Git directory, so a
workspace is no longer a self-contained directory tree. Removing a workspace
must go through jj (as it does) rather than deleting the directory alone, or the
worktree registration is left behind.

The test suite stays deterministic: jjfx's test repositories are created with
`jj git init --no-colocate`, and the colocation decision itself is covered by a
pure unit test over the flag rule rather than by creating worktrees in tests.
