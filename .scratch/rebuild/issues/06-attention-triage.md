# Attention triage: derive the badge, group and sort the list

Status: ready-for-agent

## Parent

epics/B-triage-and-actions.md

## What to build

The v0.1 payoff (ADR 0008). Combine the two lifecycle axes into the derived
Attention badge (needs-you / working / ready-to-forge / idle, per the PRD
derivation) and reorganize the list around it: group by Attention with needs-you
at the top, sort within groups, collapse the idle group. The list reorders
itself by push as state changes - no polling flicker.

## Acceptance criteria

- [ ] Each workspace shows one Attention badge derived from its (agent, work) state per the PRD rules.
- [ ] The list is grouped needs-you -> working -> ready-to-forge -> idle, with the idle group collapsible.
- [ ] A live state change (agent Stop, PR review) re-sorts the affected workspace into the right group without a manual refresh.
- [ ] The needs-you case distinguishes a Waiting-over-changes-requested-PR workspace from an idle Clean one.

## Blocked by

- issues/04-agent-lifecycle.md
- issues/05-work-lifecycle.md
