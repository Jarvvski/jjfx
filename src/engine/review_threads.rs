//! The findings source jjfx owns the review loop over: a Pull Request's
//! unresolved review threads, read through `gh`.
//!
//! ADR 0016 keeps the review bound in jjfx rather than in a prompt, and the
//! owner named the findings that bound runs over: the Pull Request's unresolved
//! review threads. [`ThreadReviewer`] maps each open thread onto a
//! [`ReviewFinding`], so the bounded [`crate::engine::ReviewLoop`] sees a
//! reviewer it can converge against - a Pull Request with no unresolved threads
//! is a pass that found nothing.
//!
//! Three decisions are deliberate and documented here rather than left implicit:
//!
//! - **Addressing a thread does not resolve it.** Resolution is the reviewer's
//!   action - a human or a bot resolves the conversation - not something jjfx or
//!   the Worker does. So a round whose threads are all still open retires
//!   nothing, the loop's progress rule sees a stall, and it stops on
//!   [`crate::engine::ReviewStop::NoProgress`] rather than claiming the review
//!   passed. That is the honest reading: the Worker's self-report is not
//!   verification (ADR 0016).
//! - **A plain issue comment is not a finding.** Only a review thread carries an
//!   `isResolved` state; a comment has none, so it can never be retired and would
//!   make the loop unable to converge. The address prompt still points the Worker
//!   at the PR's comments, but the bound counts threads alone.
//! - **A Pull Request with no review at all converges on the first pass.** No
//!   threads is no findings, which is exactly the empty pass the loop stops on -
//!   stated rather than left to fall out of the mapping.
//!
//! The read is behind [`ReviewThreads`] so a test supplies threads without a
//! network; [`GhReviewThreads`] is the production `gh` implementation.

use std::process::Command;

use serde::Deserialize;

use crate::engine::reviewer::{
    ReviewCompleteness, ReviewFinding, ReviewOutcome, ReviewRequest, ReviewSeverity, Reviewer,
    ReviewerError,
};

/// The review dimension the Pull Request's threads are reviewed under, so a pass
/// is keyed `review:<ticket>:threads:<pass>` in the decision log and the queue.
pub const REVIEW_THREADS_DIMENSION: &str = "threads";

/// How much of a thread's first line becomes the finding's title.
const TITLE_MAX_CHARS: usize = 100;

/// One comment on a review thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewThreadComment {
    author: Option<String>,
    body: String,
}

impl ReviewThreadComment {
    /// Creates a comment.
    pub fn new(author: Option<String>, body: impl Into<String>) -> Self {
        Self {
            author,
            body: body.into(),
        }
    }

    /// Returns the comment's author login, when the forge named one.
    pub fn author(&self) -> Option<&str> {
        self.author.as_deref()
    }

    /// Returns the comment's body, as the reviewer wrote it.
    pub fn body(&self) -> &str {
        &self.body
    }
}

/// One unresolved review thread, as read from the forge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewThread {
    path: Option<String>,
    start_line: Option<u64>,
    line: Option<u64>,
    comments: Vec<ReviewThreadComment>,
}

impl ReviewThread {
    /// Creates a thread.
    pub fn new(
        path: Option<String>,
        start_line: Option<u64>,
        line: Option<u64>,
        comments: Vec<ReviewThreadComment>,
    ) -> Self {
        Self {
            path,
            start_line,
            line,
            comments,
        }
    }

    /// Returns the file the thread is anchored to, when the forge named one.
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// Returns the line range the thread covers, when the forge gave one.
    pub fn lines(&self) -> Option<(u64, u64)> {
        match (self.start_line, self.line) {
            (Some(start), Some(end)) => Some((start.min(end), start.max(end))),
            (Some(start), None) => Some((start, start)),
            (None, Some(end)) => Some((end, end)),
            (None, None) => None,
        }
    }

    /// Returns the thread's comments, in the order the forge reported them.
    pub fn comments(&self) -> &[ReviewThreadComment] {
        &self.comments
    }

    /// Returns the thread's title: the first non-empty line of its first comment.
    pub fn title(&self) -> Option<String> {
        let first = self.comments.first()?;
        let line = first
            .body()
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())?;
        Some(line.chars().take(TITLE_MAX_CHARS).collect())
    }

    /// Returns the thread's comments as one body, each attributed to its author.
    pub fn body(&self) -> String {
        self.comments
            .iter()
            .map(|comment| match comment.author() {
                Some(author) => format!("@{author}: {}", comment.body()),
                None => comment.body().to_owned(),
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Returns the code blocks the thread proposed as suggested changes.
    pub fn suggestions(&self) -> Vec<String> {
        self.comments
            .iter()
            .flat_map(|comment| suggested_changes(comment.body()))
            .collect()
    }
}

/// Extracts `suggestion` fenced code blocks from a comment body.
///
/// GitHub renders a fenced block whose info string is `suggestion` as a
/// one-click change, so it is the thread's structured fix rather than prose. An
/// unrecognised fence is left in the body, where it is still readable.
fn suggested_changes(body: &str) -> Vec<String> {
    let mut suggestions = Vec::new();
    let mut lines = body.lines();
    while let Some(line) = lines.next() {
        if !line.trim_start().starts_with("```suggestion") {
            continue;
        }
        let mut block = String::new();
        for line in lines.by_ref() {
            if line.trim_start().starts_with("```") {
                break;
            }
            if !block.is_empty() {
                block.push('\n');
            }
            block.push_str(line);
        }
        suggestions.push(block);
    }
    suggestions
}

/// Maps one thread onto a normalised finding.
///
/// A review thread carries no severity, so the finding's severity is
/// [`ReviewSeverity::None`] - the honest value, and the one the loop does not
/// order by. The thread's file, lines, title, comments and suggested changes
/// carry through.
fn finding_from_thread(thread: &ReviewThread) -> ReviewFinding {
    ReviewFinding::new(
        ReviewSeverity::None,
        thread.path().map(str::to_owned),
        thread.lines(),
        thread.title(),
        thread.body(),
        thread.suggestions(),
    )
}

/// A source of a Pull Request's unresolved review threads.
///
/// A failure to read is an `Err`, never an empty list: an unreadable review is
/// not an empty one, and [`ThreadReviewer`] turns the error into an incomplete
/// outcome so the loop cannot read it as clean.
pub trait ReviewThreads: Send + Sync {
    /// Reads the unresolved review threads of `request`'s Pull Request.
    fn unresolved(&self, request: &ReviewRequest) -> Result<Vec<ReviewThread>, String>;
}

/// A [`Reviewer`] over a Pull Request's unresolved review threads.
///
/// A Pull Request with no unresolved threads is a pass with no findings, which is
/// what lets the loop converge; a read that failed is an incomplete pass, which
/// the loop escalates instead of reading as clean.
pub struct ThreadReviewer<S> {
    threads: S,
}

impl<S> ThreadReviewer<S> {
    /// Creates a reviewer that reads its threads from `threads`.
    pub fn new(threads: S) -> Self {
        Self { threads }
    }
}

impl<S: ReviewThreads> Reviewer for ThreadReviewer<S> {
    fn review(&self, request: &ReviewRequest) -> Result<ReviewOutcome, ReviewerError> {
        match self.threads.unresolved(request) {
            Ok(threads) => Ok(ReviewOutcome::new(
                threads.iter().map(finding_from_thread).collect(),
                ReviewCompleteness::Complete,
            )),
            Err(reason) => Ok(ReviewOutcome::new(
                Vec::new(),
                ReviewCompleteness::Incomplete { reason },
            )),
        }
    }
}

/// The GraphQL query that reads a Pull Request's review threads and their
/// resolution state.
///
/// `gh pr view --json` exposes no review threads, so the threads are read
/// through the API the resolution state actually lives on. Only the unresolved
/// ones become findings; the resolved ones are the threads a reviewer has
/// already closed, which is what lets the loop's progress rule see a round
/// retire one.
const REVIEW_THREADS_QUERY: &str = r#"query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(first: 100) {
        nodes {
          isResolved
          path
          startLine
          line
          comments(first: 100) {
            nodes {
              author { login }
              body
            }
          }
        }
      }
    }
  }
}"#;

/// The production [`ReviewThreads`]: the Pull Request's review threads read
/// through `gh api graphql`.
pub struct GhReviewThreads;

impl ReviewThreads for GhReviewThreads {
    fn unresolved(&self, request: &ReviewRequest) -> Result<Vec<ReviewThread>, String> {
        let Some(number) = request.number() else {
            return Err(format!(
                "the review request for {} names no Pull Request number",
                request.repository()
            ));
        };
        let Some((owner, name)) = request.repository().split_once('/') else {
            return Err(format!(
                "`{}` is not an `owner/name` repository",
                request.repository()
            ));
        };
        let command = format!(
            "gh api graphql -R {} (review threads of #{number})",
            request.repository()
        );
        let output = Command::new("gh")
            .args([
                "api",
                "graphql",
                "-f",
                &format!("query={REVIEW_THREADS_QUERY}"),
                "-F",
                &format!("owner={owner}"),
                "-F",
                &format!("name={name}"),
                "-F",
                &format!("number={number}"),
            ])
            .output()
            .map_err(|error| format!("cannot run `{command}`: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "`{command}` failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let response: GraphQlResponse = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("cannot read `{command}` output: {error}"))?;
        Ok(response
            .data
            .repository
            .pull_request
            .review_threads
            .nodes
            .into_iter()
            .filter(|thread| !thread.is_resolved)
            .map(ReviewThread::from)
            .collect())
    }
}

#[derive(Debug, Deserialize)]
struct GraphQlResponse {
    data: GraphQlData,
}

#[derive(Debug, Deserialize)]
struct GraphQlData {
    repository: GraphQlRepository,
}

#[derive(Debug, Deserialize)]
struct GraphQlRepository {
    #[serde(rename = "pullRequest")]
    pull_request: GraphQlPullRequest,
}

#[derive(Debug, Deserialize)]
struct GraphQlPullRequest {
    #[serde(rename = "reviewThreads", default)]
    review_threads: GraphQlThreads,
}

#[derive(Debug, Default, Deserialize)]
struct GraphQlThreads {
    #[serde(default)]
    nodes: Vec<GraphQlThread>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphQlThread {
    #[serde(default)]
    is_resolved: bool,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    start_line: Option<u64>,
    #[serde(default)]
    line: Option<u64>,
    #[serde(default)]
    comments: GraphQlComments,
}

#[derive(Debug, Default, Deserialize)]
struct GraphQlComments {
    #[serde(default)]
    nodes: Vec<GraphQlComment>,
}

#[derive(Debug, Deserialize)]
struct GraphQlComment {
    #[serde(default)]
    author: Option<GraphQlAuthor>,
    #[serde(default)]
    body: String,
}

#[derive(Debug, Deserialize)]
struct GraphQlAuthor {
    #[serde(default)]
    login: Option<String>,
}

impl From<GraphQlThread> for ReviewThread {
    fn from(thread: GraphQlThread) -> Self {
        Self {
            path: thread.path,
            start_line: thread.start_line,
            line: thread.line,
            comments: thread
                .comments
                .nodes
                .into_iter()
                .map(ReviewThreadComment::from)
                .collect(),
        }
    }
}

impl From<GraphQlComment> for ReviewThreadComment {
    fn from(comment: GraphQlComment) -> Self {
        Self {
            author: comment.author.and_then(|author| author.login),
            body: comment.body,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source that answers with a scripted set of unresolved threads.
    struct ScriptedThreads {
        threads: Vec<ReviewThread>,
    }

    impl ReviewThreads for ScriptedThreads {
        fn unresolved(&self, _: &ReviewRequest) -> Result<Vec<ReviewThread>, String> {
            Ok(self.threads.clone())
        }
    }

    /// A source whose read fails, as an unauthenticated or unreachable `gh` does.
    struct UnreadableThreads;

    impl ReviewThreads for UnreadableThreads {
        fn unresolved(&self, _: &ReviewRequest) -> Result<Vec<ReviewThread>, String> {
            Err("gh: not authenticated".to_owned())
        }
    }

    fn request() -> ReviewRequest {
        ReviewRequest::new("owner/repo", "main", "adam/feature").with_number(7)
    }

    fn comment(author: &str, body: &str) -> ReviewThreadComment {
        ReviewThreadComment::new(Some(author.to_owned()), body)
    }

    /// The happy path: each unresolved thread becomes a finding carrying the
    /// thread's file, lines, title, body and suggested changes.
    #[test]
    fn unresolved_threads_become_findings() {
        let source = ScriptedThreads {
            threads: vec![ReviewThread::new(
                Some("src/x.rs".to_owned()),
                Some(12),
                Some(14),
                vec![
                    comment("adam", "Cap the loop.\n\nThis can spin forever."),
                    comment("bot", "```suggestion\nn = 3\n```"),
                ],
            )],
        };

        let outcome = ThreadReviewer::new(source)
            .review(&request())
            .expect("review");

        assert!(outcome.completeness().is_complete());
        assert_eq!(outcome.findings().len(), 1);
        let finding = &outcome.findings()[0];
        assert_eq!(finding.severity(), ReviewSeverity::None);
        assert_eq!(finding.file(), Some("src/x.rs"));
        assert_eq!(finding.lines(), Some((12, 14)));
        assert_eq!(finding.title(), Some("Cap the loop."));
        assert!(finding.body().contains("@adam: Cap the loop."));
        assert!(finding.body().contains("@bot: ```suggestion"));
        assert_eq!(finding.suggestions(), ["n = 3"]);
    }

    /// A Pull Request with no unresolved threads is a pass with no findings,
    /// which is what lets the loop converge without sending anything.
    #[test]
    fn no_unresolved_threads_is_a_complete_pass_with_no_findings() {
        let source = ScriptedThreads { threads: vec![] };

        let outcome = ThreadReviewer::new(source)
            .review(&request())
            .expect("review");

        assert!(outcome.completeness().is_complete());
        assert!(outcome.findings().is_empty());
    }

    /// A read that failed is not "no findings": it is an incomplete pass, which
    /// the loop escalates rather than reading as clean.
    #[test]
    fn an_unreadable_source_is_an_incomplete_pass() {
        let outcome = ThreadReviewer::new(UnreadableThreads)
            .review(&request())
            .expect("review");

        assert!(!outcome.completeness().is_complete());
        assert_eq!(
            outcome.completeness().reason(),
            Some("gh: not authenticated")
        );
        assert!(outcome.findings().is_empty());
    }

    /// A thread anchored to one line reports that line; a thread with no line
    /// reports no range rather than a fabricated one.
    #[test]
    fn a_thread_reports_the_lines_it_has_and_no_more() {
        let single = ReviewThread::new(None, Some(9), Some(9), vec![]);
        assert_eq!(single.lines(), Some((9, 9)));
        assert_eq!(single.path(), None);
        let unanchored = ReviewThread::new(None, None, None, vec![]);
        assert_eq!(unanchored.lines(), None);
        assert_eq!(unanchored.title(), None);
    }

    /// The title is the first non-empty line, bounded, so a long thread body
    /// does not become an unbounded summary.
    #[test]
    fn the_title_is_the_first_non_empty_line_bounded() {
        let thread = ReviewThread::new(
            Some("a.rs".to_owned()),
            None,
            None,
            vec![comment("adam", "\n\n  A short title  \nmore detail")],
        );
        assert_eq!(thread.title().as_deref(), Some("A short title"));

        let long = "x".repeat(TITLE_MAX_CHARS + 50);
        let thread = ReviewThread::new(
            Some("a.rs".to_owned()),
            None,
            None,
            vec![comment("adam", &long)],
        );
        assert_eq!(
            thread.title().expect("title").chars().count(),
            TITLE_MAX_CHARS
        );
    }

    /// Only a `suggestion` fence is a suggested change; another fence stays in
    /// the body.
    #[test]
    fn only_suggestion_fences_become_suggestions() {
        let body = "Here:\n```suggestion\nlet a = 0;\n```\nand\n```rust\nlet b = 1;\n```\n";
        assert_eq!(suggested_changes(body), ["let a = 0;"]);
    }

    /// The `gh` reader refuses a request that names no Pull Request rather than
    /// contacting the forge, and reads the captured thread shape a current `gh`
    /// returns.
    #[test]
    fn the_gh_reader_refuses_a_request_without_a_number() {
        let request = ReviewRequest::new("owner/repo", "main", "adam/feature");
        let error = GhReviewThreads
            .unresolved(&request)
            .expect_err("no number must be refused");

        assert!(error.contains("names no Pull Request number"), "{error}");
    }

    /// The deserialiser reads the shape `gh api graphql` returns, keeping only
    /// the unresolved threads.
    #[test]
    fn the_response_keeps_only_unresolved_threads() {
        let body = br#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
            {"isResolved":true,"path":"a.rs","line":1,"comments":{"nodes":[{"author":{"login":"a"},"body":"done"}]}},
            {"isResolved":false,"path":"b.rs","startLine":3,"line":5,"comments":{"nodes":[{"author":{"login":"b"},"body":"open"}]}}
        ]}}}}}"#;

        let response: GraphQlResponse = serde_json::from_slice(body).expect("captured body parses");
        let threads: Vec<ReviewThread> = response
            .data
            .repository
            .pull_request
            .review_threads
            .nodes
            .into_iter()
            .filter(|thread| !thread.is_resolved)
            .map(ReviewThread::from)
            .collect();

        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].path(), Some("b.rs"));
        assert_eq!(threads[0].lines(), Some((3, 5)));
        assert_eq!(threads[0].body(), "@b: open");
    }
}
