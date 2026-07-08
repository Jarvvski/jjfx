//! The forge pipeline (ADR 0005, ticket 08): fetch -> weld -> push -> spr, run
//! natively with the workspace-safe revsets ported from `jj-forge`, modeled as
//! real step state that streams to the TUI rather than scraped stdout.
//!
//! Each mutating step runs with its current directory set to the workspace's own
//! path, so `@` resolves to *that* workspace's working copy - forging one
//! workspace never rebases another's chain. Steps shell out via `spawn_blocking`
//! (jj/gh/jj-spr calls block), and each transition is sent to the single owned
//! `App` as a [`Msg::Forge`].

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tokio::sync::mpsc::UnboundedSender;

use crate::app::Msg;

/// Weld source: the root of this workspace's own mutable chain, rebased onto
/// `trunk()`. Scoped to `::@` so only this workspace's stack moves.
const WELD_SRC: &str = "roots(mutable() & mine() & ::@)";
/// Push revisions: this workspace's chain, minus trunk and any conflicts.
const PUSH_REVS: &str = "::@ ~ trunk() ~ conflicts()";
/// The `JJ_SPR_REVSET` handed to `jj-spr`: this workspace's own non-trunk chain.
const SPR_REVSET: &str = "(::@ ~ trunk()) & mine()";

/// The four forge steps, in pipeline order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Fetch,
    Weld,
    Push,
    Spr,
}

impl Step {
    /// Position in the four-slot progress array.
    pub fn index(self) -> usize {
        match self {
            Step::Fetch => 0,
            Step::Weld => 1,
            Step::Push => 2,
            Step::Spr => 3,
        }
    }

    /// Single-letter tag for the compact row pipeline.
    pub fn letter(self) -> char {
        match self {
            Step::Fetch => 'f',
            Step::Weld => 'w',
            Step::Push => 'p',
            Step::Spr => 's',
        }
    }

    fn name(self) -> &'static str {
        match self {
            Step::Fetch => "fetch",
            Step::Weld => "weld",
            Step::Push => "push",
            Step::Spr => "spr",
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
pub async fn run(tx: UnboundedSender<Msg>, repo_root: PathBuf, targets: Vec<Target>) {
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

    for t in &targets {
        forge_one(&tx, t).await;
    }
}

/// Weld -> push -> spr for one workspace, skipping the whole pipeline if its
/// working copy is conflicted. Each step is best-effort: a failure is reported as
/// `Skipped` (mirroring `jj-forge`) and the pipeline continues.
async fn forge_one(tx: &UnboundedSender<Msg>, t: &Target) {
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
    let pushed = jj_in(&t.dir, &["git", "push", "--revisions", PUSH_REVS]).await;
    send(
        tx,
        step_result(&t.name, Step::Push, pushed, "nothing to push"),
    );

    send(tx, step_running(&t.name, Step::Spr));
    let spr = spr_sync(&t.dir).await;
    send(tx, step_result(&t.name, Step::Spr, spr, "spr sync skipped"));

    send(tx, Update::Done { ws: t.name.clone() });
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
        let out = Command::new("jj")
            .arg("--repository")
            .arg(&root)
            .args(["config", "get", &key])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
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
        let out = Command::new("jj")
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
            .output();
        matches!(out, Ok(o) if o.status.success() && !o.stdout.is_empty())
    })
    .await
    .unwrap_or(false)
}

/// Run a jj command against the repo (fetch is repo-global). Success only.
async fn jj_at(repo_root: &Path, args: &[&str]) -> bool {
    let root = repo_root.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || {
        Command::new("jj")
            .arg("--repository")
            .arg(&root)
            .args(&args)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

/// Run a jj command in the workspace's directory (so `@` is that workspace's
/// working copy, and jj snapshots it first - matching `jj-forge`). Success only.
async fn jj_in(dir: &Path, args: &[&str]) -> bool {
    let dir = dir.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || {
        Command::new("jj")
            .current_dir(&dir)
            .args(&args)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

/// Run `jj-spr sync` in the workspace dir with the scoped `JJ_SPR_REVSET`.
async fn spr_sync(dir: &Path) -> bool {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        Command::new("jj-spr")
            .current_dir(&dir)
            .env("JJ_SPR_REVSET", SPR_REVSET)
            .arg("sync")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_indices_are_distinct_and_ordered() {
        assert_eq!(Step::Fetch.index(), 0);
        assert_eq!(Step::Weld.index(), 1);
        assert_eq!(Step::Push.index(), 2);
        assert_eq!(Step::Spr.index(), 3);
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
