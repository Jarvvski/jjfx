//! The forge pipeline (ADR 0005, ticket 08): fetch -> weld -> push -> pr, run
//! natively with the workspace-safe revsets ported from `jj-forge`, modeled as
//! real step state that streams to the TUI rather than scraped stdout. The final
//! step opens/updates Pull Requests over `gh`; GPG is probed before any rewrite
//! when signing is enabled.
//!
//! Each mutating step runs with its current directory set to the workspace's own
//! path, so `@` resolves to *that* workspace's working copy - forging one
//! workspace never rebases another's chain. Steps shell out via `spawn_blocking`
//! (jj/gh calls block), and callers receive fully folded progress snapshots.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::cmd::cmd;
use crate::config::ForgeConfig;

mod pull_requests;

// Both revsets below use jj's *bare* `trunk()` deliberately, NOT
// [`crate::trunk::as_revset`]. That module resolves the base the reads measure
// against - the latest of the remote mainline and local `main`/`master`/`trunk`,
// so an unpushed local `main` still counts as trunk. Weld and push instead target
// the *real remote* mainline: you rebase onto and push against what `origin`
// actually has, and a local `main` ahead of `origin` is not a push target. This is
// a named exception to the "one trunk" rule, not an accident (ADR-0007).

/// Weld source: the root of this workspace's own mutable chain, rebased onto
/// `trunk()`. Scoped to `::@` so only this workspace's stack moves.
const WELD_SRC: &str = "roots(mutable() & mine() & ::@)";
/// Push revisions: this workspace's chain, minus trunk and any conflicts.
const PUSH_REVS: &str = "::@ ~ trunk() ~ conflicts()";
/// jj prints this (exit 0) when a push had nothing to do - the signal that a
/// push moved no revisions despite succeeding.
const NOTHING_CHANGED: &str = "Nothing changed.";

/// The four forge steps, in pipeline order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Fetch the remote once for the whole run.
    Fetch,
    /// Rebase a Workspace's mutable chain onto trunk.
    Weld,
    /// Push the Workspace's revisions.
    Push,
    /// Create or update the Workspace's Pull Request stack.
    PullRequest,
}

impl Step {
    fn index(self) -> usize {
        match self {
            Step::Fetch => 0,
            Step::Weld => 1,
            Step::Push => 2,
            Step::PullRequest => 3,
        }
    }
}

/// Live status of one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    /// The step has not started.
    Pending,
    /// The step is running.
    Running,
    /// The step completed successfully.
    Ok,
    /// The step did nothing useful (nothing to push, weld hit conflicts, ...).
    Skipped,
}

/// A workspace to forge: its name and the directory whose `@` the revsets target.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    name: String,
    dir: PathBuf,
}

impl Target {
    /// Bind a Workspace name to the directory whose `@` Forge must operate on.
    pub(crate) fn new(name: String, directory: PathBuf) -> Self {
        Self {
            name,
            dir: directory,
        }
    }
}

/// A complete, valid progress snapshot for one Workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Progress {
    steps: [Status; 4],
    active: bool,
    reason: Option<String>,
}

impl Progress {
    fn running() -> Self {
        Self {
            steps: [Status::Pending; 4],
            active: true,
            reason: None,
        }
    }

    /// Whether the Workspace pipeline is still running.
    pub(crate) fn active(&self) -> bool {
        self.active
    }

    /// The four step statuses in display and execution order.
    pub(crate) fn steps(&self) -> [(Step, Status); 4] {
        [
            (Step::Fetch, self.steps[Step::Fetch.index()]),
            (Step::Weld, self.steps[Step::Weld.index()]),
            (Step::Push, self.steps[Step::Push.index()]),
            (Step::PullRequest, self.steps[Step::PullRequest.index()]),
        ]
    }

    /// The most recent retained skip reason, if the run was not clean.
    pub(crate) fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    fn set(&mut self, step: Step, status: Status) {
        self.steps[step.index()] = status;
    }

    fn clean_success(&self) -> bool {
        self.reason.is_none() && self.steps == [Status::Ok; 4]
    }
}

/// One streamed Forge outcome, already folded into valid progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Update {
    /// A Workspace's complete progress changed; `notice` is new footer text.
    Progress {
        workspace: String,
        progress: Progress,
        notice: Option<String>,
    },
    /// A Workspace completed the full pipeline; clean success has no progress.
    Finished {
        workspace: String,
        progress: Option<Progress>,
    },
    /// A run stopped before per-Workspace completion.
    Aborted { reason: String },
}

trait JjAdapter: Send + Sync {
    fn signing_key(&self) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn fetch(&self) -> Result<bool, String>;
    fn has_conflict(&self, _dir: &Path) -> Result<bool, String> {
        Ok(false)
    }
    fn weld(&self, dir: &Path) -> Result<bool, String>;
    fn push(&self, dir: &Path) -> Result<bool, String>;

    fn origin_slug(&self) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn changes(&self, _dir: &Path) -> Result<Vec<Change>, String> {
        Ok(Vec::new())
    }
}

trait GpgAdapter: Send + Sync {
    fn unlocked(&self, _key: &str) -> Result<bool, String> {
        Ok(true)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Change {
    bookmark: String,
    description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PullRequest {
    number: u64,
    head: String,
    state: String,
    body: String,
    merged: bool,
}

struct NewPullRequest {
    slug: String,
    head: String,
    base: String,
    title: String,
    body: String,
    draft: bool,
}

trait GitHubAdapter: Send + Sync {
    fn default_branch(&self, slug: &str) -> Result<String, String>;
    fn find(&self, slug: &str, head: &str) -> Result<Option<PullRequest>, String>;
    fn create(&self, request: NewPullRequest) -> Result<PullRequest, String>;
    fn edit(&self, slug: &str, number: u64, body: &str, base: &str) -> Result<(), String>;
}

/// The deep Forge module: one interface over execution, progress, and adapters.
pub(crate) struct Forge {
    config: ForgeConfig,
    jj: Arc<dyn JjAdapter>,
    gpg: Arc<dyn GpgAdapter>,
    github: Arc<dyn GitHubAdapter>,
}

impl Forge {
    /// Bind Forge's production adapters to one repository and configuration.
    pub(crate) fn new(repo_root: PathBuf, config: ForgeConfig) -> Self {
        Self {
            config,
            jj: Arc::new(SystemJj { repo_root }),
            gpg: Arc::new(SystemGpg),
            github: Arc::new(SystemGitHub),
        }
    }

    #[cfg(test)]
    fn with_adapters(
        config: ForgeConfig,
        jj: Arc<dyn JjAdapter>,
        gpg: Arc<dyn GpgAdapter>,
    ) -> Self {
        Self {
            config,
            jj,
            gpg,
            github: Arc::new(SystemGitHub),
        }
    }

    #[cfg(test)]
    fn with_github(
        config: ForgeConfig,
        jj: Arc<dyn JjAdapter>,
        gpg: Arc<dyn GpgAdapter>,
        github: Arc<dyn GitHubAdapter>,
    ) -> Self {
        Self {
            config,
            jj,
            gpg,
            github,
        }
    }

    /// Start one run and return its ordered progress stream immediately.
    pub(crate) fn start(&self, targets: Vec<Target>) -> UnboundedReceiver<Update> {
        let (tx, rx) = mpsc::unbounded_channel();
        let config = self.config;
        let jj = Arc::clone(&self.jj);
        let gpg = Arc::clone(&self.gpg);
        let github = Arc::clone(&self.github);
        tokio::task::spawn_blocking(move || {
            run_pipeline(&tx, &targets, config, &*jj, &*gpg, &*github)
        });
        rx
    }
}

struct SystemJj {
    repo_root: PathBuf,
}

impl JjAdapter for SystemJj {
    fn signing_key(&self) -> Result<Option<String>, String> {
        if self.config_value("signing.backend")?.as_deref() != Some("gpg") {
            return Ok(None);
        }
        let Some(key) = self.config_value("signing.key")? else {
            return Ok(None);
        };
        Ok((!key.is_empty()).then_some(key))
    }

    fn fetch(&self) -> Result<bool, String> {
        cmd("jj")
            .arg("--repository")
            .arg(&self.repo_root)
            .args(["git", "fetch"])
            .run()
            .map(|run| run.ok())
            .map_err(|error| error.to_string())
    }

    fn has_conflict(&self, dir: &Path) -> Result<bool, String> {
        cmd("jj")
            .current_dir(dir)
            .args([
                "--ignore-working-copy",
                "log",
                "-r",
                "@ & conflicts()",
                "--no-graph",
                "-T",
                "\"x\"",
            ])
            .run()
            .map(|run| run.ok() && !run.stdout().is_empty())
            .map_err(|error| error.to_string())
    }

    fn weld(&self, dir: &Path) -> Result<bool, String> {
        cmd("jj")
            .current_dir(dir)
            .args(["rebase", "--skip-emptied", "-s", WELD_SRC, "-d", "trunk()"])
            .run()
            .map(|run| run.ok())
            .map_err(|error| error.to_string())
    }

    fn push(&self, dir: &Path) -> Result<bool, String> {
        cmd("jj")
            .current_dir(dir)
            .args(["git", "push", "--revisions", PUSH_REVS])
            .run()
            .map(|run| pushed_something(run.ok(), run.stdout(), run.stderr()))
            .map_err(|error| error.to_string())
    }

    fn origin_slug(&self) -> Result<Option<String>, String> {
        crate::jj::derive_repo_slug(&self.repo_root).map_err(|error| error.to_string())
    }

    fn changes(&self, dir: &Path) -> Result<Vec<Change>, String> {
        let trunk = crate::trunk::as_revset();
        let revset = format!("({trunk})..@");
        let out = crate::jj::read_in_dir(
            dir,
            &[
                "log",
                "-r",
                &revset,
                "--no-graph",
                "-T",
                "local_bookmarks.map(|b| b.name()).join(\"\\n\") ++ \"\\n\"",
            ],
        )
        .map_err(|error| error.to_string())?;
        let mut seen = std::collections::HashSet::new();
        out.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(String::from)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .filter(|bookmark| seen.insert(bookmark.clone()))
            .map(|bookmark| {
                let description = crate::jj::read_in_dir(
                    dir,
                    &["log", "-r", &bookmark, "--no-graph", "-T", "description"],
                )
                .map_err(|error| error.to_string())?;
                Ok(Change {
                    bookmark,
                    description,
                })
            })
            .collect()
    }
}

impl SystemJj {
    fn config_value(&self, key: &str) -> Result<Option<String>, String> {
        let run = cmd("jj")
            .arg("--repository")
            .arg(&self.repo_root)
            .args(["config", "get", key])
            .run()
            .map_err(|error| error.to_string())?;
        Ok(run.stdout_ok().map(|value| value.trim().to_string()))
    }
}

struct SystemGpg;

impl GpgAdapter for SystemGpg {
    fn unlocked(&self, key: &str) -> Result<bool, String> {
        let mut child = Command::new("gpg")
            .args([
                "--batch",
                "--no-tty",
                "--pinentry-mode",
                "error",
                "--local-user",
                key,
                "--sign",
                "--output",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| error.to_string())?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(b"jjfx");
        }
        child
            .wait()
            .map(|status| status.success())
            .map_err(|error| error.to_string())
    }
}

struct SystemGitHub;

impl GitHubAdapter for SystemGitHub {
    fn default_branch(&self, slug: &str) -> Result<String, String> {
        let out = cmd("gh")
            .args([
                "repo",
                "view",
                slug,
                "--json",
                "defaultBranchRef",
                "--jq",
                ".defaultBranchRef.name",
            ])
            .run()
            .map_err(|error| error.to_string())?;
        if !out.ok() {
            return Err(format!("gh repo view ({slug}): {}", out.stderr().trim()));
        }
        let name = out.stdout().trim().to_string();
        if name.is_empty() {
            Err("gh reported an empty default branch".into())
        } else {
            Ok(name)
        }
    }

    fn find(&self, slug: &str, head: &str) -> Result<Option<PullRequest>, String> {
        crate::prs::find(slug, head).map(|pull_request| {
            pull_request.map(|pull_request| {
                let merged = pull_request.is_merged();
                PullRequest {
                    number: pull_request.number,
                    head: pull_request.head,
                    state: pull_request.state,
                    body: pull_request.body.unwrap_or_default(),
                    merged,
                }
            })
        })
    }

    fn create(&self, request: NewPullRequest) -> Result<PullRequest, String> {
        let mut args = vec![
            "pr",
            "create",
            "-R",
            &request.slug,
            "--head",
            &request.head,
            "--base",
            &request.base,
            "--title",
            &request.title,
            "--body",
            &request.body,
        ];
        if request.draft {
            args.push("--draft");
        }
        let out = cmd("gh")
            .args(args)
            .run()
            .map_err(|error| error.to_string())?;
        if !out.ok() {
            return Err(format!(
                "gh pr create ({}): {}",
                request.head,
                out.stderr().trim()
            ));
        }
        let number = out
            .stdout()
            .trim()
            .rsplit('/')
            .next()
            .and_then(|number| number.parse().ok())
            .ok_or_else(|| format!("gh pr create ({}): no PR url in output", request.head))?;
        Ok(PullRequest {
            number,
            head: request.head,
            state: "OPEN".to_string(),
            body: request.body,
            merged: false,
        })
    }

    fn edit(&self, slug: &str, number: u64, body: &str, base: &str) -> Result<(), String> {
        let number_arg = number.to_string();
        let out = cmd("gh")
            .args([
                "pr",
                "edit",
                &number_arg,
                "-R",
                slug,
                "--body",
                body,
                "--base",
                base,
            ])
            .run()
            .map_err(|error| error.to_string())?;
        if out.ok() {
            Ok(())
        } else {
            Err(format!("gh pr edit #{number}: {}", out.stderr().trim()))
        }
    }
}

struct PullRequestContext {
    slug: String,
    default_branch: String,
}

fn run_pipeline(
    tx: &UnboundedSender<Update>,
    targets: &[Target],
    config: ForgeConfig,
    jj: &dyn JjAdapter,
    gpg: &dyn GpgAdapter,
    github: &dyn GitHubAdapter,
) {
    let mut progress = vec![Progress::running(); targets.len()];
    for (target, state) in targets.iter().zip(&progress) {
        publish_progress(tx, target, state, None);
    }

    let signing_key = jj.signing_key().unwrap_or_default();
    if let Some(key) = signing_key
        && matches!(gpg.unlocked(&key), Ok(false))
    {
        let _ = tx.send(Update::Aborted {
            reason: "GPG signing key is locked - unlock it (run `gpg --sign`) then retry".into(),
        });
        return;
    }

    for (target, state) in targets.iter().zip(&mut progress) {
        state.set(Step::Fetch, Status::Running);
        publish_progress(tx, target, state, None);
    }
    let fetched = jj.fetch().unwrap_or_default();
    for (target, state) in targets.iter().zip(&mut progress) {
        if fetched {
            succeed_step(tx, target, state, Step::Fetch);
        } else {
            skip_step(tx, target, state, Step::Fetch, "fetch: fetch failed");
        }
    }
    if !fetched {
        let _ = tx.send(Update::Aborted {
            reason: "fetch failed".into(),
        });
        return;
    }

    let pull_request_context = if config.pull_requests {
        match jj.origin_slug() {
            Ok(Some(slug)) => {
                github
                    .default_branch(&slug)
                    .ok()
                    .map(|default_branch| PullRequestContext {
                        slug,
                        default_branch,
                    })
            }
            Ok(None) | Err(_) => None,
        }
    } else {
        None
    };

    for (target, state) in targets.iter().zip(&mut progress) {
        let has_conflict = jj.has_conflict(&target.dir).unwrap_or_default();
        if has_conflict {
            state.active = false;
            state.reason = Some("working copy has conflicts".into());
            publish_progress(tx, target, state, Some("working copy has conflicts".into()));
            continue;
        }

        state.set(Step::Weld, Status::Running);
        publish_progress(tx, target, state, None);
        let welded = jj.weld(&target.dir).unwrap_or_default();
        if welded {
            succeed_step(tx, target, state, Step::Weld);
        } else {
            skip_step(
                tx,
                target,
                state,
                Step::Weld,
                "weld: weld skipped (conflicts?)",
            );
        }

        state.set(Step::Push, Status::Running);
        publish_progress(tx, target, state, None);
        let pushed = jj.push(&target.dir).unwrap_or_default();
        if pushed {
            succeed_step(tx, target, state, Step::Push);
        } else {
            skip_step(tx, target, state, Step::Push, "push: nothing to push");
        }

        state.set(Step::PullRequest, Status::Running);
        publish_progress(tx, target, state, None);
        if config.pull_requests {
            let reason = match pull_request_context.as_ref() {
                None => Some("pr: no origin remote".to_string()),
                Some(context) => match jj.changes(&target.dir) {
                    Ok(changes) => {
                        match pull_requests::submit(context, changes, config.draft, github) {
                            pull_requests::Outcome::Did => None,
                            pull_requests::Outcome::Noop(reason)
                            | pull_requests::Outcome::Failed(reason) => {
                                Some(format!("pr: {reason}"))
                            }
                        }
                    }
                    Err(error) => Some(format!("pr: {error}")),
                },
            };
            if let Some(reason) = reason {
                skip_step(tx, target, state, Step::PullRequest, &reason);
            } else {
                succeed_step(tx, target, state, Step::PullRequest);
            }
        } else {
            succeed_step(tx, target, state, Step::PullRequest);
        }

        state.active = false;
        let retained = (!state.clean_success()).then(|| state.clone());
        let _ = tx.send(Update::Finished {
            workspace: target.name.clone(),
            progress: retained,
        });
    }
}

fn succeed_step(
    tx: &UnboundedSender<Update>,
    target: &Target,
    progress: &mut Progress,
    step: Step,
) {
    progress.set(step, Status::Ok);
    publish_progress(tx, target, progress, None);
}

fn skip_step(
    tx: &UnboundedSender<Update>,
    target: &Target,
    progress: &mut Progress,
    step: Step,
    reason: &str,
) {
    progress.set(step, Status::Skipped);
    progress.reason = Some(reason.to_string());
    publish_progress(tx, target, progress, Some(reason.to_string()));
}

fn publish_progress(
    tx: &UnboundedSender<Update>,
    target: &Target,
    progress: &Progress,
    notice: Option<String>,
) {
    let _ = tx.send(Update::Progress {
        workspace: target.name.clone(),
        progress: progress.clone(),
        notice,
    });
}

/// A push did real work only if it exited zero and did not report the jj no-op
/// marker on either captured stream.
fn pushed_something(ok: bool, stdout: &str, stderr: &str) -> bool {
    ok && !stdout.contains(NOTHING_CHANGED) && !stderr.contains(NOTHING_CHANGED)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    struct SuccessfulJj;

    impl JjAdapter for SuccessfulJj {
        fn fetch(&self) -> Result<bool, String> {
            Ok(true)
        }

        fn weld(&self, _dir: &Path) -> Result<bool, String> {
            Ok(true)
        }

        fn push(&self, _dir: &Path) -> Result<bool, String> {
            Ok(true)
        }
    }

    struct UnlockedGpg;

    impl GpgAdapter for UnlockedGpg {}

    struct FetchFails;

    impl JjAdapter for FetchFails {
        fn fetch(&self) -> Result<bool, String> {
            Ok(false)
        }

        fn weld(&self, _dir: &Path) -> Result<bool, String> {
            panic!("weld must not run after fetch failure")
        }

        fn push(&self, _dir: &Path) -> Result<bool, String> {
            panic!("push must not run after fetch failure")
        }
    }

    struct SigningJj;

    impl JjAdapter for SigningJj {
        fn signing_key(&self) -> Result<Option<String>, String> {
            Ok(Some("ABC123".to_string()))
        }

        fn fetch(&self) -> Result<bool, String> {
            panic!("fetch must not run while the signing key is locked")
        }

        fn weld(&self, _dir: &Path) -> Result<bool, String> {
            panic!("weld must not run while the signing key is locked")
        }

        fn push(&self, _dir: &Path) -> Result<bool, String> {
            panic!("push must not run while the signing key is locked")
        }
    }

    struct LockedGpg;

    impl GpgAdapter for LockedGpg {
        fn unlocked(&self, key: &str) -> Result<bool, String> {
            assert_eq!(key, "ABC123");
            Ok(false)
        }
    }

    struct FirstWorkspaceConflicted;

    impl JjAdapter for FirstWorkspaceConflicted {
        fn fetch(&self) -> Result<bool, String> {
            Ok(true)
        }

        fn has_conflict(&self, dir: &Path) -> Result<bool, String> {
            Ok(dir.ends_with("conflicted"))
        }

        fn weld(&self, dir: &Path) -> Result<bool, String> {
            assert!(dir.ends_with("clean"));
            Ok(true)
        }

        fn push(&self, dir: &Path) -> Result<bool, String> {
            assert!(dir.ends_with("clean"));
            Ok(true)
        }
    }

    struct PushDoesNothing;

    impl JjAdapter for PushDoesNothing {
        fn fetch(&self) -> Result<bool, String> {
            Ok(true)
        }

        fn weld(&self, _dir: &Path) -> Result<bool, String> {
            Ok(true)
        }

        fn push(&self, _dir: &Path) -> Result<bool, String> {
            Ok(false)
        }
    }

    struct WeldFails;

    impl JjAdapter for WeldFails {
        fn fetch(&self) -> Result<bool, String> {
            Ok(true)
        }

        fn weld(&self, _dir: &Path) -> Result<bool, String> {
            Ok(false)
        }

        fn push(&self, _dir: &Path) -> Result<bool, String> {
            Ok(true)
        }
    }

    struct NoBookmarksJj;

    impl JjAdapter for NoBookmarksJj {
        fn fetch(&self) -> Result<bool, String> {
            Ok(true)
        }

        fn weld(&self, _dir: &Path) -> Result<bool, String> {
            Ok(true)
        }

        fn push(&self, _dir: &Path) -> Result<bool, String> {
            Ok(true)
        }

        fn origin_slug(&self) -> Result<Option<String>, String> {
            Ok(Some("owner/repo".to_string()))
        }

        fn changes(&self, _dir: &Path) -> Result<Vec<Change>, String> {
            Ok(Vec::new())
        }
    }

    struct RepositoryGitHub;

    impl GitHubAdapter for RepositoryGitHub {
        fn default_branch(&self, slug: &str) -> Result<String, String> {
            assert_eq!(slug, "owner/repo");
            Ok("main".to_string())
        }

        fn find(&self, _slug: &str, _head: &str) -> Result<Option<PullRequest>, String> {
            panic!("an empty change chain must not query Pull Requests")
        }

        fn create(&self, _request: NewPullRequest) -> Result<PullRequest, String> {
            panic!("an empty change chain must not create Pull Requests")
        }

        fn edit(&self, _slug: &str, _number: u64, _body: &str, _base: &str) -> Result<(), String> {
            panic!("an empty change chain must not edit Pull Requests")
        }
    }

    struct StackedChangesJj;

    impl JjAdapter for StackedChangesJj {
        fn fetch(&self) -> Result<bool, String> {
            Ok(true)
        }

        fn weld(&self, _dir: &Path) -> Result<bool, String> {
            Ok(true)
        }

        fn push(&self, _dir: &Path) -> Result<bool, String> {
            Ok(true)
        }

        fn origin_slug(&self) -> Result<Option<String>, String> {
            Ok(Some("owner/repo".to_string()))
        }

        fn changes(&self, _dir: &Path) -> Result<Vec<Change>, String> {
            Ok(vec![
                Change {
                    bookmark: "base".to_string(),
                    description: "Base change\n\nBase body\n".to_string(),
                },
                Change {
                    bookmark: "mid".to_string(),
                    description: "Mid change\n\nMid body\n".to_string(),
                },
                Change {
                    bookmark: "top".to_string(),
                    description: "Top change\n\nTop body\n".to_string(),
                },
            ])
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RemotePullRequest {
        number: u64,
        head: String,
        base: String,
        title: String,
        body: String,
        merged: bool,
        draft: bool,
    }

    struct GitHubState {
        next_number: u64,
        pull_requests: Vec<RemotePullRequest>,
    }

    struct MemoryGitHub {
        state: Mutex<GitHubState>,
    }

    impl MemoryGitHub {
        fn with_merged_base() -> Self {
            Self {
                state: Mutex::new(GitHubState {
                    next_number: 11,
                    pull_requests: vec![RemotePullRequest {
                        number: 10,
                        head: "base".to_string(),
                        base: "main".to_string(),
                        title: "Base change".to_string(),
                        body: "Base body".to_string(),
                        merged: true,
                        draft: false,
                    }],
                }),
            }
        }

        fn snapshot(&self) -> Vec<RemotePullRequest> {
            self.state
                .lock()
                .expect("fake GitHub state is not poisoned")
                .pull_requests
                .clone()
        }

        fn with_merged_base_and_open_mid() -> Self {
            Self {
                state: Mutex::new(GitHubState {
                    next_number: 12,
                    pull_requests: vec![
                        RemotePullRequest {
                            number: 10,
                            head: "base".to_string(),
                            base: "main".to_string(),
                            title: "Base change".to_string(),
                            body: "Base body".to_string(),
                            merged: true,
                            draft: false,
                        },
                        RemotePullRequest {
                            number: 11,
                            head: "mid".to_string(),
                            base: "base".to_string(),
                            title: "Mid change".to_string(),
                            body: "Mid body\n\n## Stack\n\n- stale".to_string(),
                            merged: false,
                            draft: true,
                        },
                    ],
                }),
            }
        }
    }

    impl GitHubAdapter for MemoryGitHub {
        fn default_branch(&self, _slug: &str) -> Result<String, String> {
            Ok("main".to_string())
        }

        fn find(&self, _slug: &str, head: &str) -> Result<Option<PullRequest>, String> {
            Ok(self
                .state
                .lock()
                .expect("fake GitHub state is not poisoned")
                .pull_requests
                .iter()
                .find(|pull_request| pull_request.head == head)
                .map(|pull_request| PullRequest {
                    number: pull_request.number,
                    head: pull_request.head.clone(),
                    state: if pull_request.merged {
                        "MERGED".to_string()
                    } else {
                        "OPEN".to_string()
                    },
                    body: pull_request.body.clone(),
                    merged: pull_request.merged,
                }))
        }

        fn create(&self, request: NewPullRequest) -> Result<PullRequest, String> {
            let mut state = self
                .state
                .lock()
                .expect("fake GitHub state is not poisoned");
            let number = state.next_number;
            state.next_number += 1;
            state.pull_requests.push(RemotePullRequest {
                number,
                head: request.head.clone(),
                base: request.base,
                title: request.title,
                body: request.body.clone(),
                merged: false,
                draft: request.draft,
            });
            Ok(PullRequest {
                number,
                head: request.head,
                state: "OPEN".to_string(),
                body: request.body,
                merged: false,
            })
        }

        fn edit(&self, _slug: &str, number: u64, body: &str, base: &str) -> Result<(), String> {
            let mut state = self
                .state
                .lock()
                .expect("fake GitHub state is not poisoned");
            let pull_request = state
                .pull_requests
                .iter_mut()
                .find(|pull_request| pull_request.number == number)
                .expect("edited Pull Request exists");
            pull_request.body = body.to_string();
            pull_request.base = base.to_string();
            Ok(())
        }
    }

    async fn updates(mut rx: UnboundedReceiver<Update>) -> Vec<Update> {
        let mut updates = Vec::new();
        while let Some(update) = rx.recv().await {
            updates.push(update);
        }
        updates
    }

    fn observed(update: &Update) -> (String, [Status; 4], bool, Option<String>, &'static str) {
        match update {
            Update::Progress {
                workspace,
                progress,
                notice,
            } => (
                workspace.clone(),
                progress.steps().map(|(_, status)| status),
                progress.active(),
                notice.clone(),
                "progress",
            ),
            Update::Finished {
                workspace,
                progress,
            } => (
                workspace.clone(),
                progress
                    .as_ref()
                    .map(|progress| progress.steps().map(|(_, status)| status))
                    .unwrap_or([Status::Ok; 4]),
                false,
                None,
                "finished",
            ),
            Update::Aborted { reason } => (
                String::new(),
                [Status::Pending; 4],
                false,
                Some(reason.clone()),
                "aborted",
            ),
        }
    }

    fn status(progress: &Progress, expected: Step) -> Status {
        progress
            .steps()
            .into_iter()
            .find(|(step, _)| *step == expected)
            .map(|(_, status)| status)
            .expect("every Forge step has progress")
    }

    #[tokio::test]
    async fn one_workspace_reports_the_complete_pipeline_then_finishes_cleanly() {
        let forge = Forge::with_adapters(
            ForgeConfig {
                pull_requests: false,
                draft: true,
            },
            Arc::new(SuccessfulJj),
            Arc::new(UnlockedGpg),
        );

        let observed = updates(forge.start(vec![Target::new(
            "feat".to_string(),
            PathBuf::from("/workspace/feat"),
        )]))
        .await
        .iter()
        .map(observed)
        .collect::<Vec<_>>();

        assert_eq!(
            observed,
            vec![
                ("feat".into(), [Status::Pending; 4], true, None, "progress"),
                (
                    "feat".into(),
                    [
                        Status::Running,
                        Status::Pending,
                        Status::Pending,
                        Status::Pending,
                    ],
                    true,
                    None,
                    "progress",
                ),
                (
                    "feat".into(),
                    [
                        Status::Ok,
                        Status::Pending,
                        Status::Pending,
                        Status::Pending,
                    ],
                    true,
                    None,
                    "progress",
                ),
                (
                    "feat".into(),
                    [
                        Status::Ok,
                        Status::Running,
                        Status::Pending,
                        Status::Pending,
                    ],
                    true,
                    None,
                    "progress",
                ),
                (
                    "feat".into(),
                    [Status::Ok, Status::Ok, Status::Pending, Status::Pending],
                    true,
                    None,
                    "progress",
                ),
                (
                    "feat".into(),
                    [Status::Ok, Status::Ok, Status::Running, Status::Pending],
                    true,
                    None,
                    "progress",
                ),
                (
                    "feat".into(),
                    [Status::Ok, Status::Ok, Status::Ok, Status::Pending],
                    true,
                    None,
                    "progress",
                ),
                (
                    "feat".into(),
                    [Status::Ok, Status::Ok, Status::Ok, Status::Running],
                    true,
                    None,
                    "progress",
                ),
                ("feat".into(), [Status::Ok; 4], true, None, "progress"),
                ("feat".into(), [Status::Ok; 4], false, None, "finished"),
            ]
        );
    }

    #[tokio::test]
    async fn fetch_failure_aborts_before_workspace_work() {
        let forge = Forge::with_adapters(
            ForgeConfig {
                pull_requests: false,
                draft: true,
            },
            Arc::new(FetchFails),
            Arc::new(UnlockedGpg),
        );

        let updates = updates(forge.start(vec![Target::new(
            "feat".to_string(),
            PathBuf::from("/workspace/feat"),
        )]))
        .await;

        assert!(matches!(
            updates.last(),
            Some(Update::Aborted { reason }) if reason == "fetch failed"
        ));
        assert_eq!(
            updates
                .iter()
                .filter(|update| matches!(update, Update::Finished { .. }))
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn locked_signing_key_aborts_before_fetch() {
        let forge = Forge::with_adapters(
            ForgeConfig {
                pull_requests: false,
                draft: true,
            },
            Arc::new(SigningJj),
            Arc::new(LockedGpg),
        );

        let updates = updates(forge.start(vec![Target::new(
            "feat".to_string(),
            PathBuf::from("/workspace/feat"),
        )]))
        .await;

        assert!(matches!(
            updates.last(),
            Some(Update::Aborted { reason })
                if reason == "GPG signing key is locked - unlock it (run `gpg --sign`) then retry"
        ));
        assert_eq!(updates.len(), 2);
    }

    #[tokio::test]
    async fn conflicted_workspace_is_skipped_and_the_next_workspace_continues() {
        let forge = Forge::with_adapters(
            ForgeConfig {
                pull_requests: false,
                draft: true,
            },
            Arc::new(FirstWorkspaceConflicted),
            Arc::new(UnlockedGpg),
        );

        let updates = updates(forge.start(vec![
            Target::new(
                "conflicted".to_string(),
                PathBuf::from("/workspace/conflicted"),
            ),
            Target::new("clean".to_string(), PathBuf::from("/workspace/clean")),
        ]))
        .await;

        assert!(updates.iter().any(|update| matches!(
            update,
            Update::Progress {
                workspace,
                progress,
                notice: Some(notice),
            } if workspace == "conflicted"
                && !progress.active()
                && progress.reason() == Some("working copy has conflicts")
                && notice == "working copy has conflicts"
        )));
        assert!(!updates.iter().any(|update| matches!(
            update,
            Update::Finished { workspace, .. } if workspace == "conflicted"
        )));
        assert!(updates.iter().any(|update| matches!(
            update,
            Update::Finished {
                workspace,
                progress: None,
            } if workspace == "clean"
        )));
    }

    #[tokio::test]
    async fn push_noop_retains_skipped_progress_and_notice() {
        let forge = Forge::with_adapters(
            ForgeConfig {
                pull_requests: false,
                draft: true,
            },
            Arc::new(PushDoesNothing),
            Arc::new(UnlockedGpg),
        );

        let updates = updates(forge.start(vec![Target::new(
            "feat".to_string(),
            PathBuf::from("/workspace/feat"),
        )]))
        .await;

        assert!(updates.iter().any(|update| matches!(
            update,
            Update::Progress {
                workspace,
                progress,
                notice: Some(notice),
            } if workspace == "feat"
                && status(progress, Step::Push) == Status::Skipped
                && progress.reason() == Some("push: nothing to push")
                && notice == "push: nothing to push"
        )));
        assert!(matches!(
            updates.last(),
            Some(Update::Finished {
                workspace,
                progress: Some(progress),
            }) if workspace == "feat"
                && status(progress, Step::Push) == Status::Skipped
        ));
    }

    #[tokio::test]
    async fn weld_failure_is_retained_while_later_steps_continue() {
        let forge = Forge::with_adapters(
            ForgeConfig {
                pull_requests: false,
                draft: true,
            },
            Arc::new(WeldFails),
            Arc::new(UnlockedGpg),
        );

        let updates = updates(forge.start(vec![Target::new(
            "feat".to_string(),
            PathBuf::from("/workspace/feat"),
        )]))
        .await;

        assert!(updates.iter().any(|update| matches!(
            update,
            Update::Progress {
                progress,
                notice: Some(notice),
                ..
            } if status(progress, Step::Weld) == Status::Skipped
                && notice == "weld: weld skipped (conflicts?)"
        )));
        assert!(matches!(
            updates.last(),
            Some(Update::Finished {
                progress: Some(progress),
                ..
            }) if status(progress, Step::Weld) == Status::Skipped
                && status(progress, Step::Push) == Status::Ok
                && status(progress, Step::PullRequest) == Status::Ok
        ));
    }

    #[tokio::test]
    async fn pull_request_step_reports_an_empty_bookmark_chain() {
        let forge = Forge::with_github(
            ForgeConfig {
                pull_requests: true,
                draft: true,
            },
            Arc::new(NoBookmarksJj),
            Arc::new(UnlockedGpg),
            Arc::new(RepositoryGitHub),
        );

        let updates = updates(forge.start(vec![Target::new(
            "feat".to_string(),
            PathBuf::from("/workspace/feat"),
        )]))
        .await;

        assert!(updates.iter().any(|update| matches!(
            update,
            Update::Progress {
                progress,
                notice: Some(notice),
                ..
            } if status(progress, Step::PullRequest) == Status::Skipped
                && progress.reason() == Some("pr: no bookmark to open a PR")
                && notice == "pr: no bookmark to open a PR"
        )));
    }

    #[tokio::test]
    async fn pull_request_step_creates_and_links_the_stack() {
        let github = Arc::new(MemoryGitHub::with_merged_base());
        let forge = Forge::with_github(
            ForgeConfig {
                pull_requests: true,
                draft: true,
            },
            Arc::new(StackedChangesJj),
            Arc::new(UnlockedGpg),
            github.clone(),
        );

        let updates = updates(forge.start(vec![Target::new(
            "feat".to_string(),
            PathBuf::from("/workspace/feat"),
        )]))
        .await;

        assert!(matches!(
            updates.last(),
            Some(Update::Finished { progress: None, .. })
        ));
        assert_eq!(
            github.snapshot(),
            vec![
                RemotePullRequest {
                    number: 10,
                    head: "base".to_string(),
                    base: "main".to_string(),
                    title: "Base change".to_string(),
                    body: "Base body".to_string(),
                    merged: true,
                    draft: false,
                },
                RemotePullRequest {
                    number: 11,
                    head: "mid".to_string(),
                    base: "main".to_string(),
                    title: "Mid change".to_string(),
                    body: "Mid body\n\n## Stack\n\n- ~~#10~~\n- 👉🏻 #11\n- #12".to_string(),
                    merged: false,
                    draft: true,
                },
                RemotePullRequest {
                    number: 12,
                    head: "top".to_string(),
                    base: "mid".to_string(),
                    title: "Top change".to_string(),
                    body: "Top body\n\n## Stack\n\n- ~~#10~~\n- #11\n- 👉🏻 #12".to_string(),
                    merged: false,
                    draft: true,
                },
            ]
        );
    }

    #[tokio::test]
    async fn pull_request_step_updates_an_existing_open_stack_entry() {
        let github = Arc::new(MemoryGitHub::with_merged_base_and_open_mid());
        let forge = Forge::with_github(
            ForgeConfig {
                pull_requests: true,
                draft: true,
            },
            Arc::new(StackedChangesJj),
            Arc::new(UnlockedGpg),
            github.clone(),
        );

        let updates = updates(forge.start(vec![Target::new(
            "feat".to_string(),
            PathBuf::from("/workspace/feat"),
        )]))
        .await;
        let pull_requests = github.snapshot();

        assert!(matches!(
            updates.last(),
            Some(Update::Finished { progress: None, .. })
        ));
        assert_eq!(pull_requests[1].number, 11);
        assert_eq!(pull_requests[1].base, "main");
        assert_eq!(
            pull_requests[1].body,
            "Mid body\n\n## Stack\n\n- ~~#10~~\n- 👉🏻 #11\n- #12"
        );
        assert_eq!(pull_requests[2].number, 12);
        assert_eq!(pull_requests[2].base, "mid");
    }
}
