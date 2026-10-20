//! The tool-enforced bound on a re-review loop: never more than the pass cap,
//! never another pass for a round that made no progress, and an escalation
//! rather than a silent success when the bound stops it.
//!
//! This is the bound ADR 0016 keeps in jjfx rather than in a prompt. It is
//! generic over the [`Reviewer`] that supplies each pass's findings; jjfx no
//! longer ingests an external review bot's findings into the review verb, so the
//! bound stands on its own until the harness drives it.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::engine::state::StepKey;

/// One pull request for a reviewer to review, addressed without a local checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRequest {
    repository: String,
    base: String,
    source_branch: String,
}

impl ReviewRequest {
    /// Creates a request for `owner/repo`'s `source_branch` against `base`.
    pub fn new(
        repository: impl Into<String>,
        base: impl Into<String>,
        source_branch: impl Into<String>,
    ) -> Self {
        Self {
            repository: repository.into(),
            base: base.into(),
            source_branch: source_branch.into(),
        }
    }

    /// Returns the `owner/repo` the review targets.
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// Returns the base ref the review compares against.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// Returns the head ref the review reviews.
    pub fn source_branch(&self) -> &str {
        &self.source_branch
    }
}

/// How severe a reviewer judged one finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSeverity {
    /// A correctness or security defect.
    Critical,
    /// A significant defect.
    Major,
    /// A smaller defect.
    Minor,
    /// A nit.
    Trivial,
    /// Informational.
    Info,
    /// No severity, or one this build does not know.
    None,
}

impl ReviewSeverity {
    /// Parses the reviewer's severity word, defaulting to [`Self::None`] for an
    /// unrecognised value so a new severity never fails a review.
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "critical" => Self::Critical,
            "major" => Self::Major,
            "minor" => Self::Minor,
            "trivial" => Self::Trivial,
            "info" => Self::Info,
            _ => Self::None,
        }
    }

    /// Returns the canonical lowercase severity word.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Major => "major",
            Self::Minor => "minor",
            Self::Trivial => "trivial",
            Self::Info => "info",
            Self::None => "none",
        }
    }
}

/// One normalised finding from a reviewer, independent of its vendor's schema.
///
/// Equality is by content: the review loop compares passes by it to tell a
/// finding it has already seen from one it has not.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReviewFinding {
    severity: ReviewSeverity,
    file: Option<String>,
    lines: Option<(u64, u64)>,
    title: Option<String>,
    body: String,
    suggestions: Vec<String>,
}

impl ReviewFinding {
    /// Creates a finding.
    pub fn new(
        severity: ReviewSeverity,
        file: Option<String>,
        lines: Option<(u64, u64)>,
        title: Option<String>,
        body: impl Into<String>,
        suggestions: Vec<String>,
    ) -> Self {
        Self {
            severity,
            file,
            lines,
            title,
            body: body.into(),
            suggestions,
        }
    }

    /// Returns the finding's severity.
    pub const fn severity(&self) -> ReviewSeverity {
        self.severity
    }

    /// Returns the file the finding is on, when the reviewer named one.
    pub fn file(&self) -> Option<&str> {
        self.file.as_deref()
    }

    /// Returns the line range the finding covers, when the reviewer gave one.
    pub const fn lines(&self) -> Option<(u64, u64)> {
        self.lines
    }

    /// Returns the finding's title, when the reviewer gave one.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Returns the fix instructions the Worker should act on.
    pub fn body(&self) -> &str {
        &self.body
    }

    /// Returns the reviewer's suggested fixes.
    pub fn suggestions(&self) -> &[String] {
        &self.suggestions
    }

    /// Returns `path:line-line` (or just the path), when the reviewer located the
    /// finding.
    pub fn location(&self) -> Option<String> {
        let file = self.file.as_deref()?;
        Some(match self.lines {
            Some((start, end)) if start == end => format!("{file}:{start}"),
            Some((start, end)) => format!("{file}:{start}-{end}"),
            None => file.to_owned(),
        })
    }

    /// Returns a one-line `[severity] location - title` summary.
    pub fn summary(&self) -> String {
        let mut summary = format!("[{}]", self.severity.as_str());
        if let Some(location) = self.location() {
            summary.push(' ');
            summary.push_str(&location);
        }
        if let Some(title) = self.title.as_deref() {
            summary.push_str(" - ");
            summary.push_str(title);
        }
        summary
    }
}

/// Whether a reviewer pass covered everything it was asked to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewCompleteness {
    /// The reviewer finished and covered its scope.
    Complete,
    /// The reviewer failed or missed files; its findings are still usable.
    Incomplete {
        /// Why the pass was incomplete.
        reason: String,
    },
}

impl ReviewCompleteness {
    /// Reports whether the pass was complete.
    pub const fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }

    /// Returns why the pass was incomplete, when it was.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Complete => None,
            Self::Incomplete { reason } => Some(reason),
        }
    }
}

/// What one reviewer pass produced.
///
/// This is a step's *decision*: what the pass concluded. It is persisted by a
/// [`DecisionLog`] so a repeat of the same step can be replayed from it rather
/// than asking the model to derive it again (ticket 09).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewOutcome {
    findings: Vec<ReviewFinding>,
    completeness: ReviewCompleteness,
}

impl ReviewOutcome {
    /// Creates an outcome.
    pub fn new(findings: Vec<ReviewFinding>, completeness: ReviewCompleteness) -> Self {
        Self {
            findings,
            completeness,
        }
    }

    /// Returns the normalised findings, in the order the reviewer emitted them.
    pub fn findings(&self) -> &[ReviewFinding] {
        &self.findings
    }

    /// Returns whether the pass covered its scope.
    pub const fn completeness(&self) -> &ReviewCompleteness {
        &self.completeness
    }
}

/// An AI reviewer that produces normalised findings for one pull request.
pub trait Reviewer: Send + Sync {
    /// Reviews one pull request.
    fn review(&self, request: &ReviewRequest) -> Result<ReviewOutcome, ReviewerError>;
}

/// A reviewer could not be run or could not be understood.
#[derive(Debug, Error)]
pub enum ReviewerError {
    /// The reviewer's executable is not installed.
    #[error("reviewer {reviewer} is not available: {detail}")]
    Unavailable {
        /// The reviewer that is missing.
        reviewer: &'static str,
        /// The underlying failure.
        detail: String,
    },
    /// The reviewer ran but failed in a way that produced nothing usable.
    #[error("reviewer {reviewer} failed: {detail}")]
    Command {
        /// The reviewer that failed.
        reviewer: &'static str,
        /// The underlying failure.
        detail: String,
    },
    /// A recorded decision could not be read back or written.
    #[error("the decision log for step {step} failed: {detail}")]
    DecisionLog {
        /// The step whose decision could not be read or written.
        step: String,
        /// The underlying failure.
        detail: String,
    },
}

/// A durable log of the decisions a step recorded, keyed by step identity.
///
/// This is the seam that makes a recorded decision replayable: a reader asks the
/// log what the step already decided, and only asks the model when the log has
/// nothing. The production implementation is the pool's decision sidecar
/// (`.jj/pools/<name>/decisions.json`); a test can supply its own.
pub trait DecisionLog: Send + Sync {
    /// The decision recorded for `step`, if any.
    fn decision(&self, step: &StepKey) -> Result<Option<ReviewOutcome>, DecisionLogError>;

    /// Records `decision` for `step`, replacing any prior one.
    fn record(&self, step: &StepKey, decision: &ReviewOutcome) -> Result<(), DecisionLogError>;
}

/// A recorded decision could not be read back or written.
#[derive(Debug, Error)]
#[error("{detail}")]
pub struct DecisionLogError {
    detail: String,
}

impl DecisionLogError {
    /// Creates a failure from its human-facing detail.
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

/// A [`Reviewer`] that records each decision and replays a recorded one instead
/// of re-invoking the model behind it.
///
/// The first call for its step asks the inner reviewer and records what it
/// decided. Every later call for the same step - a retry, a later round, a fresh
/// process - returns the recorded decision without calling the inner reviewer at
/// all, so a repeated step costs nothing and cannot silently diverge from what
/// was already decided (ticket 09). The step is what is replayed, so a different
/// pass or dimension is a different [`ReplayingReviewer`] over a different key.
pub struct ReplayingReviewer<S, R> {
    decisions: S,
    step: StepKey,
    inner: R,
}

impl<S, R> ReplayingReviewer<S, R> {
    /// Creates a reviewer that records and replays decisions for `step`.
    pub fn new(decisions: S, step: StepKey, inner: R) -> Self {
        Self {
            decisions,
            step,
            inner,
        }
    }

    /// Returns the step whose decision this reviewer records and replays.
    pub const fn step(&self) -> &StepKey {
        &self.step
    }
}

impl<S, R> Reviewer for ReplayingReviewer<S, R>
where
    S: DecisionLog,
    R: Reviewer,
{
    fn review(&self, request: &ReviewRequest) -> Result<ReviewOutcome, ReviewerError> {
        let step = self.step.as_str().to_owned();
        if let Some(recorded) =
            self.decisions
                .decision(&self.step)
                .map_err(|error| ReviewerError::DecisionLog {
                    step: step.clone(),
                    detail: error.to_string(),
                })?
        {
            return Ok(recorded);
        }
        let outcome = self.inner.review(request)?;
        self.decisions
            .record(&self.step, &outcome)
            .map_err(|error| ReviewerError::DecisionLog {
                step,
                detail: error.to_string(),
            })?;
        Ok(outcome)
    }
}

/// Why a bounded review loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewStop {
    /// The pass cap was reached while findings were still being produced.
    PassCap,
    /// A pass produced nothing to address, so there was nothing left to iterate.
    Converged,
    /// A round made no progress: it surfaced nothing new, or retired nothing, so
    /// another pass would only spend compute for the same findings.
    NoProgress,
}

impl ReviewStop {
    /// Returns the human-facing reason the loop stopped.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PassCap => "the pass cap",
            Self::Converged => "convergence (a pass found nothing)",
            Self::NoProgress => "no progress (a round found nothing new or retired nothing)",
        }
    }

    /// Returns whether the loop stopped with findings it could not resolve, so a
    /// human must pick them up rather than the loop reporting success.
    pub const fn needs_escalation(self) -> bool {
        !matches!(self, Self::Converged)
    }
}

/// The findings a bounded loop stopped with, for a human to pick up.
///
/// A loop that stops without converging has not resolved its last round's
/// findings - either it never addressed them, or it addressed them and had no
/// pass left to confirm they are gone. It reports them here instead of
/// discarding them, so hitting the bound escalates rather than silently
/// succeeding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewEscalation {
    findings: Vec<ReviewFinding>,
    stop: ReviewStop,
}

impl ReviewEscalation {
    /// Creates an escalation for the findings a loop stopped with.
    fn new(findings: Vec<ReviewFinding>, stop: ReviewStop) -> Self {
        Self { findings, stop }
    }

    /// Returns the findings the loop could not resolve, in the reviewer's order.
    pub fn findings(&self) -> &[ReviewFinding] {
        &self.findings
    }

    /// Returns why the loop stopped.
    pub const fn stop(&self) -> ReviewStop {
        self.stop
    }
}

/// What a bounded review loop did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewLoopSummary {
    passes: usize,
    findings: usize,
    stop: ReviewStop,
    escalation: Option<ReviewEscalation>,
}

impl ReviewLoopSummary {
    /// A loop that stopped because a pass produced nothing to address.
    fn converged(passes: usize, findings: usize) -> Self {
        Self {
            passes,
            findings,
            stop: ReviewStop::Converged,
            escalation: None,
        }
    }

    /// A loop that stopped with findings still unresolved.
    fn escalated(
        passes: usize,
        findings: usize,
        stop: ReviewStop,
        unresolved: Vec<ReviewFinding>,
    ) -> Self {
        Self {
            passes,
            findings,
            stop,
            escalation: Some(ReviewEscalation::new(unresolved, stop)),
        }
    }

    /// Returns the number of reviewer passes that ran.
    pub const fn passes(&self) -> usize {
        self.passes
    }

    /// Returns the total number of findings seen across every pass.
    pub const fn findings(&self) -> usize {
        self.findings
    }

    /// Returns why the loop stopped.
    pub const fn stop(&self) -> ReviewStop {
        self.stop
    }

    /// Returns the escalation a loop that did not converge stopped with, or
    /// `None` when it converged.
    pub fn escalation(&self) -> Option<&ReviewEscalation> {
        self.escalation.as_ref()
    }

    /// Returns the findings the loop could not resolve, or an empty slice when it
    /// converged.
    pub fn unresolved(&self) -> &[ReviewFinding] {
        self.escalation
            .as_ref()
            .map_or(&[], |escalation| escalation.findings())
    }
}

/// A review loop step failed.
#[derive(Debug, Error)]
pub enum ReviewLoopError<E> {
    /// The reviewer could not produce findings.
    #[error(transparent)]
    Reviewer(#[from] ReviewerError),
    /// Addressing a pass's findings failed.
    #[error("the review address pass failed: {0}")]
    Address(E),
}

/// A bounded re-review loop: review, address, review again - never more than the
/// pass cap, and never another pass for a round that made no progress.
///
/// Two bounds hold, not one. The pass cap is the hard ceiling: the loop can
/// never run longer than `max_passes`, whatever the reviewer reports. The
/// progress rule stops it earlier. A round must both surface a finding the
/// previous pass had not and retire one it had, so a reviewer repeating itself
/// finds nothing new and a Worker fixing nothing retires nothing - either way
/// the round is a stall, and another pass would only buy the same findings
/// again. A pass that produces no findings at all has converged.
///
/// Whatever stops it, a loop that has not converged escalates its unresolved
/// findings rather than reporting success, so the bound hands work to a human
/// instead of spinning or silently passing.
#[derive(Debug, Clone, Copy)]
pub struct ReviewLoop {
    max_passes: usize,
}

impl ReviewLoop {
    /// Creates a loop that runs at most `max_passes` reviewer passes.
    pub const fn new(max_passes: usize) -> Self {
        Self { max_passes }
    }

    /// Returns the pass cap.
    pub const fn max_passes(&self) -> usize {
        self.max_passes
    }

    /// Runs the loop, calling `address` once per pass that produced findings.
    pub fn run<R, A, E>(
        &self,
        reviewer: &R,
        request: &ReviewRequest,
        mut address: A,
    ) -> Result<ReviewLoopSummary, ReviewLoopError<E>>
    where
        R: Reviewer + ?Sized,
        A: FnMut(usize, &ReviewOutcome) -> Result<(), E>,
    {
        let mut passes = 0;
        let mut findings = 0;
        let mut previous: Option<HashSet<ReviewFinding>> = None;
        let mut unresolved = Vec::new();
        for pass in 1..=self.max_passes {
            let outcome = reviewer.review(request)?;
            passes = pass;
            findings += outcome.findings().len();
            if outcome.findings().is_empty() {
                return Ok(ReviewLoopSummary::converged(passes, findings));
            }
            let current: HashSet<ReviewFinding> = outcome.findings().iter().cloned().collect();
            if let Some(previous) = previous.as_ref() {
                let found_new = current.difference(previous).next().is_some();
                let retired = previous.difference(&current).next().is_some();
                if !(found_new && retired) {
                    return Ok(ReviewLoopSummary::escalated(
                        passes,
                        findings,
                        ReviewStop::NoProgress,
                        outcome.findings().to_vec(),
                    ));
                }
            }
            previous = Some(current);
            unresolved = outcome.findings().to_vec();
            address(pass, &outcome).map_err(ReviewLoopError::Address)?;
        }
        Ok(ReviewLoopSummary::escalated(
            passes,
            findings,
            ReviewStop::PassCap,
            unresolved,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;

    fn request() -> ReviewRequest {
        ReviewRequest::new("owner/repo", "main", "adam/feature")
    }

    /// A finding with a distinct title, so two scripted findings are never equal
    /// and a pass can be told from the one before it.
    fn finding_titled(title: &str) -> ReviewFinding {
        ReviewFinding::new(
            ReviewSeverity::Major,
            Some("src/x.rs".to_owned()),
            Some((12, 14)),
            Some(title.to_owned()),
            "Cap the loop.",
            vec!["n = 3".to_owned()],
        )
    }

    /// A scripted pass's findings, one per title.
    fn pass(titles: &[&str]) -> Vec<ReviewFinding> {
        titles.iter().map(|title| finding_titled(title)).collect()
    }

    /// The titles of a set of findings, for asserting which a report carries.
    fn titles(findings: &[ReviewFinding]) -> Vec<&str> {
        findings.iter().filter_map(ReviewFinding::title).collect()
    }

    /// A reviewer that returns a scripted set of findings per pass, then nothing.
    struct ScriptedReviewer {
        passes: Vec<Vec<ReviewFinding>>,
        calls: AtomicUsize,
    }

    impl ScriptedReviewer {
        fn new(passes: Vec<Vec<ReviewFinding>>) -> Self {
            Self {
                passes,
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl Reviewer for ScriptedReviewer {
        fn review(&self, _: &ReviewRequest) -> Result<ReviewOutcome, ReviewerError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let findings = self.passes.get(call).cloned().unwrap_or_default();
            Ok(ReviewOutcome::new(findings, ReviewCompleteness::Complete))
        }
    }

    /// A reviewer that never stops producing a fresh finding and never retires
    /// one, as a reviewer whose rounds would otherwise run to the cap does: every
    /// pass adds a finding and clears none.
    #[derive(Default)]
    struct EndlessReviewer {
        calls: AtomicUsize,
    }

    impl Reviewer for EndlessReviewer {
        fn review(&self, _: &ReviewRequest) -> Result<ReviewOutcome, ReviewerError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let findings = (0..=call)
                .map(|n| finding_titled(&format!("finding {n}")))
                .collect();
            Ok(ReviewOutcome::new(findings, ReviewCompleteness::Complete))
        }
    }

    struct FailingReviewer;

    impl Reviewer for FailingReviewer {
        fn review(&self, _: &ReviewRequest) -> Result<ReviewOutcome, ReviewerError> {
            Err(ReviewerError::Command {
                reviewer: "fake",
                detail: "boom".to_owned(),
            })
        }
    }

    #[test]
    fn the_loop_stops_at_the_pass_cap() {
        // Each round both surfaces a finding and retires one, so it makes
        // progress - and the cap still ends it.
        let reviewer =
            ScriptedReviewer::new(vec![pass(&["a"]), pass(&["b"]), pass(&["c"]), pass(&["d"])]);
        let mut addressed = Vec::new();

        let summary = ReviewLoop::new(3)
            .run(&reviewer, &request(), |pass, outcome| {
                addressed.push((pass, outcome.findings().len()));
                Ok::<(), String>(())
            })
            .expect("loop");

        assert_eq!(summary.passes(), 3);
        assert_eq!(summary.findings(), 3);
        assert_eq!(summary.stop(), ReviewStop::PassCap);
        assert_eq!(addressed, [(1, 1), (2, 1), (3, 1)]);
        // The cap did not converge, so it escalates the last pass's findings.
        let escalation = summary.escalation().expect("escalation");
        assert_eq!(escalation.stop(), ReviewStop::PassCap);
        assert_eq!(titles(escalation.findings()), ["c"]);
    }

    #[test]
    fn the_loop_stops_when_a_pass_finds_nothing() {
        let reviewer = ScriptedReviewer::new(vec![pass(&["a", "b"]), pass(&[]), pass(&["c"])]);
        let mut addressed = 0;

        let summary = ReviewLoop::new(5)
            .run(&reviewer, &request(), |_, _| {
                addressed += 1;
                Ok::<(), String>(())
            })
            .expect("loop");

        assert_eq!(summary.passes(), 2);
        assert_eq!(summary.findings(), 2);
        assert_eq!(summary.stop(), ReviewStop::Converged);
        assert_eq!(addressed, 1);
        // Convergence is the one stop that is not an escalation.
        assert!(summary.escalation().is_none());
        assert!(summary.unresolved().is_empty());
        assert!(!summary.stop().needs_escalation());
    }

    #[test]
    fn the_loop_never_runs_more_than_the_cap() {
        let reviewer = ScriptedReviewer::new(vec![pass(&["a"]); 10]);

        let summary = ReviewLoop::new(1)
            .run(&reviewer, &request(), |_, _| Ok::<(), String>(()))
            .expect("loop");

        assert_eq!(summary.passes(), 1);
        assert_eq!(summary.stop(), ReviewStop::PassCap);
        assert_eq!(titles(summary.unresolved()), ["a"]);
    }

    #[test]
    fn the_loop_stops_when_a_round_finds_nothing_new() {
        // The reviewer repeats itself exactly: the second pass buys nothing.
        let reviewer = ScriptedReviewer::new(vec![pass(&["a", "b"]), pass(&["a", "b"])]);
        let mut addressed = 0;

        let summary = ReviewLoop::new(10)
            .run(&reviewer, &request(), |_, _| {
                addressed += 1;
                Ok::<(), String>(())
            })
            .expect("loop");

        assert_eq!(summary.passes(), 2);
        assert_eq!(summary.findings(), 4);
        assert_eq!(summary.stop(), ReviewStop::NoProgress);
        assert_eq!(addressed, 1);
        assert_eq!(titles(summary.unresolved()), ["a", "b"]);
    }

    #[test]
    fn the_loop_stops_when_a_round_retires_nothing() {
        // The measured failure: every round finds something new and fixes
        // nothing, so the findings never come down.
        let reviewer = ScriptedReviewer::new(vec![
            pass(&["a"]),
            pass(&["a", "b"]),
            pass(&["a", "b", "c"]),
        ]);

        let summary = ReviewLoop::new(10)
            .run(&reviewer, &request(), |_, _| Ok::<(), String>(()))
            .expect("loop");

        assert_eq!(summary.passes(), 2);
        assert_eq!(summary.stop(), ReviewStop::NoProgress);
        assert_eq!(titles(summary.unresolved()), ["a", "b"]);
    }

    #[test]
    fn a_reviewer_that_never_converges_terminates_on_the_progress_rule() {
        // Without the progress rule this loop runs a million passes, each adding
        // a finding and clearing none. The bound is what makes it terminate - and
        // terminate with a report, not a silent success.
        let reviewer = EndlessReviewer::default();
        let mut addressed = 0;

        let summary = ReviewLoop::new(1_000_000)
            .run(&reviewer, &request(), |_, _| {
                addressed += 1;
                Ok::<(), String>(())
            })
            .expect("loop");

        assert_eq!(summary.passes(), 2);
        assert_eq!(summary.findings(), 3);
        assert_eq!(summary.stop(), ReviewStop::NoProgress);
        assert_eq!(addressed, 1);
        assert_eq!(titles(summary.unresolved()), ["finding 0", "finding 1"]);
        assert_eq!(
            summary.escalation().expect("escalation").stop(),
            ReviewStop::NoProgress
        );
    }

    #[test]
    fn an_address_failure_surfaces_from_the_loop() {
        let reviewer = ScriptedReviewer::new(vec![pass(&["a"])]);

        let error = ReviewLoop::new(3)
            .run(&reviewer, &request(), |_, _| Err("address boom".to_owned()))
            .expect_err("address failure");

        assert!(matches!(error, ReviewLoopError::Address(message) if message == "address boom"));
    }

    #[test]
    fn a_reviewer_failure_surfaces_from_the_loop() {
        let error = ReviewLoop::new(3)
            .run(&FailingReviewer, &request(), |_, _| Ok::<(), String>(()))
            .expect_err("reviewer failure");

        assert!(matches!(error, ReviewLoopError::Reviewer(_)), "{error}");
    }

    /// An in-memory decision log, standing in for the pool's decision sidecar.
    ///
    /// A log is genuinely shared mutable state - that is what makes a decision
    /// replayable by a reader that did not write it - so the double shares its
    /// map; it is not standing in for a borrow the real store would not need.
    #[derive(Default, Clone)]
    struct InMemoryLog {
        decisions: Arc<Mutex<BTreeMap<StepKey, ReviewOutcome>>>,
    }

    impl DecisionLog for InMemoryLog {
        fn decision(&self, step: &StepKey) -> Result<Option<ReviewOutcome>, DecisionLogError> {
            Ok(self
                .decisions
                .lock()
                .expect("decision log")
                .get(step)
                .cloned())
        }

        fn record(&self, step: &StepKey, decision: &ReviewOutcome) -> Result<(), DecisionLogError> {
            self.decisions
                .lock()
                .expect("decision log")
                .insert(step.clone(), decision.clone());
            Ok(())
        }
    }

    /// A reviewer that fails loudly if it is ever asked, so a test can prove a
    /// replayed decision never reached the model.
    struct PanickingReviewer;

    impl Reviewer for PanickingReviewer {
        fn review(&self, _: &ReviewRequest) -> Result<ReviewOutcome, ReviewerError> {
            panic!("a replayed decision must not re-invoke the reviewer");
        }
    }

    /// Ticket 09: the first pass for a step records its decision, so a later pass
    /// over the same step is the same decision rather than a second model call.
    #[test]
    fn the_first_pass_for_a_step_records_its_decision() {
        let step = StepKey::review("ENG-1", "security", 1);
        let log = InMemoryLog::default();
        let inner = ScriptedReviewer::new(vec![pass(&["bound the loop"])]);

        let outcome = ReplayingReviewer::new(log.clone(), step.clone(), inner)
            .review(&request())
            .expect("first pass");

        assert_eq!(titles(outcome.findings()), ["bound the loop"]);
        assert_eq!(
            log.decision(&step).expect("read"),
            Some(ReviewOutcome::new(
                pass(&["bound the loop"]),
                ReviewCompleteness::Complete
            )),
            "the decision must be recorded under the step's identity"
        );
    }

    /// Ticket 09, the strongest criterion: a recorded decision is replayed from
    /// what was stored, without the model being asked to derive it again.
    #[test]
    fn a_recorded_decision_is_replayed_without_calling_the_reviewer() {
        let step = StepKey::review("ENG-1", "security", 1);
        let log = InMemoryLog::default();
        log.record(
            &step,
            &ReviewOutcome::new(pass(&["already decided"]), ReviewCompleteness::Complete),
        )
        .expect("record a decision");

        let outcome = ReplayingReviewer::new(log, step, PanickingReviewer)
            .review(&request())
            .expect("replay");

        assert_eq!(titles(outcome.findings()), ["already decided"]);
    }

    /// A step the log has nothing for is the model's to decide: the replay
    /// reviewer records the fresh outcome so the next pass can replay it.
    #[test]
    fn a_step_the_log_does_not_know_is_decided_by_the_reviewer() {
        let step = StepKey::review("ENG-1", "security", 1);
        let log = InMemoryLog::default();
        let inner = ScriptedReviewer::new(vec![pass(&["fresh"])]);

        let outcome = ReplayingReviewer::new(log.clone(), step.clone(), inner)
            .review(&request())
            .expect("decide");

        assert_eq!(titles(outcome.findings()), ["fresh"]);
        assert!(log.decision(&step).expect("read").is_some());
    }
}
