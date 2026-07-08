# The UI is organized around Attention (triage-first), not a static dashboard

The primary surface is the workspace list, grouped and ordered by the derived
Attention signal - needs-you, then working, then ready-to-forge, then idle - so
the tool answers "which workspace needs me, and why?" before the user has to
scan. Each row reads both lifecycle axes at a glance; diff, graph, and PR detail
are progressive disclosure for the selected workspace rather than always-on
panels.

Chosen over porting the original dense four-column dashboard (which leaves the
triage work to the user) and over a minimal single-detail view (which loses the
at-a-glance overview). It is made practical by the event-sourced state model:
the list updates by push, so it can reorder on Attention changes without the
poll-driven `...`/flicker of the original.

Status: accepted
