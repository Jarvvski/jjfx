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
use crate::config::{ChecksConfig, ForgeConfig};
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

/// The five forge steps, in pipeline order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Fetch the remote once for the whole run.
    Fetch,
    /// Rebase a Workspace's mutable chain onto trunk.
    Weld,
    /// Run the repo-owned build check against the just-welded tree, so it sees
    /// what trunk will actually be (ticket 18).
    Check,
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
            Step::Check => 2,
            Step::Push => 3,
            Step::PullRequest => 4,
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
    /// The step does not apply to this run: no build check is configured, so
    /// there is nothing to run. Counts as clean, and the row does not draw a
    /// step that never ran - an unconfigured repo forges exactly as it did
    /// before the check existed.
    NotApplicable,
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
    steps: [Status; 5],
    active: bool,
    reason: Option<String>,
}

impl Progress {
    fn running() -> Self {
        Self {
            steps: [Status::Pending; 5],
            active: true,
            reason: None,
        }
    }

    /// Whether the Workspace pipeline is still running.
    pub(crate) fn active(&self) -> bool {
        self.active
    }

    /// The five step statuses in display and execution order.
    pub(crate) fn steps(&self) -> [(Step, Status); 5] {
        [
            (Step::Fetch, self.steps[Step::Fetch.index()]),
            (Step::Weld, self.steps[Step::Weld.index()]),
            (Step::Check, self.steps[Step::Check.index()]),
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

    /// Whether the run finished cleanly. A step that does not apply (no build
    /// check configured) is clean: it is not a skip, and it must not taint an
    /// otherwise successful forge.
    fn clean_success(&self) -> bool {
        self.reason.is_none()
            && self
                .steps
                .iter()
                .all(|status| matches!(status, Status::Ok | Status::NotApplicable))
    }

    #[cfg(test)]
    pub(crate) fn finished_for_test(steps: [Status; 5], reason: &str) -> Self {
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
    /// Whether a Pull Request already exists for `head`, open or merged.
    fn exists(&self, slug: &str, head: &str) -> Result<bool, String>;
    fn create(&self, request: NewPullRequest) -> Result<(), String>;
}

/// What the forge pipeline needs from jjfx's config: how it opens Pull Requests,
/// and the repo-owned check it must pass before it pushes.
#[derive(Debug, Clone)]
pub(crate) struct Settings {
    /// `[forge]`: whether and how the pipeline opens Pull Requests.
    pub(crate) forge: ForgeConfig,
    /// `[checks]`: the build check that runs against the welded tree.
    pub(crate) checks: ChecksConfig,
}

/// The deep Forge module: one interface over execution, progress, and adapters.
pub(crate) struct Forge {
    settings: Settings,
    jj: Arc<dyn JjAdapter>,
    gpg: Arc<dyn GpgAdapter>,
    github: Arc<dyn GitHubAdapter>,
    /// The repository facade the undo records are written through, when the
    /// Forge is bound to a real repository (it is absent in unit tests).
    repository: Option<Repository>,
}

impl Forge {
    /// Bind Forge's production adapters to one repository and configuration.
    pub(crate) fn new(repo_root: PathBuf, settings: Settings) -> Self {
        Self {
            settings,
            jj: Arc::new(SystemJj {
                repo_root: repo_root.clone(),
            }),
            gpg: Arc::new(SystemGpg),
            github: Arc::new(SystemGitHub),
            repository: Repository::open(&repo_root).ok(),
        }
    }

    #[cfg(test)]
    fn with_adapters(settings: Settings, jj: Arc<dyn JjAdapter>, gpg: Arc<dyn GpgAdapter>) -> Self {
        Self {
            settings,
            jj,
            gpg,
            github: Arc::new(SystemGitHub),
            repository: None,
        }
    }

    #[cfg(test)]
    fn with_github(
        settings: Settings,
        jj: Arc<dyn JjAdapter>,
        gpg: Arc<dyn GpgAdapter>,
        github: Arc<dyn GitHubAdapter>,
    ) -> Self {
        Self {
            settings,
            jj,
            gpg,
            github,
            repository: None,
        }
    }

    /// Start one run and return its ordered progress stream immediately.
    pub(crate) fn start(&self, targets: Vec<Target>) -> UnboundedReceiver<Update> {
        let (tx, rx) = mpsc::unbounded_channel();
        let settings = self.settings.clone();
        let jj = Arc::clone(&self.jj);
        let gpg = Arc::clone(&self.gpg);
        let github = Arc::clone(&self.github);
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || {
            run_pipeline(
                &tx,
                &targets,
                settings,
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

    fn exists(&self, slug: &str, head: &str) -> Result<bool, String> {
        crate::prs::find(slug, head).map(|pull_request| pull_request.is_some())
    }

    fn create(&self, request: NewPullRequest) -> Result<(), String> {
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
        Ok(())
    }
}

struct PullRequestContext {
    slug: String,
    default_branch: String,
}

fn run_pipeline(
    tx: &UnboundedSender<Update>,
    targets: &[Target],
    settings: Settings,
    jj: &dyn JjAdapter,
    gpg: &dyn GpgAdapter,
    github: &dyn GitHubAdapter,
    repository: Option<&Repository>,
) {
    let config = settings.forge;
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

    // Whether this run has a forge at all. A repository with no forge - Pull
    // Requests switched off, or no origin remote to open one against - is a
    // supported path, not a degraded one: the Pull Request step does not apply
    // and does not taint an otherwise clean forge. A forge that *failed* is a
    // different fact, and stays a skip with a reason.
    let forge: Option<Result<&PullRequestContext, &str>> = match &pull_request_context {
        Ok(Some(context)) if config.pull_requests => Some(Ok(context)),
        Err(error) => Some(Err(error.as_str())),
        Ok(_) => None,
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

        // The repo-owned build check runs against the just-welded tree - what
        // trunk will actually be, which is why it belongs here and not in a
        // post-Run hook on the un-welded tree. An unconfigured repo has nothing
        // to run, so the step does not apply and does not taint the result. A
        // check that fails, or cannot run at all, stops the push: an unbuildable
        // change is not a change, and pushing it is what makes the pair
        // invisible.
        if settings.checks.command().is_none() {
            state.set(Step::Check, Status::NotApplicable);
            publish_progress(tx, target, state, None);
        } else {
            state.set(Step::Check, Status::Running);
            publish_progress(tx, target, state, None);
            match crate::checks::run(&target.dir, &settings.checks) {
                crate::checks::Outcome::Passed => succeed_step(tx, target, state, Step::Check),
                crate::checks::Outcome::NotConfigured => {
                    state.set(Step::Check, Status::NotApplicable);
                    publish_progress(tx, target, state, None);
                }
                outcome => {
                    let reason = outcome
                        .stop_reason()
                        .unwrap_or_else(|| "check: the build check did not pass".to_string());
                    skip_step(tx, target, state, Step::Check, &reason);
                    finish_target(tx, target, state);
                    continue;
                }
            }
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

        match &forge {
            // No forge: the step does not apply, and the run is still clean.
            None => {
                state.set(Step::PullRequest, Status::NotApplicable);
                publish_progress(tx, target, state, None);
            }
            Some(Ok(context)) => {
                state.set(Step::PullRequest, Status::Running);
                publish_progress(tx, target, state, None);
                let reason = match jj.changes(&target.dir) {
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
                };
                if let Some(reason) = reason {
                    skip_step(tx, target, state, Step::PullRequest, &reason);
                } else {
                    succeed_step(tx, target, state, Step::PullRequest);
                }
            }
            // A forge that could not be reached is a failure, not an absence.
            Some(Err(error)) => {
                state.set(Step::PullRequest, Status::Running);
                publish_progress(tx, target, state, None);
                skip_step(
                    tx,
                    target,
                    state,
                    Step::PullRequest,
                    &format!("pr: {error}"),
                );
            }
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

    /// The forge settings a test forges with: Pull Requests and draft as given,
    /// and no build check unless the test asks for one.
    fn settings(pull_requests: bool, draft: bool) -> Settings {
        Settings {
            forge: ForgeConfig {
                pull_requests,
                draft,
            },
            checks: ChecksConfig::default(),
        }
    }

    /// The forge settings with a build check configured, for the tests that
    /// exercise the Check step.
    fn settings_with_check(pull_requests: bool, command: &str) -> Settings {
        Settings {
            forge: ForgeConfig {
                pull_requests,
                draft: true,
            },
            checks: ChecksConfig {
                command: Some(command.to_owned()),
                ..ChecksConfig::default()
            },
        }
    }

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
        let config = settings(false, true);
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

    /// A jj whose repository has no `origin` remote, so there is no forge to
    /// open Pull Requests against.
    struct NoOriginJj;

    impl JjAdapter for NoOriginJj {
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
            Ok(None)
        }
    }

    struct RepositoryGitHub;

    impl GitHubAdapter for RepositoryGitHub {
        fn default_branch(&self, slug: &str) -> Result<String, String> {
            assert_eq!(slug, "owner/repo");
            Ok("main".to_string())
        }

        fn exists(&self, _slug: &str, _head: &str) -> Result<bool, String> {
            panic!("an empty change chain must not query Pull Requests")
        }

        fn create(&self, _request: NewPullRequest) -> Result<(), String> {
            panic!("an empty change chain must not create Pull Requests")
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

        fn exists(&self, _slug: &str, _head: &str) -> Result<bool, String> {
            panic!("a failed default-branch lookup must not query Pull Requests")
        }

        fn create(&self, _request: NewPullRequest) -> Result<(), String> {
            panic!("a failed default-branch lookup must not create Pull Requests")
        }
    }

    /// A created Pull Request as the fake forge recorded it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct CreatedPullRequest {
        head: String,
        base: String,
        title: String,
        body: String,
        draft: bool,
    }

    /// A forge that records what was opened and answers `exists` from that log.
    #[derive(Default)]
    struct MemoryGitHub {
        created: Mutex<Vec<CreatedPullRequest>>,
    }

    impl MemoryGitHub {
        fn snapshot(&self) -> Vec<CreatedPullRequest> {
            self.created
                .lock()
                .expect("fake GitHub state is not poisoned")
                .clone()
        }
    }

    impl GitHubAdapter for MemoryGitHub {
        fn default_branch(&self, _slug: &str) -> Result<String, String> {
            Ok("main".to_string())
        }

        fn exists(&self, _slug: &str, head: &str) -> Result<bool, String> {
            Ok(self
                .created
                .lock()
                .expect("fake GitHub state is not poisoned")
                .iter()
                .any(|pull_request| pull_request.head == head))
        }

        fn create(&self, request: NewPullRequest) -> Result<(), String> {
            self.created
                .lock()
                .expect("fake GitHub state is not poisoned")
                .push(CreatedPullRequest {
                    head: request.head,
                    base: request.base,
                    title: request.title,
                    body: request.body,
                    draft: request.draft,
                });
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

    fn observed(update: &Update) -> (String, [Status; 5], bool, Option<String>, &'static str) {
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
                    .unwrap_or([Status::Ok; 5]),
                false,
                None,
                "finished",
            ),
            Update::Aborted { reason } => (
                String::new(),
                [Status::Pending; 5],
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
            settings(false, true),
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
                ("feat".into(), [Status::Pending; 5], true, None, "progress"),
                (
                    "feat".into(),
                    [
                        Status::Running,
                        Status::Pending,
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
                        Status::Ok,
                        Status::Pending,
                        Status::Pending,
                        Status::Pending,
                    ],
                    true,
                    None,
                    "progress",
                ),
                // No build check configured: the step does not apply, and the
                // pipeline carries straight on to the push.
                (
                    "feat".into(),
                    [
                        Status::Ok,
                        Status::Ok,
                        Status::NotApplicable,
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
                        Status::Ok,
                        Status::NotApplicable,
                        Status::Running,
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
                        Status::Ok,
                        Status::NotApplicable,
                        Status::Ok,
                        Status::Pending,
                    ],
                    true,
                    None,
                    "progress",
                ),
                // No forge configured: the Pull Request step does not apply, and
                // the run is still a clean success.
                (
                    "feat".into(),
                    [
                        Status::Ok,
                        Status::Ok,
                        Status::NotApplicable,
                        Status::Ok,
                        Status::NotApplicable,
                    ],
                    true,
                    None,
                    "progress",
                ),
                ("feat".into(), [Status::Ok; 5], false, None, "finished",),
            ]
        );
    }

    #[tokio::test]
    async fn fetch_failure_aborts_before_workspace_work() {
        let forge = Forge::with_adapters(
            settings(false, true),
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
            settings(false, true),
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
            settings(false, true),
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
            settings(false, true),
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
            settings(true, true),
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
            settings(false, true),
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
            settings(false, true),
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
            settings(true, true),
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
            settings(false, true),
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
            settings(false, true),
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
            settings(false, true),
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
                && status(progress, Step::PullRequest) == Status::NotApplicable
        ));
    }

    #[tokio::test]
    async fn fix_normalises_the_chain_between_weld_and_push() {
        // The just-welded chain must be normalised before it is pushed.
        let jj = Arc::new(RecordingJj {
            calls: Mutex::new(Vec::new()),
        });
        let forge = Forge::with_adapters(settings(false, true), jj.clone(), Arc::new(UnlockedGpg));

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
            settings(false, true),
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

    /// A jj whose weld materialises trunk into the workspace, as a real rebase
    /// does, so a check can prove it ran against the welded tree.
    struct WeldingJj {
        calls: Mutex<Vec<String>>,
    }

    impl JjAdapter for WeldingJj {
        fn fetch(&self) -> Result<bool, String> {
            self.record("fetch");
            Ok(true)
        }

        fn weld(&self, dir: &Path) -> Result<bool, String> {
            self.record("weld");
            std::fs::write(dir.join("welded"), "").map_err(|error| error.to_string())?;
            Ok(true)
        }

        fn push(&self, _dir: &Path) -> Result<PushOutcome, String> {
            self.record("push");
            Ok(PushOutcome::Pushed)
        }
    }

    impl WeldingJj {
        fn record(&self, call: &str) {
            self.calls
                .lock()
                .expect("the call log is not poisoned")
                .push(call.to_string());
        }
    }

    /// A jj whose push must never run: the build check in front of it did not
    /// pass.
    struct CheckMustStopThePush;

    impl JjAdapter for CheckMustStopThePush {
        fn fetch(&self) -> Result<bool, String> {
            Ok(true)
        }

        fn weld(&self, _dir: &Path) -> Result<bool, String> {
            Ok(true)
        }

        fn push(&self, _dir: &Path) -> Result<PushOutcome, String> {
            panic!("a build check that did not pass must stop the push")
        }
    }

    /// A throwaway workspace directory for the tests that run a real build check.
    fn workspace_dir(tag: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the epoch")
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("jjfx-forge-{tag}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("creating the workspace dir");
        dir
    }

    #[tokio::test]
    async fn the_build_check_runs_against_the_welded_tree_between_weld_and_push() {
        // The check looks for the file Weld materialises, so it can only pass if
        // it runs *after* the weld - which is the whole point of running it here
        // rather than on the un-welded tree.
        let dir = workspace_dir("check-order");
        let jj = Arc::new(WeldingJj {
            calls: Mutex::new(Vec::new()),
        });
        let forge = Forge::with_adapters(
            settings_with_check(false, "test -f welded && pwd > checked-cwd"),
            jj.clone(),
            Arc::new(UnlockedGpg),
        );

        let updates =
            updates(forge.start(vec![Target::new("feat".to_string(), dir.clone())])).await;

        // The check passed (so the weld had happened), and the push followed it.
        assert_eq!(
            *jj.calls.lock().expect("the call log is not poisoned"),
            vec!["fetch", "weld", "push"]
        );
        assert!(updates.iter().any(|update| matches!(
            update,
            Update::Progress {
                workspace,
                progress,
                ..
            } if workspace == "feat" && status(progress, Step::Check) == Status::Ok
        )));

        // It ran in the workspace's own working copy.
        let checked = std::fs::read_to_string(dir.join("checked-cwd"))
            .expect("the check records the directory it ran in");
        assert_eq!(
            std::fs::canonicalize(checked.trim()).expect("the recorded directory exists"),
            std::fs::canonicalize(&dir).expect("the workspace directory exists")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_change_that_does_not_build_stops_the_push_and_names_the_command() {
        let dir = workspace_dir("check-fails");
        let forge = Forge::with_adapters(
            settings_with_check(false, "echo 'error: cannot find value x' >&2; exit 1"),
            Arc::new(CheckMustStopThePush),
            Arc::new(UnlockedGpg),
        );

        let updates =
            updates(forge.start(vec![Target::new("feat".to_string(), dir.clone())])).await;

        assert!(updates.iter().any(|update| matches!(
            update,
            Update::Progress {
                workspace,
                progress,
                notice: Some(notice),
            } if workspace == "feat"
                && status(progress, Step::Check) == Status::Skipped
                && notice.contains("check:")
                && notice.contains("exit 1")
                && notice.contains("error: cannot find value x")
        )));
        assert!(matches!(
            updates.last(),
            Some(Update::Finished {
                workspace,
                progress: Some(progress),
            }) if workspace == "feat"
                && status(progress, Step::Check) == Status::Skipped
                && status(progress, Step::Push) == Status::Pending
                && status(progress, Step::PullRequest) == Status::Pending
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_check_that_cannot_run_stops_the_push() {
        let dir = workspace_dir("check-missing");
        let forge = Forge::with_adapters(
            settings_with_check(false, "jjfx-no-such-program-exists"),
            Arc::new(CheckMustStopThePush),
            Arc::new(UnlockedGpg),
        );

        let updates =
            updates(forge.start(vec![Target::new("feat".to_string(), dir.clone())])).await;

        assert!(updates.iter().any(|update| matches!(
            update,
            Update::Progress {
                workspace,
                progress,
                notice: Some(notice),
            } if workspace == "feat"
                && status(progress, Step::Check) == Status::Skipped
                && notice.contains("could not run")
        )));
        assert!(matches!(
            updates.last(),
            Some(Update::Finished {
                progress: Some(progress),
                ..
            }) if status(progress, Step::Push) == Status::Pending
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn an_unconfigured_repo_does_not_run_a_check_and_still_pushes() {
        let jj = Arc::new(RecordingJj {
            calls: Mutex::new(Vec::new()),
        });
        let forge = Forge::with_adapters(settings(false, true), jj.clone(), Arc::new(UnlockedGpg));

        let updates = updates(forge.start(vec![Target::new(
            "feat".to_string(),
            PathBuf::from("/workspace/feat"),
        )]))
        .await;

        // Nothing was run, and the push is not gated on anything.
        assert_eq!(
            *jj.calls.lock().expect("the call log is not poisoned"),
            vec!["fetch", "weld", "fix", "push"]
        );
        assert!(updates.iter().any(|update| matches!(
            update,
            Update::Progress { progress, .. }
                if status(progress, Step::Check) == Status::NotApplicable
        )));
    }

    #[tokio::test]
    async fn pull_request_step_reports_an_empty_bookmark_chain() {
        let forge = Forge::with_github(
            settings(true, true),
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
    async fn a_repository_with_no_forge_forges_cleanly() {
        // A repository that does not open Pull Requests is a supported path, not
        // a degraded one: the Pull Request step does not apply, and the run is a
        // clean success rather than a skip that retains a reason.
        let forge = Forge::with_adapters(
            settings(false, true),
            Arc::new(SuccessfulJj),
            Arc::new(UnlockedGpg),
        );

        let updates = updates(forge.start(vec![Target::new(
            "feat".to_string(),
            PathBuf::from("/workspace/feat"),
        )]))
        .await;

        assert!(updates.iter().any(|update| matches!(
            update,
            Update::Progress { progress, .. }
                if status(progress, Step::PullRequest) == Status::NotApplicable
        )));
        assert!(matches!(
            updates.last(),
            Some(Update::Finished { progress: None, .. })
        ));
    }

    #[tokio::test]
    async fn a_repository_with_no_origin_remote_forges_cleanly() {
        // Pull Requests are on, but there is no origin remote to open one
        // against: still a supported path, not a skip.
        let forge = Forge::with_github(
            settings(true, true),
            Arc::new(NoOriginJj),
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
            Update::Progress { progress, .. }
                if status(progress, Step::PullRequest) == Status::NotApplicable
        )));
        assert!(matches!(
            updates.last(),
            Some(Update::Finished { progress: None, .. })
        ));
    }

    #[tokio::test]
    async fn pull_request_step_opens_one_flat_pr_per_change() {
        let github = Arc::new(MemoryGitHub::default());
        let forge = Forge::with_github(
            settings(true, true),
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
        // One Pull Request per change, each based on the default branch - no
        // base-chained stack, and no `## Stack` section written into any body.
        assert_eq!(
            github.snapshot(),
            vec![
                CreatedPullRequest {
                    head: "base".to_string(),
                    base: "main".to_string(),
                    title: "Base change".to_string(),
                    body: "Base body".to_string(),
                    draft: true,
                },
                CreatedPullRequest {
                    head: "mid".to_string(),
                    base: "main".to_string(),
                    title: "Mid change".to_string(),
                    body: "Mid body".to_string(),
                    draft: true,
                },
                CreatedPullRequest {
                    head: "top".to_string(),
                    base: "main".to_string(),
                    title: "Top change".to_string(),
                    body: "Top body".to_string(),
                    draft: true,
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_change_that_already_has_a_pull_request_is_left_alone() {
        // A re-forge must not duplicate a Pull Request or rewrite its body.
        let github = Arc::new(MemoryGitHub::default());
        github
            .created
            .lock()
            .expect("fake GitHub state is not poisoned")
            .push(CreatedPullRequest {
                head: "base".to_string(),
                base: "main".to_string(),
                title: "Base change".to_string(),
                body: "human-edited body".to_string(),
                draft: false,
            });
        let forge = Forge::with_github(
            settings(true, true),
            Arc::new(StackedChangesJj),
            Arc::new(UnlockedGpg),
            github.clone(),
        );

        updates(forge.start(vec![Target::new(
            "feat".to_string(),
            PathBuf::from("/workspace/feat"),
        )]))
        .await;

        let pull_requests = github.snapshot();
        assert_eq!(pull_requests.len(), 3);
        assert_eq!(pull_requests[0].body, "human-edited body");
    }
}
