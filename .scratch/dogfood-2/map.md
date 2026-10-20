# The second drive: does jjfx make working a Ticket backlog better?

Status: open (filed 2026-10-20)

## Why this exists

The first drive (`../dogfood/`) ran jjfx against a static site with four Tickets
and a local bare origin. It found ten things, and `.scratch/harness/` was built
from them.

That drive asked *does jjfx work*. This one asks the question the owner actually
cares about: **does jjfx make working a Ticket backlog in a jj repo, through
opencode, a better experience than doing it by hand?** The harness batch was
built from observed pain in the owner's real loop - 1,481 sessions in five days,
of which jjfx's own Worker pool was 9 - so this drive is the *validation* of that
batch, not a repeat of the first. If the batch did not move the needle, the point
of this drive is to find out where the pain went instead.

## The goal

Nail the **agent usage**: the shape of an opencode session working a Ticket
backlog in a jj repo with jjfx, such that the experience is better than the
alternative. The drive should end with:

- a **usage playbook** for agents - what an agent should know and do, and what
  jjfx should be doing for it rather than asking it to remember;
- the **friction an agent actually hits**, filed as Tickets rather than left in
  a report;
- a judgement, with evidence, on whether the harness batch moved the needle.

## The fixture

A **fresh fake repository building a tool** - not a static site - worked **from
a Ticket backlog**.

Why a tool rather than a site: it has a real build and a real test suite, so the
verified-done check has something to verify, and a Ticket can fail its check in
a way that is visible rather than aesthetic. A site cannot fail a check.

## The binary: a local build, not the one on `PATH`

The drive must exercise the **unreleased** changes, so it runs a `jjfx` built
from this repository's `main` - never a released binary.

This is not hypothetical. Measured 2026-10-20:

- `which -a jjfx` finds `/Users/jarvis/.cargo/bin/jjfx` **first**, reporting
  `jjfx 0.54.0`; the mise shim beside it is the same version.
- The build already sitting in `target/` (19 Oct 18:51) reports `0.55.0-dev` but
  **predates `resume` (#124) and `ticket list` (#141)** - `jjfx help resume` and
  `jjfx help ticket list` both fail against it. A stale build is not always a
  released one.

So, before the drive:

- **build from the current `main`** (`mise run build`), *after* the last ticket
  lands so the build contains everything;
- run the drive with the built binary **first on `PATH`** (`target/release`), or
  by absolute path;
- **verify it** - a claim you did not run is the thing `dogfood/07` was about:
  `jjfx version` must report `0.55.0-dev` (a checkout build carries the `-dev`
  suffix), and every verb the drive uses must answer (`jjfx help resume`,
  `jjfx help ticket`, `jjfx children`, `jjfx absorb`).

`dogfood/07` documented exactly this trap for an agent: a stale binary first on
`PATH` makes the skill's central promise - that `jjfx help <command>` reflects
the parser - false, silently. The drive should treat *"which jjfx am I running?"*
as a precondition, and the playbook should say so.

## Method

- Drive it **the way an agent would**, through jjfx: dispatch, pool, the queue,
  the review bound - not by hand-running commands and watching.
- **Test our own claims rather than confirming them.** This is the first drive's
  discipline and the reason it was worth running. If the harness batch is
  supposed to have fixed something, try to falsify that rather than demonstrate
  it.
- Record **every** friction as a Ticket, and keep the report separate from the
  backlog, as the first drive did.
- Leave the fixture's evidence behind deliberately.

## Decisions (2026-10-20)

- **Language: Python.** A tool in a language unlike Rust, so the drive proves
  jjfx is toolchain-agnostic rather than accidentally Rust-shaped.
- **Forge: a real throwaway GitHub repository.** The one blessed forge path is
  validated end to end - push, open a PR, read the check, merge on green - which
  a local bare origin cannot exercise at all.
- **Topology: jjfx dispatch, hosting the real shape.** Driven through jjfx
  (pool, queue, governor, review bound) with the review step included, so jjfx
  hosts the owner's real `build -> general -> reviewer` shape. Whether it can is
  the crux of "agent usage".

## A question the drive must answer

`harness/09` and `harness/12` built step identity, a decision log and a review
loop that replays a recorded pass without reaching the model - proven end to
end at the harness seam, by driving the loop twice through a reviewer that
panics if asked.

**No command invokes that loop**, and that is not an oversight in either ticket:
`jjfx review` *sends a prompt* and produces no findings of its own, so jjfx
holds no decision to replay until something calls the loop. The drive should
settle whether that is the right boundary - jjfx is not a reviewer - or whether
the reduction the parent ticket was filed for (seven review dimensions recurring
8-11x) needs jjfx to own the loop after all. **Measure it, do not assume it**:
the honest question is whether an agent working a backlog through jjfx ever
repeats a dimension, and whether anything notices if it does.

## References

- `.scratch/dogfood/` - the first drive, its report and its ten findings
- `.scratch/harness/map.md` - what the first drive produced
- `.scratch/harness/issues/12-wire-the-decision-log.md` - the last of it
