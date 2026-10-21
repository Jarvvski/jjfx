//! Driving a bounded review loop through the decision log.
//!
//! [`ReviewLoop`] is the bound and `ReplayingReviewer` is the replay seam; this
//! module is the caller that joins them. Each pass is keyed by the step identity
//! `review:<ticket>:<dimension>:<pass>`, and the pass's disposition is written to
//! the durable queue so an operator can tell a replay from a pass that ran
//! (ticket 12).
//!
//! Replay has two scopes, and the difference matters. [`RecordedReviewLoop::new`]
//! reads the pool's durable decision log, so a pass replays across invocations -
//! the harness seam, whose reviewer is a model and whose decisions do not depend
//! on state the outside world can move. [`RecordedReviewLoop::per_invocation`]
//! replays only within the run that owns it: `jjfx review` decides each pass on
//! the Pull Request's *current* review threads, so a decision must not outlive
//! the state it saw.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::engine::pool::current_timestamp;
use crate::engine::queue::WorkQueue;
use crate::engine::reviewer::{
    DecisionLog, DecisionLogError, ReviewLoop, ReviewLoopError, ReviewLoopSummary, ReviewOutcome,
    ReviewRequest, Reviewer,
};
use crate::engine::state::{QueueSettlement, QueuedStep, StepKey};

/// The detail a replayed pass records in the queue.
pub const REPLAYED_DETAIL: &str = "replayed from the recorded decision";

/// The Ticket and review dimension one review loop is reviewing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSubject {
    ticket: String,
    dimension: String,
}

impl ReviewSubject {
    /// Creates the subject of a review loop.
    pub fn new(ticket: impl Into<String>, dimension: impl Into<String>) -> Self {
        Self {
            ticket: ticket.into(),
            dimension: dimension.into(),
        }
    }

    /// Returns the Ticket the loop reviews.
    pub fn ticket(&self) -> &str {
        &self.ticket
    }

    /// Returns the dimension the loop reviews.
    pub fn dimension(&self) -> &str {
        &self.dimension
    }

    /// Returns the identity of this subject's `pass`.
    pub fn step(&self, pass: u32) -> StepKey {
        StepKey::review(&self.ticket, &self.dimension, pass)
    }
}

/// A decision log scoped to one invocation.
///
/// It replays a decision recorded during the run that owns it and forgets every
/// decision when that run ends. `jjfx review` drives its loop over one of these
/// rather than the pool's durable sidecar, because a review pass decides on the
/// Pull Request's *current* review threads: a decision is only good for the state
/// it saw, and one replayed by a later invocation would serve a verdict the
/// threads have since contradicted - a recorded "no findings" that hides a thread
/// opened afterwards, which the loop would report as convergence.
///
/// The interior mutability is genuine shared state, not a borrow-checker dodge:
/// [`DecisionLog::record`] takes `&self` so a reader and a writer can share the
/// log, exactly as the durable sidecar does across processes.
#[derive(Debug, Default)]
pub struct EphemeralDecisionLog {
    decisions: Mutex<BTreeMap<StepKey, ReviewOutcome>>,
}

impl DecisionLog for EphemeralDecisionLog {
    fn decision(&self, step: &StepKey) -> Result<Option<ReviewOutcome>, DecisionLogError> {
        let decisions = self
            .decisions
            .lock()
            .map_err(|_| DecisionLogError::new("the invocation's decision log is poisoned"))?;
        Ok(decisions.get(step).cloned())
    }

    fn record(&self, step: &StepKey, decision: &ReviewOutcome) -> Result<(), DecisionLogError> {
        let mut decisions = self
            .decisions
            .lock()
            .map_err(|_| DecisionLogError::new("the invocation's decision log is poisoned"))?;
        decisions.insert(step.clone(), decision.clone());
        Ok(())
    }
}

/// Drives a bounded review loop through the decision log and the durable queue.
///
/// This is the seam ticket 12 wires: it turns the replay mechanism ticket 09
/// built into the caller of a review pass. The bound stays [`ReviewLoop`]'s -
/// replay adds and removes no round - and what this adds is the step identity
/// each pass is keyed by and the durable record of whether it ran or replayed.
#[derive(Debug, Clone)]
pub struct RecordedReviewLoop<S> {
    decisions: S,
    queue: WorkQueue,
    subject: ReviewSubject,
}

impl RecordedReviewLoop<EphemeralDecisionLog> {
    /// Creates a driver whose replay is scoped to this invocation.
    ///
    /// A pass already decided in this run is replayed; a pass decided by an
    /// earlier invocation is not. This is the driver `jjfx review` uses, so each
    /// review reads the Pull Request's review threads as they are *now* rather
    /// than trusting a verdict recorded when they may have been different.
    pub fn per_invocation(queue: WorkQueue, subject: ReviewSubject) -> Self {
        Self::new(EphemeralDecisionLog::default(), queue, subject)
    }
}

impl<S: DecisionLog> RecordedReviewLoop<S> {
    /// Creates a loop driver over `decisions`, recording to `queue`.
    pub fn new(decisions: S, queue: WorkQueue, subject: ReviewSubject) -> Self {
        Self {
            decisions,
            queue,
            subject,
        }
    }

    /// Runs `bound` over `reviewer`, replaying a pass the log already recorded.
    ///
    /// A repeated `(ticket, dimension, pass)` is served from `decisions` rather
    /// than reaching `reviewer`, and every pass's disposition is recorded in the
    /// queue.
    pub fn run<R, A, E>(
        &self,
        bound: ReviewLoop,
        reviewer: &R,
        request: &ReviewRequest,
        address: A,
    ) -> Result<ReviewLoopSummary, ReviewLoopError<E>>
    where
        R: Reviewer + ?Sized,
        A: FnMut(usize, &ReviewOutcome) -> Result<(), E>,
    {
        let summary = bound.run_recorded(
            reviewer,
            &self.decisions,
            self.subject.ticket(),
            self.subject.dimension(),
            request,
            address,
        )?;
        self.record_passes(&summary);
        Ok(summary)
    }

    /// Records each pass's disposition in the durable queue.
    ///
    /// Best effort, exactly as every other write at this seam: a review that has
    /// already run must not fail because its record could not be written. Every
    /// write is keyed by step identity and idempotent, so a later pass settles
    /// the same step.
    fn record_passes(&self, summary: &ReviewLoopSummary) {
        for pass in 1..=summary.passes() as u32 {
            let step = self.subject.step(pass);
            if summary.replayed_passes().contains(&pass) {
                let _ = self.queue.record_replay(&step, REPLAYED_DETAIL);
                continue;
            }
            let Ok(at) = current_timestamp() else {
                continue;
            };
            let offered = QueuedStep::review(
                self.subject.ticket(),
                self.subject.dimension(),
                pass,
                at.clone(),
                None,
            );
            let _ = self.queue.enqueue(std::slice::from_ref(&offered));
            let _ = self.queue.settle(&step, QueueSettlement::Done, at);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::engine::Repository;
    use crate::engine::reviewer::{
        ReviewCompleteness, ReviewFinding, ReviewSeverity, ReviewerError,
    };
    use crate::engine::state::QueueItemState;

    fn request() -> ReviewRequest {
        ReviewRequest::new("owner/repo", "main", "adam/feature")
    }

    fn finding(title: &str) -> ReviewFinding {
        ReviewFinding::new(
            ReviewSeverity::Major,
            Some("src/x.rs".to_owned()),
            Some((12, 14)),
            Some(title.to_owned()),
            "Cap the loop.",
            vec!["n = 3".to_owned()],
        )
    }

    fn repository() -> (tempfile::TempDir, Repository) {
        let temp = tempfile::tempdir().expect("temporary repository");
        fs::create_dir(temp.path().join(".jj")).expect("repository marker");
        let repository = Repository::open(temp.path()).expect("repository");
        (temp, repository)
    }

    /// A reviewer that finds one thing on its first pass and nothing after, so the
    /// loop converges and every pass it ran is recorded.
    #[derive(Default)]
    struct ConvergingReviewer {
        calls: AtomicUsize,
    }

    impl Reviewer for ConvergingReviewer {
        fn review(&self, _: &ReviewRequest) -> Result<ReviewOutcome, ReviewerError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let findings = if call == 0 {
                vec![finding("bound the loop")]
            } else {
                Vec::new()
            };
            Ok(ReviewOutcome::new(findings, ReviewCompleteness::Complete))
        }
    }

    /// A reviewer that fails loudly if it is ever asked, so a test can prove a
    /// replayed pass never reached the model.
    struct PanickingReviewer;

    impl Reviewer for PanickingReviewer {
        fn review(&self, _: &ReviewRequest) -> Result<ReviewOutcome, ReviewerError> {
            panic!("a replayed pass must not re-invoke the reviewer");
        }
    }

    /// Ticket 12: a pass that ran is recorded in the durable queue, and a pass
    /// that replayed is recorded as a replay - which is how an operator tells the
    /// two apart.
    #[test]
    fn a_pass_that_ran_and_a_pass_that_replayed_are_recorded_differently() {
        let (temp, repository) = repository();
        let subject = ReviewSubject::new("ENG-1", "security");
        let first = RecordedReviewLoop::new(
            repository.state_store().decisions(),
            repository.work_queue(),
            subject.clone(),
        );

        let summary = first
            .run(
                ReviewLoop::new(3),
                &ConvergingReviewer::default(),
                &request(),
                |_, _| Ok::<(), String>(()),
            )
            .expect("the first drive decides");
        assert_eq!(summary.passes(), 2);
        assert_eq!(summary.stop(), crate::engine::ReviewStop::Converged);
        assert_eq!(summary.replayed(), 0);

        // A fresh handle over the same Repository: the records are read from disk.
        let reopened = Repository::open(temp.path()).expect("reopen repository");
        for pass in 1..=2 {
            let item = reopened
                .work_queue()
                .item(&subject.step(pass))
                .expect("item")
                .expect("recorded");
            assert_eq!(item.state, QueueItemState::Done);
            assert_eq!(
                item.detail, None,
                "a pass that ran carries no replay detail"
            );
        }

        // The second drive reaches neither the reviewer nor the model.
        let second = RecordedReviewLoop::new(
            reopened.state_store().decisions(),
            reopened.work_queue(),
            subject.clone(),
        );
        let summary = second
            .run(
                ReviewLoop::new(3),
                &PanickingReviewer,
                &request(),
                |_, _| Ok::<(), String>(()),
            )
            .expect("the second drive replays");

        assert_eq!(summary.passes(), 2);
        assert_eq!(summary.replayed(), 2);
        assert_eq!(summary.replayed_passes(), [1, 2]);

        // The operator sees the replay in `status --json`'s queue, through the
        // item's detail.
        for pass in 1..=2 {
            let item = reopened
                .work_queue()
                .item(&subject.step(pass))
                .expect("item")
                .expect("recorded");
            assert_eq!(
                item.state,
                QueueItemState::Done,
                "a replay does not move the step"
            );
            assert_eq!(item.detail.as_deref(), Some(REPLAYED_DETAIL));
        }
    }

    /// A reviewer that finds nothing and counts how often it is asked.
    #[derive(Default)]
    struct CountingReviewer {
        calls: AtomicUsize,
    }

    impl Reviewer for CountingReviewer {
        fn review(&self, _: &ReviewRequest) -> Result<ReviewOutcome, ReviewerError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ReviewOutcome::new(Vec::new(), ReviewCompleteness::Complete))
        }
    }

    /// The rule `jjfx review` must hold: a recorded decision must not outlive the
    /// state it decided on. Two `per_invocation` drivers over the same Repository
    /// each reach the reviewer, so a pass is never served from an earlier run's
    /// record - which is what lets a later run see a review thread opened after
    /// the first.
    #[test]
    fn an_invocation_scoped_driver_reaches_the_reviewer_on_every_run() {
        let (_temp, repository) = repository();
        let subject = ReviewSubject::new("ENG-1", "threads");
        let reviewer = CountingReviewer::default();

        for run in 1..=2 {
            let summary =
                RecordedReviewLoop::per_invocation(repository.work_queue(), subject.clone())
                    .run(ReviewLoop::new(3), &reviewer, &request(), |_, _| {
                        Ok::<(), String>(())
                    })
                    .expect("run");
            assert_eq!(summary.stop(), crate::engine::ReviewStop::Converged);
            assert_eq!(
                summary.replayed(),
                0,
                "run {run} must not replay a decision from an earlier invocation"
            );
        }

        assert_eq!(
            reviewer.calls.load(Ordering::SeqCst),
            2,
            "each invocation must read the reviewer again"
        );
    }
}
