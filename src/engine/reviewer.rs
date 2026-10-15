//! The configured AI reviewer: invoke it, normalise its findings, and bound the
//! re-review loop.
//!
//! The first adapter is the CodeRabbit CLI in agent mode. `cr review --agent`
//! writes one JSON object per line to stdout and reports completeness through its
//! exit code and its `complete` event, so the adapter keeps findings that were
//! emitted before a failure and reports the run as incomplete rather than
//! discarding them.

use std::io;
use std::process::Command;

use serde_json::Value;
use thiserror::Error;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

/// The CodeRabbit CLI in agent mode.
#[derive(Debug, Clone)]
pub struct CodeRabbitReviewer {
    program: String,
}

impl Default for CodeRabbitReviewer {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeRabbitReviewer {
    /// Creates the adapter over the `cr` executable.
    pub fn new() -> Self {
        Self {
            program: "cr".to_owned(),
        }
    }

    /// Creates the adapter over a specific executable, for tests and for a
    /// non-default install location.
    pub fn with_program(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// The reviewer's command line.
    ///
    /// It never carries a credential: `cr` reads its own stored auth (`cr auth
    /// login --api-key`) or environment, so no secret is ever visible in `ps`.
    fn arguments(request: &ReviewRequest) -> Vec<String> {
        vec![
            "review".to_owned(),
            "--agent".to_owned(),
            "--remote".to_owned(),
            request.repository().to_owned(),
            "--base".to_owned(),
            request.base().to_owned(),
            "--source-branch".to_owned(),
            request.source_branch().to_owned(),
        ]
    }
}

impl Reviewer for CodeRabbitReviewer {
    fn review(&self, request: &ReviewRequest) -> Result<ReviewOutcome, ReviewerError> {
        let output = Command::new(&self.program)
            .args(Self::arguments(request))
            .output()
            .map_err(|source| match source.kind() {
                io::ErrorKind::NotFound => ReviewerError::Unavailable {
                    reviewer: "coderabbit",
                    detail: format!("{} was not found on PATH", self.program),
                },
                _ => ReviewerError::Command {
                    reviewer: "coderabbit",
                    detail: source.to_string(),
                },
            })?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let (findings, completion) = parse_agent_stream(&stdout);
        let code = output.status.code();
        if !matches!(code, Some(0 | 1)) {
            return Err(ReviewerError::Command {
                reviewer: "coderabbit",
                detail: format!(
                    "exited with {code:?}: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }
        let completeness = completeness_from(code, completion.as_ref());
        Ok(ReviewOutcome::new(findings, completeness))
    }
}

/// The `complete` event's fields, as far as the adapter reads them.
#[derive(Debug, Default)]
struct Completion {
    status: Option<String>,
    outcome: Option<String>,
    message: Option<String>,
    unreviewed_file_count: Option<u64>,
}

/// Parses agent-mode NDJSON into findings and the final `complete` event.
///
/// Lines that are not JSON, or that carry an event type this build does not
/// know, are ignored: the stream is additive, so an unknown event must not fail
/// a review.
fn parse_agent_stream(stdout: &str) -> (Vec<ReviewFinding>, Option<Completion>) {
    let mut findings = Vec::new();
    let mut completion = None;
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match event.get("type").and_then(Value::as_str) {
            Some("finding") => {
                if let Some(finding) = finding_from(&event) {
                    findings.push(finding);
                }
            }
            Some("complete") => {
                completion = Some(Completion {
                    status: string_field(&event, "status"),
                    outcome: string_field(&event, "outcome"),
                    message: string_field(&event, "message"),
                    unreviewed_file_count: event.get("unreviewedFileCount").and_then(Value::as_u64),
                });
            }
            _ => {}
        }
    }
    (findings, completion)
}

/// Builds one finding from a `finding` event, or `None` when it carries no
/// actionable text at all.
fn finding_from(event: &Value) -> Option<ReviewFinding> {
    let body = string_field(event, "codegenInstructions")
        .or_else(|| string_field(event, "comment"))
        .or_else(|| string_field(event, "title"))?;
    let severity = string_field(event, "severity")
        .map_or(ReviewSeverity::None, |value| ReviewSeverity::parse(&value));
    let lines = match (
        event.get("startLine").and_then(Value::as_u64),
        event.get("endLine").and_then(Value::as_u64),
    ) {
        (Some(start), Some(end)) => Some((start, end)),
        (Some(start), None) => Some((start, start)),
        _ => None,
    };
    Some(ReviewFinding::new(
        severity,
        string_field(event, "fileName"),
        lines,
        string_field(event, "title"),
        body,
        string_list_field(event, "suggestions"),
    ))
}

/// Reads a non-blank string field.
fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(str::to_owned)
}

/// Reads an array field as strings, rendering non-string elements as JSON so a
/// structured suggestion is not lost.
fn string_list_field(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match item {
                    Value::String(text) => Some(text.clone()),
                    Value::Null => None,
                    other => Some(other.to_string()),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Decides whether a pass was complete from its exit code and `complete` event.
fn completeness_from(code: Option<i32>, completion: Option<&Completion>) -> ReviewCompleteness {
    let mut reason = match code {
        Some(0) => None,
        Some(1) => Some("the reviewer reported an incomplete review".to_owned()),
        other => Some(format!("the reviewer exited with {other:?}")),
    };
    if let Some(completion) = completion {
        if completion.status.as_deref() == Some("review_skipped") {
            return ReviewCompleteness::Complete;
        }
        if completion.outcome.as_deref() == Some("failed") {
            reason = Some(completion_message(
                completion,
                "the reviewer reported a failed review",
            ));
        }
        if completion
            .unreviewed_file_count
            .is_some_and(|count| count > 0)
        {
            reason = Some(completion_message(
                completion,
                "the reviewer did not cover every file",
            ));
        }
    }
    match reason {
        Some(reason) => ReviewCompleteness::Incomplete { reason },
        None => ReviewCompleteness::Complete,
    }
}

/// Prefers the reviewer's own message over the adapter's generic one.
fn completion_message(completion: &Completion, fallback: &str) -> String {
    completion
        .message
        .clone()
        .unwrap_or_else(|| fallback.to_owned())
}

/// Why a bounded review loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewStop {
    /// The pass cap was reached while findings were still being produced.
    PassCap,
    /// A pass produced nothing to address, so there was nothing left to iterate.
    Converged,
}

impl ReviewStop {
    /// Returns the human-facing reason the loop stopped.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PassCap => "the pass cap",
            Self::Converged => "convergence (a pass found nothing)",
        }
    }
}

/// What a bounded review loop did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewLoopSummary {
    passes: usize,
    findings: usize,
    stop: ReviewStop,
}

impl ReviewLoopSummary {
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
/// pass cap.
///
/// The cap is the bound. A pass that produces nothing to address stops the loop
/// early because there is nothing to send a Worker, not because a findings count
/// was reached; the loop can never run longer than the cap, whatever the reviewer
/// reports.
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
        for pass in 1..=self.max_passes {
            let outcome = reviewer.review(request)?;
            passes = pass;
            findings += outcome.findings().len();
            if outcome.findings().is_empty() {
                return Ok(ReviewLoopSummary {
                    passes,
                    findings,
                    stop: ReviewStop::Converged,
                });
            }
            address(pass, &outcome).map_err(ReviewLoopError::Address)?;
        }
        Ok(ReviewLoopSummary {
            passes,
            findings,
            stop: ReviewStop::PassCap,
        })
    }
}
