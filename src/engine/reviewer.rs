//! The configured AI reviewer: invoke it, normalise its findings, and bound the
//! re-review loop.
//!
//! The first adapter is the CodeRabbit CLI in agent mode. `cr review --agent`
//! writes one JSON object per line to stdout and reports completeness through its
//! exit code and its `complete` event, so the adapter keeps findings that were
//! emitted before a failure and reports the run as incomplete rather than
//! discarding them.

use std::collections::HashSet;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use tempfile::TempDir;

    fn write_executable(directory: &Path, name: &str, contents: &str) -> String {
        let path = directory.join(name);
        fs::write(&path, contents).expect("script");
        let mut permissions = fs::metadata(&path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("permissions");
        path.display().to_string()
    }

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

    #[test]
    fn the_coderabbit_adapter_normalises_agent_ndjson() {
        let directory = TempDir::new().expect("temporary directory");
        let program = write_executable(
            directory.path(),
            "cr",
            "#!/bin/sh\n\
             printf '%s\\n' '{\"type\":\"review_context\",\"remote\":\"owner/repo\"}'\n\
             printf '%s\\n' '{\"type\":\"heartbeat\"}'\n\
             printf '%s\\n' '{\"type\":\"finding\",\"severity\":\"major\",\"fileName\":\"src/x.rs\",\"startLine\":12,\"endLine\":14,\"title\":\"Unbounded loop\",\"codegenInstructions\":\"Cap the loop.\",\"suggestions\":[\"n = 3\"]}'\n\
             printf '%s\\n' '{\"type\":\"finding\",\"severity\":\"minor\",\"fileName\":\"src/y.rs\",\"comment\":\"Rename this.\"}'\n\
             printf '%s\\n' '{\"type\":\"complete\",\"status\":\"review_completed\",\"outcome\":\"completed\"}'\n",
        );
        let reviewer = CodeRabbitReviewer::with_program(program);

        let outcome = reviewer.review(&request()).expect("review");

        assert_eq!(outcome.findings().len(), 2, "{:?}", outcome.findings());
        let first = &outcome.findings()[0];
        assert_eq!(first.severity(), ReviewSeverity::Major);
        assert_eq!(first.location().as_deref(), Some("src/x.rs:12-14"));
        assert_eq!(first.title(), Some("Unbounded loop"));
        assert_eq!(first.body(), "Cap the loop.");
        assert_eq!(first.suggestions(), ["n = 3"]);
        assert_eq!(first.summary(), "[major] src/x.rs:12-14 - Unbounded loop");
        // A finding with no codegen instructions falls back to its comment.
        assert_eq!(outcome.findings()[1].body(), "Rename this.");
        assert_eq!(outcome.completeness(), &ReviewCompleteness::Complete);
    }

    #[test]
    fn an_incomplete_review_keeps_the_findings_it_emitted() {
        let directory = TempDir::new().expect("temporary directory");
        let program = write_executable(
            directory.path(),
            "cr",
            "#!/bin/sh\n\
             printf '%s\\n' '{\"type\":\"finding\",\"severity\":\"critical\",\"fileName\":\"src/x.rs\",\"codegenInstructions\":\"Fix it.\"}'\n\
             printf '%s\\n' '{\"type\":\"complete\",\"status\":\"review_completed\",\"outcome\":\"failed\",\"unreviewedFileCount\":3,\"message\":\"missed 3 files\"}'\n\
             exit 1\n",
        );
        let reviewer = CodeRabbitReviewer::with_program(program);

        let outcome = reviewer.review(&request()).expect("review");

        assert_eq!(outcome.findings().len(), 1);
        assert!(!outcome.completeness().is_complete());
        assert_eq!(outcome.completeness().reason(), Some("missed 3 files"));
    }

    #[test]
    fn a_skipped_review_is_complete_with_no_findings() {
        let directory = TempDir::new().expect("temporary directory");
        let program = write_executable(
            directory.path(),
            "cr",
            "#!/bin/sh\n\
             printf '%s\\n' '{\"type\":\"status\",\"status\":\"review_skipped\"}'\n\
             printf '%s\\n' '{\"type\":\"complete\",\"status\":\"review_skipped\",\"findings\":0,\"message\":\"No changes detected\"}'\n",
        );
        let reviewer = CodeRabbitReviewer::with_program(program);

        let outcome = reviewer.review(&request()).expect("review");

        assert!(outcome.findings().is_empty());
        assert_eq!(outcome.completeness(), &ReviewCompleteness::Complete);
    }

    #[test]
    fn the_reviewer_command_never_carries_a_credential() {
        let directory = TempDir::new().expect("temporary directory");
        let argv_path = directory.path().join("argv.txt");
        let program = write_executable(
            directory.path(),
            "cr",
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nprintf '%s\\n' '{{\"type\":\"complete\",\"status\":\"review_completed\"}}'\n",
                argv_path.display()
            ),
        );
        let reviewer = CodeRabbitReviewer::with_program(program);

        reviewer.review(&request()).expect("review");

        let argv = fs::read_to_string(&argv_path).expect("argv");
        let argv = argv.lines().collect::<Vec<_>>();
        assert_eq!(
            argv,
            [
                "review",
                "--agent",
                "--remote",
                "owner/repo",
                "--base",
                "main",
                "--source-branch",
                "adam/feature",
            ]
        );
    }

    #[test]
    fn a_missing_reviewer_program_is_unavailable() {
        let reviewer = CodeRabbitReviewer::with_program("/nonexistent/jjfx-cr");

        let error = reviewer.review(&request()).expect_err("unavailable");

        assert!(
            matches!(error, ReviewerError::Unavailable { .. }),
            "{error}"
        );
    }

    #[test]
    fn a_reviewer_that_exits_with_a_signal_fails() {
        let directory = TempDir::new().expect("temporary directory");
        let program = write_executable(directory.path(), "cr", "#!/bin/sh\nexit 3\n");
        let reviewer = CodeRabbitReviewer::with_program(program);

        let error = reviewer.review(&request()).expect_err("command failure");

        assert!(matches!(error, ReviewerError::Command { .. }), "{error}");
    }

    #[test]
    fn non_json_lines_are_ignored() {
        let directory = TempDir::new().expect("temporary directory");
        let program = write_executable(
            directory.path(),
            "cr",
            "#!/bin/sh\n\
             printf '%s\\n' 'not json at all'\n\
             printf '%s\\n' '{\"type\":\"unknown_event\"}'\n\
             printf '%s\\n' '{\"type\":\"finding\",\"codegenInstructions\":\"Fix it.\"}'\n",
        );
        let reviewer = CodeRabbitReviewer::with_program(program);

        let outcome = reviewer.review(&request()).expect("review");

        assert_eq!(outcome.findings().len(), 1);
        assert_eq!(outcome.findings()[0].severity(), ReviewSeverity::None);
        assert!(outcome.findings()[0].location().is_none());
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
}
