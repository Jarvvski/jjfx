//! The forge pipeline (ADR 0005, ticket 08): fetch -> weld -> push -> pr, run
//! natively with the workspace-safe revsets ported from `jj-forge`, modeled as
//! real step state that streams to the TUI rather than scraped stdout. The final
//! step opens/updates pull requests over `gh` (see [`crate::pr`]); the whole
//! pipeline depends only on `jj` and `gh`, never a third-party CLI.
//!
//! Each mutating step runs with its current directory set to the workspace's own
//! path, so `@` resolves to *that* workspace's working copy - forging one
//! workspace never rebases another's chain. Steps shell out via `spawn_blocking`
//! (jj/gh calls block), and each transition is sent to the single owned `App` as
//! a [`Msg::Forge`].

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tokio::sync::mpsc::UnboundedSender;

use crate::app::Msg;
use crate::cmd::{Run, cmd};
use crate::config::ForgeConfig;
use crate::pr;

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
pub enum Step {
    Fetch,
    Weld,
    Push,
    Pr,
}

impl Step {
    /// Position in the four-slot progress array.
    pub fn index(self) -> usize {
        match self {
            Step::Fetch => 0,
            Step::Weld => 1,
            Step::Push => 2,
            Step::Pr => 3,
        }
    }

    /// Single-letter tag for the compact row pipeline.
    pub fn letter(self) -> char {
        match self {
            Step::Fetch => 'f',
            Step::Weld => 'w',
            Step::Push => 'p',
            Step::Pr => 'r',
        }
    }

    fn name(self) -> &'static str {
        match self {
            Step::Fetch => "fetch",
            Step::Weld => "weld",
            Step::Push => "push",
            Step::Pr => "pr",
        }
    }
}

/// Live status of one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    Ok,
    /// The step did nothing useful (nothing to push, weld hit conflicts, ...).
    Skipped,
}

/// A forge progress update for one workspace (or the whole run), streamed to the
/// app and folded into its per-workspace forge state.
#[derive(Debug, Clone)]
pub enum Update {
    /// Begin forging these workspaces - reset their progress to all-pending.
    Start(Vec<String>),
    /// A step changed state for a workspace; `reason` is set on a non-`Ok` result.
    Step {
        ws: String,
        step: Step,
        status: Status,
        reason: Option<String>,
    },
    /// The whole workspace was skipped before its pipeline ran (conflicts).
    Skip { ws: String, reason: String },
    /// The workspace's pipeline finished.
    Done { ws: String },
    /// The whole run was aborted before per-workspace work (GPG locked, fetch).
    Aborted(String),
}

/// A workspace to forge: its name and the directory whose `@` the revsets target.
#[derive(Debug, Clone)]
pub struct Target {
    pub name: String,
    pub dir: PathBuf,
}

/// Run the forge pipeline for `targets`: a shared `fetch`, then `weld -> push ->
/// spr` per workspace. Sends each transition as a [`Msg::Forge`]; a send failure
/// (the app has quit) simply ends the run.
pub async fn run(
    tx: UnboundedSender<Msg>,
    repo_root: PathBuf,
    targets: Vec<Target>,
    cfg: ForgeConfig,
) {
    let names: Vec<String> = targets.iter().map(|t| t.name.clone()).collect();
    send(&tx, Update::Start(names.clone()));

    // GPG guard: signing happens on weld (rewrite) and push (sign-on-push), so a
    // locked key would trigger a pinentry prompt inside the alt-screen. Detect it
    // up front and abort cleanly rather than corrupt the terminal.
    if let Some(key) = signing_key(&repo_root).await
        && !gpg_unlocked(key).await
    {
        send(
            &tx,
            Update::Aborted(
                "GPG signing key is locked - unlock it (run `gpg --sign`) then retry".into(),
            ),
        );
        return;
    }

    // fetch once, shared by every target.
    for name in &names {
        send(&tx, step_running(name, Step::Fetch));
    }
    let fetched = jj_at(&repo_root, &["git", "fetch"]).await;
    for name in &names {
        send(&tx, step_result(name, Step::Fetch, fetched, "fetch failed"));
    }
    if !fetched {
        send(&tx, Update::Aborted("fetch failed".into()));
        return;
    }

    // Resolve the repo-wide PR facts (slug + default branch) once, shared by every
    // target. `None` when PR management is off or there is no origin remote.
    let pr_ctx = if cfg.pull_requests {
        pr::context(repo_root.clone(), cfg.draft).await
    } else {
        None
    };

    for t in &targets {
        forge_one(&tx, t, cfg, pr_ctx.as_ref()).await;
    }
}

/// Weld -> push -> spr for one workspace, skipping the whole pipeline if its
/// working copy is conflicted. Each step is best-effort: a failure is reported as
/// `Skipped` (mirroring `jj-forge`) and the pipeline continues.
async fn forge_one(
    tx: &UnboundedSender<Msg>,
    t: &Target,
    cfg: ForgeConfig,
    pr_ctx: Option<&pr::Context>,
) {
    if has_conflict(&t.dir).await {
        send(
            tx,
            Update::Skip {
                ws: t.name.clone(),
                reason: "working copy has conflicts".into(),
            },
        );
        return;
    }

    send(tx, step_running(&t.name, Step::Weld));
    let welded = jj_in(
        &t.dir,
        &["rebase", "--skip-emptied", "-s", WELD_SRC, "-d", "trunk()"],
    )
    .await;
    send(
        tx,
        step_result(&t.name, Step::Weld, welded, "weld skipped (conflicts?)"),
    );

    send(tx, step_running(&t.name, Step::Push));
    let pushed = did_push(&t.dir).await;
    send(
        tx,
        step_result(&t.name, Step::Push, pushed, "nothing to push"),
    );

    send(tx, step_running(&t.name, Step::Pr));
    let (ok, reason) = pr_step(cfg, pr_ctx, &t.dir).await;
    send(
        tx,
        Update::Step {
            ws: t.name.clone(),
            step: Step::Pr,
            status: if ok { Status::Ok } else { Status::Skipped },
            reason,
        },
    );

    send(tx, Update::Done { ws: t.name.clone() });
}

/// Run the native PR step, returning `(did_real_work, footer_reason)`. PR
/// management disabled by config is a no-op success (nothing to report); a
/// missing context (no origin remote) or a `gh`/`jj` failure is a `Skipped` with
/// a reason so the row stays honest instead of claiming "forged".
async fn pr_step(
    cfg: ForgeConfig,
    pr_ctx: Option<&pr::Context>,
    dir: &Path,
) -> (bool, Option<String>) {
    if !cfg.pull_requests {
        return (true, None);
    }
    let Some(ctx) = pr_ctx else {
        return (false, Some("pr: no origin remote".into()));
    };
    match pr::submit(ctx.clone(), dir.to_path_buf()).await {
        pr::Outcome::Did => (true, None),
        pr::Outcome::Noop(r) | pr::Outcome::Failed(r) => (false, Some(format!("pr: {r}"))),
    }
}

fn send(tx: &UnboundedSender<Msg>, update: Update) {
    let _ = tx.send(Msg::Forge(update));
}

fn step_running(ws: &str, step: Step) -> Update {
    Update::Step {
        ws: ws.to_string(),
        step,
        status: Status::Running,
        reason: None,
    }
}

/// Build a step outcome: `Ok` on success, else `Skipped` carrying a short hint.
fn step_result(ws: &str, step: Step, ok: bool, skip_hint: &str) -> Update {
    Update::Step {
        ws: ws.to_string(),
        step,
        status: if ok { Status::Ok } else { Status::Skipped },
        reason: if ok {
            None
        } else {
            Some(format!("{}: {skip_hint}", step.name()))
        },
    }
}

/// The GPG signing key jj would use, or `None` when signing is off / not GPG (so
/// the guard is a no-op). Read from jj config so it is never hard-coded.
async fn signing_key(repo_root: &Path) -> Option<String> {
    if config_get(repo_root, "signing.backend").await.as_deref() != Some("gpg") {
        return None;
    }
    let key = config_get(repo_root, "signing.key").await?;
    (!key.is_empty()).then_some(key)
}

/// Read a single jj config value (trimmed), `None` if unset or on any failure.
async fn config_get(repo_root: &Path, key: &str) -> Option<String> {
    let root = repo_root.to_path_buf();
    let key = key.to_string();
    tokio::task::spawn_blocking(move || {
        cmd("jj")
            .arg("--repository")
            .arg(&root)
            .args(["config", "get", &key])
            .run()
            .ok()
            .and_then(Run::stdout_ok)
            .map(|s| s.trim().to_string())
    })
    .await
    .ok()
    .flatten()
}

/// Whether the GPG key can sign non-interactively. `--pinentry-mode error` makes
/// gpg fail immediately instead of prompting when the passphrase is not cached,
/// so a `false` means "locked". Any environmental problem (gpg missing, spawn
/// failure) degrades to `true` - we do not block a forge on a shaky probe; the
/// push would surface its own error.
async fn gpg_unlocked(key: String) -> bool {
    tokio::task::spawn_blocking(move || {
        let child = Command::new("gpg")
            .args([
                "--batch",
                "--no-tty",
                "--pinentry-mode",
                "error",
                "--local-user",
                &key,
                "--sign",
                "--output",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(_) => return true, // no gpg on PATH: don't block the forge
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(b"jjfx");
        }
        child.wait().map(|s| s.success()).unwrap_or(true)
    })
    .await
    .unwrap_or(true)
}

/// Is the workspace's working copy conflicted? A pure read (`--ignore-working-copy`,
/// no snapshot) run in the workspace dir so `@` is that workspace's copy.
async fn has_conflict(dir: &Path) -> bool {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        cmd("jj")
            .current_dir(&dir)
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
            .map(|r| r.ok() && !r.stdout().is_empty())
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

/// Run a jj command against the repo (fetch is repo-global). Success only.
async fn jj_at(repo_root: &Path, args: &[&str]) -> bool {
    let root = repo_root.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || {
        cmd("jj")
            .arg("--repository")
            .arg(&root)
            .args(&args)
            .run()
            .map(|r| r.ok())
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

/// Run a jj command in the workspace's directory (so `@` is that workspace's
/// working copy, and jj snapshots it first - matching `jj-forge`). Success only.
async fn jj_in(dir: &Path, args: &[&str]) -> bool {
    jj_in_run(dir, args).await.map(|r| r.ok()).unwrap_or(false)
}

/// Like [`jj_in`], but hands back the captured [`Run`] so a caller can tell a
/// no-op from real work (jj exits 0 either way). `None` if it could not spawn.
async fn jj_in_run(dir: &Path, args: &[&str]) -> Option<Run> {
    let dir = dir.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || cmd("jj").current_dir(&dir).args(&args).run().ok())
        .await
        .ok()
        .flatten()
}

/// Push this workspace's chain, reporting whether it *actually pushed anything*.
/// `jj git push` exits 0 even when there is nothing to push (an unbookmarked or
/// undescribed working copy, or an already-pushed chain), printing "Nothing
/// changed." - so keying off the exit code alone marks an empty push a success
/// and makes the whole forge report "forged" when it moved nothing. Reading the
/// output turns that no-op into a `Skipped` "nothing to push" instead.
async fn did_push(dir: &Path) -> bool {
    match jj_in_run(dir, &["git", "push", "--revisions", PUSH_REVS]).await {
        Some(run) => pushed_something(run.ok(), run.stdout(), run.stderr()),
        None => false,
    }
}

/// The pure decision behind [`did_push`]: a push did real work only if it exited
/// zero *and* did not report "Nothing changed." on either stream.
fn pushed_something(ok: bool, stdout: &str, stderr: &str) -> bool {
    ok && !stdout.contains(NOTHING_CHANGED) && !stderr.contains(NOTHING_CHANGED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_indices_are_distinct_and_ordered() {
        assert_eq!(Step::Fetch.index(), 0);
        assert_eq!(Step::Weld.index(), 1);
        assert_eq!(Step::Push.index(), 2);
        assert_eq!(Step::Pr.index(), 3);
    }

    #[test]
    fn push_noop_is_not_real_work_despite_exit_zero() {
        // The bug: jj exits 0 with "Nothing changed." on stderr when a push
        // moved nothing - that must read as a no-op, not a success.
        assert!(!pushed_something(true, "", "Nothing changed.\n"));
        assert!(!pushed_something(true, "Nothing changed.\n", ""));
        // A real push (bookmark moved) has no such line.
        assert!(pushed_something(
            true,
            "",
            "Changes to push to origin:\n  Add bookmark adam/x to abc123\n"
        ));
        // A failed push is never real work.
        assert!(!pushed_something(false, "", ""));
    }

    #[test]
    fn step_result_marks_ok_or_skipped_with_reason() {
        match step_result("feat", Step::Push, true, "nothing to push") {
            Update::Step { status, reason, .. } => {
                assert_eq!(status, Status::Ok);
                assert!(reason.is_none());
            }
            _ => panic!("expected Step"),
        }
        match step_result("feat", Step::Push, false, "nothing to push") {
            Update::Step {
                status,
                reason,
                step,
                ws,
            } => {
                assert_eq!(status, Status::Skipped);
                assert_eq!(step, Step::Push);
                assert_eq!(ws, "feat");
                assert_eq!(reason.as_deref(), Some("push: nothing to push"));
            }
            _ => panic!("expected Step"),
        }
    }
}
