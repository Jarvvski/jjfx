# Attention triage: derive the badge, group and sort the list

Status: done

## Parent

epics/B-triage-and-actions.md

## What to build

The v0.1 payoff (ADR 0008). Combine the two lifecycle axes into the derived
Attention badge (needs-you / working / ready-to-forge / idle, per the PRD
derivation) and reorganize the list around it: group by Attention with needs-you
at the top, sort within groups, collapse the idle group. The list reorders
itself by push as state changes - no polling flicker.

## Acceptance criteria

- [x] Each workspace shows one Attention badge derived from its (agent, work) state per the PRD rules. (Verified: `attention::derive` implements the PRD rules with a full unit-test matrix; each row renders a coloured badge column alongside its group.)
- [x] The list is grouped needs-you -> working -> ready-to-forge -> idle, with the idle group collapsible. (Verified: `classified()` sorts by `Attention` then name - `list_groups_by_attention_needs_you_first`; `c` folds the idle group - `idle_group_folds_and_selection_stays_valid`; live PTY run showed the four headings.)
- [x] A live state change (agent Stop, PR review) re-sorts the affected workspace into the right group without a manual refresh. (Verified via PTY: the default workspace moved ready-to-forge -> working -> needs-you -> ready-to-forge as `UserPromptSubmit`/`PermissionRequest`/`Stop` events arrived, each re-grouping with no keypress.)
- [x] The needs-you case distinguishes a Waiting-over-changes-requested-PR workspace from an idle Clean one. (Verified: `waiting_over_changes_requested_pr_is_needs_you_not_idle` - Waiting+ChangesRequested -> NeedsYou, Waiting+Clean -> Idle, Waiting+Approved -> not NeedsYou.)

## Blocked by

- issues/04-agent-lifecycle.md
- issues/05-work-lifecycle.md
