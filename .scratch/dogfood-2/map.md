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

## Open decisions

- **Language**: a non-Rust tool would prove jjfx is toolchain-agnostic; Rust
  reuses a toolchain already known to work end to end.
- **Forge**: a real throwaway GitHub repository validates the one blessed forge
  path end to end (push, open a PR, merge on green); a local bare origin is
  cheaper and offline but cannot exercise merge-on-green at all.
- **Topology**: jjfx's own dispatch (pool, queue, governor) is the thing to
  drive; whether the drive should also reproduce the owner's real
  `build -> general -> reviewer` shape, and see whether jjfx can host it, is the
  crux of "agent usage" and is not yet settled.

## References

- `.scratch/dogfood/` - the first drive, its report and its ten findings
- `.scratch/harness/map.md` - what the first drive produced
- `.scratch/harness/issues/12-wire-the-decision-log.md` - the last of it
