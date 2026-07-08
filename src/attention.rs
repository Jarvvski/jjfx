//! The derived Attention signal (ADR 0008): collapse the two orthogonal
//! lifecycle axes into the single "what, if anything, do I need to do here?"
//! badge the list is organized around. The derivation is deliberately tunable;
//! it follows the PRD rules verbatim.

use crate::agent::AgentState;
use crate::work::WorkState;

/// The single human-facing signal per workspace, in priority order (the order
/// the list groups by): needs-you first, idle last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Attention {
    /// The agent is blocked, or waiting over a change-requested PR - act now.
    NeedsYou,
    /// A turn is in progress.
    Working,
    /// Dirty or pushed work sitting idle - ready to be forged toward merge.
    ReadyToForge,
    /// Clean work and no live agent - nothing to do.
    Idle,
}

impl Attention {
    /// Group heading for the list.
    pub fn heading(self) -> &'static str {
        match self {
            Attention::NeedsYou => "needs you",
            Attention::Working => "working",
            Attention::ReadyToForge => "ready to forge",
            Attention::Idle => "idle",
        }
    }
}

/// Derive the Attention badge from the two axes, per the PRD:
///
/// - **needs-you**: agent NeedsAttention, or agent Waiting over a
///   changes-requested PR.
/// - **working**: agent Working.
/// - **ready-to-forge**: Dirty/Pushed work sitting idle (agent not Working).
/// - **idle**: Clean work and no live agent (the fallback).
pub fn derive(agent: AgentState, work: WorkState) -> Attention {
    // needs-you: a blocked agent, or a Waiting agent over a change-requested PR.
    // The second clause is what distinguishes a genuine needs-you from an idle
    // Clean workspace whose agent merely finished its turn.
    if agent == AgentState::NeedsAttention {
        return Attention::NeedsYou;
    }
    if agent == AgentState::Waiting
        && matches!(work, WorkState::PrOpen { verdict, .. } if verdict.is_changes_requested())
    {
        return Attention::NeedsYou;
    }

    if agent == AgentState::Working {
        return Attention::Working;
    }

    // ready-to-forge: dirty or pushed work, with no turn in progress (guaranteed
    // here, since Working returned above).
    if matches!(work, WorkState::Dirty { .. } | WorkState::Pushed) {
        return Attention::ReadyToForge;
    }

    Attention::Idle
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work::ReviewVerdict;

    #[test]
    fn needs_attention_agent_is_always_needs_you() {
        assert_eq!(
            derive(AgentState::NeedsAttention, WorkState::Clean),
            Attention::NeedsYou
        );
        // Even over pushed work, a blocked agent is needs-you.
        assert_eq!(
            derive(AgentState::NeedsAttention, WorkState::Pushed),
            Attention::NeedsYou
        );
    }

    #[test]
    fn waiting_over_changes_requested_pr_is_needs_you_not_idle() {
        let changes = WorkState::PrOpen {
            number: 1,
            verdict: ReviewVerdict::ChangesRequested,
        };
        assert_eq!(derive(AgentState::Waiting, changes), Attention::NeedsYou);

        // The distinguishing case: a Waiting agent over Clean work is *not*
        // needs-you - it is just an idle husk.
        assert_eq!(
            derive(AgentState::Waiting, WorkState::Clean),
            Attention::Idle
        );

        // A Waiting agent over an approved PR is also not needs-you.
        let approved = WorkState::PrOpen {
            number: 1,
            verdict: ReviewVerdict::Approved,
        };
        assert_ne!(derive(AgentState::Waiting, approved), Attention::NeedsYou);
    }

    #[test]
    fn working_agent_is_working() {
        assert_eq!(
            derive(
                AgentState::Working,
                WorkState::Dirty {
                    added: 1,
                    removed: 0
                }
            ),
            Attention::Working
        );
    }

    #[test]
    fn dirty_or_pushed_idle_is_ready_to_forge() {
        assert_eq!(
            derive(
                AgentState::Absent,
                WorkState::Dirty {
                    added: 3,
                    removed: 1
                }
            ),
            Attention::ReadyToForge
        );
        assert_eq!(
            derive(AgentState::Ended, WorkState::Pushed),
            Attention::ReadyToForge
        );
        // But a Working agent over dirty work is Working, not ready-to-forge.
        assert_eq!(
            derive(AgentState::Working, WorkState::Pushed),
            Attention::Working
        );
    }

    #[test]
    fn clean_idle_and_unknown_fall_to_idle() {
        assert_eq!(
            derive(AgentState::Absent, WorkState::Clean),
            Attention::Idle
        );
        assert_eq!(
            derive(AgentState::Absent, WorkState::Unknown),
            Attention::Idle
        );
    }

    #[test]
    fn ordering_puts_needs_you_first_idle_last() {
        let mut v = [
            Attention::Idle,
            Attention::Working,
            Attention::NeedsYou,
            Attention::ReadyToForge,
        ];
        v.sort();
        assert_eq!(
            v,
            [
                Attention::NeedsYou,
                Attention::Working,
                Attention::ReadyToForge,
                Attention::Idle,
            ]
        );
    }
}
