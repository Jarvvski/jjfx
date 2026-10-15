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
use crate::engine::{MutationGuard, MutationKind, Repository};
use crate::jj::{FixOutcome, PushOutcome};

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
/// Fix source: the same chain weld just rebased onto trunk. `jj fix -s` on its
/// root rewrites that root and every descendant through the user's configured
/// formatters, so what is pushed is already formatted.
const FIX_SRC: &str = WELD_SRC;
/// Push revisions: this workspace's chain, minus trunk and any conflicts.
const PUSH_REVS: &str = "::@ ~ trunk() ~ conflicts()";

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

    #[cfg(test)]
    pub(crate) fn finished_for_test(steps: [Status; 4], reason: &str) -> Self {
        Self {
            steps,
            active: false,
            reason: Some(reason.to_string()),
        }
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
    /// The conflicted commit on the workspace's chain
    /// (`(::@ ~ trunk()) & conflicts()`), or `None` when the chain is clean.
    ///
    /// The whole mutable chain, not just `@`: a weld can bury a conflict in a
    /// lower commit while `@` stays clean, and pushing that chain would push
    /// broken commits. The commit is returned so the refusal can name it - a
    /// conflicted chain is refused explicitly, never silently dropped from the
    /// push.
    fn conflict(&self, _dir: &Path) -> Result<Option<crate::work::ChangeId>, String> {
        Ok(None)
    }
    fn weld(&self, dir: &Path) -> Result<bool, String>;
    fn push(&self, dir: &Path) -> Result<PushOutcome, String>;

    /// Normalise the workspace's just-welded chain through the user's
    /// `[fix.tools]` jj config, before anything is pushed. A no-op when none are
    /// configured; a typed error when a formatter failed.
    fn fix(&self, _dir: &Path) -> Result<FixOutcome, String> {
        Ok(FixOutcome::NoTools)
    }

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
    /// The repository facade the undo records are written through, when the
    /// Forge is bound to a real repository (it is absent in unit tests).
    repository: Option<Repository>,
}

impl Forge {
    /// Bind Forge's production adapters to one repository and configuration.
    pub(crate) fn new(repo_root: PathBuf, config: ForgeConfig) -> Self {
        Self {
            config,
            jj: Arc::new(SystemJj {
                repo_root: repo_root.clone(),
            }),
            gpg: Arc::new(SystemGpg),
            github: Arc::new(SystemGitHub),
            repository: Repository::open(&repo_root).ok(),
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
            repository: None,
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
            repository: None,
        }
    }

    /// Start one run and return its ordered progress stream immediately.
    pub(crate) fn start(&self, targets: Vec<Target>) -> UnboundedReceiver<Update> {
        let (tx, rx) = mpsc::unbounded_channel();
        let config = self.config;
        let jj = Arc::clone(&self.jj);
        let gpg = Arc::clone(&self.gpg);
        let github = Arc::clone(&self.github);
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || {
            run_pipeline(
                &tx,
                &targets,
                config,
                &*jj,
                &*gpg,
                &*github,
                repository.as_ref(),
            )
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

    fn conflict(&self, dir: &Path) -> Result<Option<crate::work::ChangeId>, String> {
        // The whole mutable chain, not just `@`: a weld can bury a conflict in a
        // lower commit while `@` stays clean, and pushing that chain would push
        // broken commits.
        let run = cmd("jj")
            .current_dir(dir)
            .args([
                "--ignore-working-copy",
                "log",
                "-r",
                "(::@ ~ trunk()) & conflicts()",
                "--no-graph",
                "-T",
                "change_id ++ \"\\n\"",
            ])
            .run()
            .map_err(|error| error.to_string())?;
        if !run.ok() {
            return Err(format!("jj log (conflicts): {}", run.stderr().trim()));
        }
        // The deepest conflicted commit owns the conflict; every commit above it
        // merely inherits it, so that is the one to name and to resolve.
        Ok(run
            .stdout()
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .map(crate::work::ChangeId::short))
    }

    fn weld(&self, dir: &Path) -> Result<bool, String> {
        let run = cmd("jj")
            .current_dir(dir)
            .args(["rebase", "--skip-emptied", "-s", WELD_SRC, "-d", "trunk()"])
            .run()
            .map_err(|error| error.to_string())?;
        if !run.ok() {
            return Err(format!("jj rebase: {}", run.stderr().trim()));
        }
        Ok(true)
    }

    fn push(&self, dir: &Path) -> Result<PushOutcome, String> {
        crate::jj::push(dir, PUSH_REVS).map_err(|error| error.to_string())
    }

    fn fix(&self, dir: &Path) -> Result<FixOutcome, String> {
        crate::jj::fix(dir, FIX_SRC).map_err(|error| error.to_string())
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
                let revset = crate::jj::revset_symbol(&bookmark);
                let description = crate::jj::read_in_dir(
                    dir,
                    &["log", "-r", &revset, "--no-graph", "-T", "description"],
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
    repository: Option<&Repository>,
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

    // A failed `gh` or jj lookup must not read as "no origin remote": that is
    // how a broken Pull Request step used to look like a clean skip.
    let pull_request_context: Result<Option<PullRequestContext>, String> = if config.pull_requests {
        match jj.origin_slug() {
            Ok(Some(slug)) => github.default_branch(&slug).map(|default_branch| {
                Some(PullRequestContext {
                    slug,
                    default_branch,
                })
            }),
            Ok(None) => Ok(None),
            Err(error) => Err(error),
        }
    } else {
        Ok(None)
    };

    for (target, state) in targets.iter().zip(&mut progress) {
        // Record the operation the repository is at before this Workspace's
        // weld, so the whole forge of this Workspace can be undone from the tool.
        // The guard completes the record on every exit from the body below.
        let _undo = repository
            .map(|repository| MutationGuard::begin(repository, &target.name, MutationKind::Forge));
        if stop_on_conflicts(tx, target, state, jj.conflict(&target.dir)) {
            continue;
        }

        state.set(Step::Weld, Status::Running);
        publish_progress(tx, target, state, None);
        match jj.weld(&target.dir) {
            Ok(true) => succeed_step(tx, target, state, Step::Weld),
            Ok(false) => skip_step(
                tx,
                target,
                state,
                Step::Weld,
                "weld: weld skipped (conflicts?)",
            ),
            Err(error) => skip_step(tx, target, state, Step::Weld, &format!("weld: {error}")),
        }

        // A rebase that produced a conflict still exits zero, so re-check the
        // chain: pushing a conflicted chain would push broken commits and open a
        // Pull Request over them.
        if stop_on_conflicts(tx, target, state, jj.conflict(&target.dir)) {
            continue;
        }

        // Normalise the just-welded chain through the user's `jj fix` tools, so
        // what is pushed is already formatted. A formatter that failed is a
        // typed error and stops the push - never a silent pass of unformatted
        // commits.
        if let Err(error) = jj.fix(&target.dir) {
            skip_step(tx, target, state, Step::Weld, &format!("fix: {error}"));
            finish_target(tx, target, state);
            continue;
        }

        // `jj fix` rewrites commits, so it can bury a conflict the weld's check
        // could not see. Refuse it here too, naming the commit, rather than
        // letting the push revset's conflict exclusion quietly drop it.
        if stop_on_conflicts(tx, target, state, jj.conflict(&target.dir)) {
            continue;
        }

        state.set(Step::Push, Status::Running);
        publish_progress(tx, target, state, None);
        let push = jj.push(&target.dir).unwrap_or_else(PushOutcome::Failed);
        let push_failed = match push {
            PushOutcome::Pushed => {
                succeed_step(tx, target, state, Step::Push);
                false
            }
            PushOutcome::Nothing => {
                skip_step(tx, target, state, Step::Push, "push: nothing to push");
                false
            }
            PushOutcome::Failed(detail) => {
                let reason = if detail.is_empty() {
                    "push: push failed".to_string()
                } else {
                    format!("push: {detail}")
                };
                skip_step(tx, target, state, Step::Push, &reason);
                true
            }
        };
        if push_failed {
            // The remote was not updated; a Pull Request opened now would point
            // at stale commits.
            finish_target(tx, target, state);
            continue;
        }

        state.set(Step::PullRequest, Status::Running);
        publish_progress(tx, target, state, None);
        if config.pull_requests {
            let reason = match pull_request_context.as_ref() {
                Err(error) => Some(format!("pr: {error}")),
                Ok(None) => Some("pr: no origin remote".to_string()),
                Ok(Some(context)) => match jj.changes(&target.dir) {
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

/// Finish one target: mark it inactive and send the terminal `Finished` update,
/// retaining its progress when the run was not a clean success. Shared by every
/// path that ends a target (a completed pipeline, a failed push, a failed fix).
fn finish_target(tx: &UnboundedSender<Update>, target: &Target, state: &mut Progress) {
    state.active = false;
    let retained = (!state.clean_success()).then(|| state.clone());
    let _ = tx.send(Update::Finished {
        workspace: target.name.clone(),
        progress: retained,
    });
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

/// Stop one target before push when its chain has conflicts, or when the
/// conflict check itself failed. Pushing a conflicted chain would push broken
/// commits, and a failed check must not read as "clean". The refusal names the
/// conflicted commit, so the reason is the surfaced state rather than a silent
/// exclusion. Returns `true` when the target was stopped (the caller
/// `continue`s to the next target).
fn stop_on_conflicts(
    tx: &UnboundedSender<Update>,
    target: &Target,
    state: &mut Progress,
    check: Result<Option<crate::work::ChangeId>, String>,
) -> bool {
    let reason = match check {
        Ok(None) => return false,
        Ok(Some(commit)) => {
            format!("conflict in {commit} - resolve it before pushing")
        }
        Err(error) => format!("conflict check failed: {error}"),
    };
    state.active = false;
    state.reason = Some(reason.clone());
    publish_progress(tx, target, state, Some(reason));
    true
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

        fn push(&self, _dir: &Path) -> Result<PushOutcome, String> {
            Ok(PushOutcome::Pushed)
        }
    }

    /// A jj seam whose weld really mutates the repository, so the pipeline's undo
    /// record has something to point at.
    struct MutatingJj {
        root: PathBuf,
    }

    impl JjAdapter for MutatingJj {
        fn fetch(&self) -> Result<bool, String> {
            Ok(true)
        }

        fn weld(&self, _dir: &Path) -> Result<bool, String> {
            let output = crate::jj::command()
                .current_dir(&self.root)
                .args(["describe", "-r", "@-", "-m", "welded"])
                .output()
                .map_err(|error| error.to_string())?;
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
            }
            Ok(true)
        }

        fn push(&self, _dir: &Path) -> Result<PushOutcome, String> {
            Ok(PushOutcome::Pushed)
        }
    }

    #[test]
    fn a_pipeline_run_records_the_workspace_it_forged() {
        let root = crate::store::test_local_repo("forge-undo");
        std::fs::write(root.join("seed.txt"), "seed\n").expect("write seed file");
        let commit = crate::jj::command()
            .current_dir(&root)
            .args(["commit", "-m", "seed"])
            .output()
            .expect("jj should be installed");
        assert!(commit.status.success());
        let repository = crate::engine::Repository::open(&root).expect("repository");
        let config = ForgeConfig {
            pull_requests: false,
            draft: true,
        };
        let (tx, _rx) = mpsc::unbounded_channel();

        run_pipeline(
            &tx,
            &[Target::new("feat".to_owned(), root.clone())],
            config,
            &MutatingJj { root: root.clone() },
            &UnlockedGpg,
            &RepositoryGitHub,
            Some(&repository),
        );

        let record = repository
            .state_store()
            .undo()
            .mutation("feat")
            .expect("record")
            .expect("a forged Workspace is recorded for undo");
        assert_eq!(record.kind, crate::engine::MutationKind::Forge);
        assert!(matches!(
            repository.undo_mutation("feat", false).expect("undo"),
            crate::engine::UndoOutcome::Restored { .. }
        ));
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

        fn push(&self, _dir: &Path) -> Result<PushOutcome, String> {
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

        fn push(&self, _dir: &Path) -> Result<PushOutcome, String> {
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

        fn conflict(&self, dir: &Path) -> Result<Option<crate::work::ChangeId>, String> {
            Ok(dir
                .ends_with("conflicted")
                .then(|| crate::work::ChangeId::short("conflicted1")))
        }

        fn weld(&self, dir: &Path) -> Result<bool, String> {
            assert!(dir.ends_with("clean"));
            Ok(true)
        }

        fn push(&self, dir: &Path) -> Result<PushOutcome, String> {
            assert!(dir.ends_with("clean"));
            Ok(PushOutcome::Pushed)
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

        fn push(&self, _dir: &Path) -> Result<PushOutcome, String> {
            Ok(PushOutcome::Nothing)
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

        fn push(&self, _dir: &Path) -> Result<PushOutcome, String> {
            Ok(PushOutcome::Pushed)
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

        fn push(&self, _dir: &Path) -> Result<PushOutcome, String> {
            Ok(PushOutcome::Pushed)
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

        fn push(&self, _dir: &Path) -> Result<PushOutcome, String> {
            Ok(PushOutcome::Pushed)
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

    struct PushFails;

    impl JjAdapter for PushFails {
        fn fetch(&self) -> Result<bool, String> {
            Ok(true)
        }

        fn weld(&self, _dir: &Path) -> Result<bool, String> {
            Ok(true)
        }

        fn push(&self, _dir: &Path) -> Result<PushOutcome, String> {
            Ok(PushOutcome::Failed(
                "remote rejected the update".to_string(),
            ))
        }
    }

    struct WeldIntroducesConflict {
        welded: Mutex<bool>,
    }

    impl JjAdapter for WeldIntroducesConflict {
        fn fetch(&self) -> Result<bool, String> {
            Ok(true)
        }

        fn conflict(&self, _dir: &Path) -> Result<Option<crate::work::ChangeId>, String> {
            Ok((*self.welded.lock().expect("weld flag is not poisoned"))
                .then(|| crate::work::ChangeId::short("weldconflict")))
        }

        fn weld(&self, _dir: &Path) -> Result<bool, String> {
            *self.welded.lock().expect("weld flag is not poisoned") = true;
            Ok(true)
        }

        fn push(&self, _dir: &Path) -> Result<PushOutcome, String> {
            panic!("a chain that weld left conflicted must not be pushed")
        }
    }

    struct ConflictCheckFails;

    /// A jj seam whose `jj fix` buries a conflict the weld's own check could not
    /// see - the case the push revset's conflict exclusion used to hide.
    struct FixIntroducesConflict {
        fixed: Mutex<bool>,
    }

    impl JjAdapter for FixIntroducesConflict {
        fn fetch(&self) -> Result<bool, String> {
            Ok(true)
        }

        fn conflict(&self, _dir: &Path) -> Result<Option<crate::work::ChangeId>, String> {
            Ok((*self.fixed.lock().expect("fix flag is not poisoned"))
                .then(|| crate::work::ChangeId::short("fixconflict")))
        }

        fn weld(&self, _dir: &Path) -> Result<bool, String> {
            Ok(true)
        }

        fn fix(&self, _dir: &Path) -> Result<FixOutcome, String> {
            *self.fixed.lock().expect("fix flag is not poisoned") = true;
            Ok(FixOutcome::Fixed)
        }

        fn push(&self, _dir: &Path) -> Result<PushOutcome, String> {
            panic!("a chain that fix left conflicted must not be pushed")
        }
    }
    impl JjAdapter for ConflictCheckFails {
        fn fetch(&self) -> Result<bool, String> {
            Ok(true)
        }

        fn conflict(&self, _dir: &Path) -> Result<Option<crate::work::ChangeId>, String> {
            Err("jj log (conflicts): boom".to_string())
        }

        fn weld(&self, _dir: &Path) -> Result<bool, String> {
            panic!("weld must not run when the conflict check failed")
        }

        fn push(&self, _dir: &Path) -> Result<PushOutcome, String> {
            panic!("push must not run when the conflict check failed")
        }
    }

    struct WeldErrors;

    impl JjAdapter for WeldErrors {
        fn fetch(&self) -> Result<bool, String> {
            Ok(true)
        }

        fn weld(&self, _dir: &Path) -> Result<bool, String> {
            Err("jj rebase: boom".to_string())
        }

        fn push(&self, _dir: &Path) -> Result<PushOutcome, String> {
            Ok(PushOutcome::Pushed)
        }
    }

    /// Records the order of the mutating steps so a test can prove `fix` runs
    /// between `weld` and `push`.
    struct RecordingJj {
        calls: Mutex<Vec<&'static str>>,
    }

    impl RecordingJj {
        fn record(&self, call: &'static str) {
            self.calls
                .lock()
                .expect("the call log is not poisoned")
                .push(call);
        }
    }

    impl JjAdapter for RecordingJj {
        fn fetch(&self) -> Result<bool, String> {
            self.record("fetch");
            Ok(true)
        }

        fn weld(&self, _dir: &Path) -> Result<bool, String> {
            self.record("weld");
            Ok(true)
        }

        fn fix(&self, _dir: &Path) -> Result<FixOutcome, String> {
            self.record("fix");
            Ok(FixOutcome::Fixed)
        }

        fn push(&self, _dir: &Path) -> Result<PushOutcome, String> {
            self.record("push");
            Ok(PushOutcome::Pushed)
        }
    }

    struct FixFails;

    impl JjAdapter for FixFails {
        fn fetch(&self) -> Result<bool, String> {
            Ok(true)
        }

        fn weld(&self, _dir: &Path) -> Result<bool, String> {
            Ok(true)
        }

        fn fix(&self, _dir: &Path) -> Result<FixOutcome, String> {
            Err("formatter `sh` exited non-zero".to_string())
        }

        fn push(&self, _dir: &Path) -> Result<PushOutcome, String> {
            panic!("a failed formatter must stop the push")
        }
    }

    struct BrokenGitHub;
    impl GitHubAdapter for BrokenGitHub {
        fn default_branch(&self, _slug: &str) -> Result<String, String> {
            Err("gh: not authenticated".to_string())
        }

        fn find(&self, _slug: &str, _head: &str) -> Result<Option<PullRequest>, String> {
            panic!("a failed default-branch lookup must not query Pull Requests")
        }

        fn create(&self, _request: NewPullRequest) -> Result<PullRequest, String> {
            panic!("a failed default-branch lookup must not create Pull Requests")
        }

        fn edit(&self, _slug: &str, _number: u64, _body: &str, _base: &str) -> Result<(), String> {
            panic!("a failed default-branch lookup must not edit Pull Requests")
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
                && progress.reason() == Some("conflict in conflicted1 - resolve it before pushing")
                && notice == "conflict in conflicted1 - resolve it before pushing"
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
    async fn failed_push_is_reported_and_does_not_open_a_pull_request() {
        let forge = Forge::with_github(
            ForgeConfig {
                pull_requests: true,
                draft: true,
            },
            Arc::new(PushFails),
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
                workspace,
                progress,
                notice: Some(notice),
            } if workspace == "feat"
                && status(progress, Step::Push) == Status::Skipped
                && progress.reason() == Some("push: remote rejected the update")
                && notice == "push: remote rejected the update"
        )));
        assert!(matches!(
            updates.last(),
            Some(Update::Finished {
                workspace,
                progress: Some(progress),
            }) if workspace == "feat"
                && status(progress, Step::Push) == Status::Skipped
                && status(progress, Step::PullRequest) == Status::Pending
        ));
    }

    #[tokio::test]
    async fn conflict_introduced_by_weld_stops_the_pipeline_before_push() {
        let forge = Forge::with_adapters(
            ForgeConfig {
                pull_requests: false,
                draft: true,
            },
            Arc::new(WeldIntroducesConflict {
                welded: Mutex::new(false),
            }),
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
                && !progress.active()
                && progress.reason() == Some("conflict in weldconflict - resolve it before pushing")
                && notice == "conflict in weldconflict - resolve it before pushing"
        )));
        assert!(matches!(
            updates.last(),
            Some(Update::Progress {
                workspace,
                progress,
                notice: Some(notice),
            }) if workspace == "feat"
                && !progress.active()
                && status(progress, Step::Weld) == Status::Ok
                && status(progress, Step::Push) == Status::Pending
                && status(progress, Step::PullRequest) == Status::Pending
                && notice == "conflict in weldconflict - resolve it before pushing"
        ));
    }

    #[tokio::test]
    async fn conflict_introduced_by_fix_is_refused_explicitly_not_silently_excluded() {
        // `jj fix` rewrites commits, so it can bury a conflict the weld's check
        // could not see. The push revset excludes conflicts, so without this gate
        // the commit would vanish from the push silently; the refusal names it.
        let forge = Forge::with_adapters(
            ForgeConfig {
                pull_requests: false,
                draft: true,
            },
            Arc::new(FixIntroducesConflict {
                fixed: Mutex::new(false),
            }),
            Arc::new(UnlockedGpg),
        );

        let updates = updates(forge.start(vec![Target::new(
            "feat".to_string(),
            PathBuf::from("/workspace/feat"),
        )]))
        .await;

        assert!(matches!(
            updates.last(),
            Some(Update::Progress {
                workspace,
                progress,
                notice: Some(notice),
            }) if workspace == "feat"
                && !progress.active()
                && status(progress, Step::Weld) == Status::Ok
                && status(progress, Step::Push) == Status::Pending
                && status(progress, Step::PullRequest) == Status::Pending
                && notice == "conflict in fixconflict - resolve it before pushing"
        ));
    }

    #[tokio::test]
    async fn failed_pull_request_context_is_reported_not_silently_skipped() {
        let forge = Forge::with_github(
            ForgeConfig {
                pull_requests: true,
                draft: true,
            },
            Arc::new(StackedChangesJj),
            Arc::new(UnlockedGpg),
            Arc::new(BrokenGitHub),
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
                && status(progress, Step::PullRequest) == Status::Skipped
                && progress.reason() == Some("pr: gh: not authenticated")
                && notice == "pr: gh: not authenticated"
        )));
        assert!(matches!(
            updates.last(),
            Some(Update::Finished {
                workspace,
                progress: Some(progress),
            }) if workspace == "feat"
                && status(progress, Step::PullRequest) == Status::Skipped
        ));
    }

    #[tokio::test]
    async fn a_failed_conflict_check_is_reported_and_does_not_push() {
        // A broken conflict read must not read as "clean" and carry on to push.
        let forge = Forge::with_adapters(
            ForgeConfig {
                pull_requests: false,
                draft: true,
            },
            Arc::new(ConflictCheckFails),
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
                && !progress.active()
                && progress.reason() == Some("conflict check failed: jj log (conflicts): boom")
                && notice == "conflict check failed: jj log (conflicts): boom"
        )));
        assert!(!updates.iter().any(|update| matches!(
            update,
            Update::Finished { workspace, .. } if workspace == "feat"
        )));
    }

    #[tokio::test]
    async fn a_failed_weld_is_reported_not_silently_skipped() {
        // A jj rebase failure must surface its error, not read as a conflict skip.
        let forge = Forge::with_adapters(
            ForgeConfig {
                pull_requests: false,
                draft: true,
            },
            Arc::new(WeldErrors),
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
                && status(progress, Step::Weld) == Status::Skipped
                && progress.reason() == Some("weld: jj rebase: boom")
                && notice == "weld: jj rebase: boom"
        )));
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
    async fn fix_normalises_the_chain_between_weld_and_push() {
        // The just-welded chain must be normalised before it is pushed.
        let jj = Arc::new(RecordingJj {
            calls: Mutex::new(Vec::new()),
        });
        let forge = Forge::with_adapters(
            ForgeConfig {
                pull_requests: false,
                draft: true,
            },
            jj.clone(),
            Arc::new(UnlockedGpg),
        );

        updates(forge.start(vec![Target::new(
            "feat".to_string(),
            PathBuf::from("/workspace/feat"),
        )]))
        .await;

        assert_eq!(
            *jj.calls.lock().expect("the call log is not poisoned"),
            vec!["fetch", "weld", "fix", "push"]
        );
    }

    #[tokio::test]
    async fn a_failed_formatter_stops_the_push_and_surfaces_a_typed_error() {
        // A formatter that failed must not pass silently: the push stops and the
        // error is reported against the Weld step.
        let forge = Forge::with_adapters(
            ForgeConfig {
                pull_requests: false,
                draft: true,
            },
            Arc::new(FixFails),
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
                && status(progress, Step::Weld) == Status::Skipped
                && progress.reason() == Some("fix: formatter `sh` exited non-zero")
                && notice == "fix: formatter `sh` exited non-zero"
        )));
        assert!(matches!(
            updates.last(),
            Some(Update::Finished {
                workspace,
                progress: Some(progress),
            }) if workspace == "feat"
                && status(progress, Step::Weld) == Status::Skipped
                && status(progress, Step::Push) == Status::Pending
                && status(progress, Step::PullRequest) == Status::Pending
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
        assert!(matches!(
            updates.last(),
            Some(Update::Finished {
                workspace,
                progress: Some(progress),
            }) if workspace == "feat" && !progress.active()
        ));
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
