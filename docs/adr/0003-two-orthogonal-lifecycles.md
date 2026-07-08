# Model agent state and work state as two orthogonal lifecycles

A workspace carries two independent lifecycles - agent activity
(Absent/Working/Waiting/NeedsAttention/Ended) and work progress
(Clean/Dirty/Pushed/PrOpen/Merged) - modeled as separate axes rather than
collapsed into a single enum. Collapsing loses real distinctions: an agent
Waiting over a change-requested PR needs a nudge, whereas an agent Waiting over
a Clean workspace is just an idle husk to delete. The list view derives one
"Attention" badge from the pair, but the axes stay independently queryable
underneath.

Status: accepted
