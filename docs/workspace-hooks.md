# Workspace hooks

Workspaces are provisioned and cleaned up by two repository-local scripts:

```
.jjfx/setup.sh
.jjfx/teardown.sh
```

They run for every Workspace lifecycle path - CLI, TUI, and Worker Workspaces -
so a repository can carry its own provisioning without jjfx hard-coding it. A
hook is optional; when the file is absent the step is a no-op.

Because each workspace is a checkout of the repository, the hook files must be
tracked in jj (for example `jj file track root:.jjfx/setup.sh`) so they are
present inside every workspace.

## `setup.sh`

Runs when a workspace is created, before the tab opens or the Worker is
considered ready.

- Invoked as `/bin/sh .jjfx/setup.sh` with the new workspace directory as the
  working directory and stdin closed.
- Its stdout and stderr are captured and streamed: the TUI shows progress in the
  footer, and the CLI routes hook output to stderr.
- Setup is **transactional**. A non-zero exit rolls back the creation and
  rejects the workspace - a failed setup never leaves a half-provisioned
  workspace behind. The error names `setup.sh` and includes the captured stderr.

Typical use: trusting `mise`, installing dependencies, or generating per-clone
files. Defer heavyweight work until it is actually needed - the default
provisioning trusts the project's `mise.toml` rather than eagerly installing
everything.

## `teardown.sh`

Runs when a workspace is removed.

- Invoked as `/bin/sh .jjfx/teardown.sh` with the workspace directory as the
  working directory and stdin closed.
- Teardown is **best-effort**: a failure is reported but does not block removal.
  The workspace is still deleted.

Typical use: stopping services, freeing ports, or removing generated artifacts.

## Notes

- Hooks are not sandboxed; they run with your user's permissions.
- Earlier releases configured provisioning with a `[workspace]` section in jjfx's
  config. That section is now accepted and ignored - move its work into
  `.jjfx/setup.sh`. See [Configuration](configuration.md).
