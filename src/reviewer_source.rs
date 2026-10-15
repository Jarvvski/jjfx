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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_reviewer_configured_selects_nothing() {
        let config = ReviewConfig::default();

        let reviewer = configured_reviewer(&config).expect("no reviewer");

        assert!(reviewer.is_none());
    }

    #[test]
    fn coderabbit_is_constructed() {
        let config = ReviewConfig {
            reviewer: ReviewerKind::Coderabbit,
            max_passes: 1,
        };

        let reviewer = configured_reviewer(&config).expect("coderabbit");

        assert!(reviewer.is_some());
    }

    #[test]
    fn the_opencode_action_reviewer_is_a_setup_error() {
        let config = ReviewConfig {
            reviewer: ReviewerKind::OpenCodeAction,
            max_passes: 1,
        };

        let error = configured_reviewer(&config)
            .err()
            .expect("not available yet");

        assert!(error.to_string().contains("not available yet"), "{error}");
    }
}
