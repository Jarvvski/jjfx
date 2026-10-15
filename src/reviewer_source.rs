//! Selecting the configured AI reviewer for the review verb.
//!
//! The reviewer is a `[review]` config decision, not a property of the Agent
//! Runtime: a repository that configures one gets findings inlined into its
//! review prompts whatever runtime authors the fix. The selected reviewer is a
//! plain [`Reviewer`], so the bounded review loop stays generic over it.

use crate::config::{ReviewConfig, ReviewerKind};
use crate::engine::{CodeRabbitReviewer, Reviewer};

/// Builds the reviewer `[review] reviewer` selects.
///
/// `None` means no reviewer is configured, which leaves `review` exactly as it
/// was. A configured reviewer this build cannot construct is a setup error,
/// surfaced before any Worker Run starts.
pub(crate) fn configured_reviewer(
    config: &ReviewConfig,
) -> anyhow::Result<Option<Box<dyn Reviewer>>> {
    match config.reviewer {
        ReviewerKind::None => Ok(None),
        ReviewerKind::Coderabbit => Ok(Some(Box::new(CodeRabbitReviewer::new()))),
        ReviewerKind::OpenCodeAction => anyhow::bail!(
            "the opencode-action reviewer is not available yet; \
             set [review] reviewer = \"coderabbit\" or \"none\""
        ),
    }
}
