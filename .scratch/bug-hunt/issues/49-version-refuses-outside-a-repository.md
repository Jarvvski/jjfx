# `jjfx version` refuses to run outside a jj repository

Status: needs-triage
Type: bug
Blocked by: none

## Problem

`jjfx version` errors when the current directory is not inside a jj repository,
while `jjfx --version` reports the version anywhere:

```
$ cd /tmp && jjfx version
Error: not inside a jj repository: no .jj/ found above /tmp

$ cd /tmp && jjfx --version
jjfx 0.56.0
```

Found while verifying the `v0.56.0` release: unpacking the published archive into
a temporary directory and running the binary - the first thing anyone does with a
fresh download - fails this way.

## Why it matters

`docs/releasing.md` frames `jjfx version` as *the* diagnostic a bug report uses:

> `jjfx version` reports `<version>-dev` for a build from a checkout, because such
> a build is not the tagged release... a crate unpacked from crates.io carries no
> VCS metadata, so both report the clean version. **A bug report therefore says
> which build is in hand.**

A bug report very often comes from outside a repository - a home directory, a
temporary directory, a plain git checkout, or a freshly unpacked archive - which
is exactly where this command refuses to answer. The one command whose purpose is
to say which build is in hand is the one that cannot be run in the place a report
usually starts.

## Evidence

- Reproduced on the released `0.56.0` archive (`bin/jjfx` unpacked into `/tmp`)
  **and** on the installed `0.54.0`, so this is long-standing rather than a
  regression in the recent batch.
- `jjfx --version` succeeds in both, and reports the clean `jjfx 0.56.0` from the
  archive - so the archive itself is sound, including its published `.sha256`,
  which matches.
- Inside a repository both forms work.

## Acceptance Criteria

- [ ] `jjfx version` reports the version, and the build provenance
      `docs/releasing.md` describes, with no repository present.
- [ ] `jjfx --version` keeps working, and the two agree.
- [ ] A test covers the no-repository case.
- [ ] The fix is scoped to the version command: no other command becomes
      repo-independent by accident, and no command that needs a repository loses
      its clear refusal.
- [ ] If the intended answer is instead to document that `version` needs a
      repository, say so and change `docs/releasing.md` - but then the
      bug-report guidance needs a command that works outside one.
