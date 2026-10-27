# A repository-declared pool profile is ignored, and the lane runs the default runtime

Status: needs-triage
Type: bug
Blocked by: none

## Problem

A Repository may declare a named Worker Pool and **reference** a profile granted
user-globally - the docs' own example is `[pools.bugs] size = 2 profile = "fast"`,
and ADR 0014 is explicit that a Repository selects among the profiles already
granted. In the drive-4 fixture that declaration is ignored: the lane runs the
*default* runtime instead.

This is not cosmetic. The fixture declares `profile = "drive"` (granted as
`[profiles.drive] runtime = "opencode"`), and the lane ran `claude` - a runtime
that is not even authenticated on this machine, so every Run died at once with an
agent authentication error that pointed at credentials rather than at the
configuration being ignored.

## Evidence

User config (granted profiles):

```toml
[profiles.opencode]
runtime = "opencode"

[profiles.drive]
runtime = "opencode"
```

Repository config:

```toml
[pools.ledger]
size = 4
label = "ledger"
profile = "drive"
```

Then:

```
$ jjfx status
Profile (default): opencode
Profile (ledger): claude
```

Reproduced twice, with two different profile names (`opencode` and `drive`), and
after a full `jjfx pool destroy --force ledger` followed by a re-materialisation
(`Created worker-...`), so the pool was materialised *with* the declaration
present.

`jjfx pool profile <runtime>` - the explicit verb - reaches only `default`:

```
$ jjfx pool profile opencode
Pool profile set to opencode
$ jjfx status | grep Profile
Profile (default): opencode
Profile (ledger): claude
```

So for a **named** pool there is no route to a non-default runtime at all: the
declaration is ignored and the verb does not address it.

## Second facet: a declared `default` size is not materialised either

```toml
[pools.default]
size = 4
label = "ledger"
```

```
$ jjfx dispatch --all
Pool has 0 idle worker(s) but 2 ticket(s) to dispatch. Grow pool by 2? Not a
terminal, so not growing the pool; run `jjfx pool <N>` to grow it.
```

The pool materialised with 0 workers and the declaration's `size` was not
applied. (The message is good - it names the fix - but the declaration was
silently inert.)

## Acceptance Criteria

- [ ] A named pool declared with `profile = "<name>"` runs that profile's
      runtime, proven by a test that declares a pool referencing a granted
      profile and asserts the resolved runtime.
- [ ] `jjfx status` (or `pool list --json`) reports the resolved profile for
      every pool, so a mismatch is visible rather than inferred from a Run's
      failure.
- [ ] A declared size is materialised for a named pool and for `default`, or the
      declaration is rejected loudly at load rather than ignored.
- [ ] If a Repository may in fact not select a runtime this way, the docs and
      ADR 0014 are corrected and the declaration is refused with a message that
      says so - silence is not an option.
