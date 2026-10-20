//! Resuming the work an interruption left in flight (ticket 07).
//!
//! An interruption - jjfx killed with Runs in flight - must leave a durable
//! record of what was in flight rather than only a terminal status, and that
//! record must let the work be resumed **explicitly**, without redoing the steps
//! that completed. The queue's per-step state is the one notion of progress
//! these tests hold the resumption to.
//!
//! The interruption is a real kill of a real process: a stand-in for the Run is
//! started in its own process group, recorded on the Worker and in the queue's
//! in-flight record exactly as a Dispatch records it, and then killed where it
//! stands. What is *not* real is that jjfx launched that process - the record is
//! the same record a killed Run leaves, but it is written by the test rather
//! than by a Dispatch, so the kill is deterministic rather than racing a real
//! Run's start-up. This repository has been bitten by load-sensitive tests three
//! times, so the interruption is fixed by construction instead of by timing.
//! The resumption itself is not simulated: the resumed step really is dispatched
//! and really runs.
#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use jjfx::engine::{
    AgentRuntime, AgentRuntimeProfile, InFlightRecord, PoolCapacity, QueueItemState,
    QueueSettlement, QueuedStep, Repository, RunRecord, RunRecordConclusion, StepKey,
    WireTimestamp, WorkerId,
};
use tempfile::TempDir;

mod support;

/// A jj Repository with an identity, an origin remote and a `main` bookmark.
///
/// The identity is passed as `JJ_CONFIG` rather than set with `jj config set
/// --repo`: jj stores repo config under a directory derived from
/// `XDG_CONFIG_HOME`, so a `--repo` write can land where jjfx never looks.
struct Fixture {
    root: TempDir,
    config_home: TempDir,
    bin: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().expect("temporary repository");
        let config_home = TempDir::new().expect("temporary config home");
        let bin = TempDir::new().expect("temporary bin");
        fs::write(
            config_home.path().join("jj.toml"),
            "user.name = \"Test User\"\nuser.email = \"test@example.com\"\n",
        )
        .expect("jj config file");
        let fixture = Self {
            root,
            config_home,
            bin,
        };
        fixture.jj(&["git", "init", "--no-colocate"]);
        fixture.jj(&[
            "git",
            "remote",
            "add",
            "origin",
            "git@github.com:owner/repo.git",
        ]);
        fixture.jj(&["bookmark", "create", "main", "-r", "@"]);
        fixture
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn jj_config_path(&self) -> PathBuf {
        self.config_home.path().join("jj.toml")
    }

    /// `PATH` with the fixture's `bin` ahead of the inherited one, holding the
    /// stub terminal Dispatch's Eligibility check needs.
    ///
    /// `resume` re-dispatches the work it recovers, so it runs the same
    /// terminal seam as `dispatch` (ADR 0011). Without a `kitten` that answers,
    /// that seam fails closed and the resumption never runs - which is green on
    /// a machine running kitty and red on CI. The stub answers deterministically,
    /// so the test does not depend on the machine it runs on.
    fn env_path(&self) -> OsString {
        support::stub_terminal(self.bin.path())
    }

    fn jj(&self, args: &[&str]) {
        let output = Command::new("jj")
            .args(["--config", "signing.behavior=drop"])
            .args(args)
            .current_dir(self.path())
            .env("JJ_CONFIG", self.jj_config_path())
            .output()
            .expect("jj should be installed");
        assert!(
            output.status.success(),
            "jj {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Puts a fake `claude` on the fixture's `PATH` that exits successfully, so a
    /// Dispatch can launch a Run without a real Agent Runtime.
    fn with_fake_agent(&self) {
        let agent = self.bin.path().join("claude");
        fs::write(
            &agent,
            "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n",
        )
        .expect("fake agent");
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).expect("executable agent");
    }

    /// Runs a `jjfx` invocation, tolerating a stub terminal starved past the
    /// seam's deadline under load.
    ///
    /// The stub answers instantly, but the seam gives the real subprocess a
    /// short deadline, so on a loaded machine it can be starved past it and
    /// Dispatch fails closed before reaching the behaviour under test. That is
    /// an environmental transient, not a product failure, so the shared runner
    /// retries it, bounded by the liveness budget.
    fn jjfx(&self, args: &[&str]) -> Output {
        support::run_jjfx_tolerating_a_starved_terminal(|| {
            let mut command = Command::new(env!("CARGO_BIN_EXE_jjfx"));
            command
                .args(args)
                .current_dir(self.path())
                .env("XDG_CONFIG_HOME", self.config_home.path())
                .env("JJ_CONFIG", self.jj_config_path())
                .env("PATH", self.env_path());
            command
        })
    }

    /// The Pool's state directory.
    fn pool_dir(&self) -> PathBuf {
        self.path().join(".jj/pools/default")
    }

    /// Opens the Repository and grows the Pool to two Workers, so one can hold
    /// the interrupted Run while the other is free to resume it.
    fn repository_with_two_workers(&self) -> (Repository, WorkerId, WorkerId) {
        let repository = Repository::open(self.path()).expect("open the Repository");
        repository
            .worker_pool()
            .resize_to(PoolCapacity::new(2).expect("capacity"))
            .expect("grow the Worker Pool");
        let workers = repository
            .worker_pool()
            .snapshot()
            .workers()
            .iter()
            .map(|worker| worker.worker_id().clone())
            .collect::<Vec<_>>();
        assert_eq!(workers.len(), 2, "the Pool must hold two Workers");
        (repository, workers[0].clone(), workers[1].clone())
    }

    /// Starts the process that stands in for the Run, leading its own process
    /// group as a launched Run does, and records it on the Worker so
    /// reconciliation reads the Run the queue names.
    fn start_run(&self, worker: &WorkerId, ticket: &str) -> Child {
        let mut command = Command::new("sleep");
        command
            .arg("300")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let run = command.spawn().expect("a stand-in for the Run");

        let path = self.pool_dir().join(format!("{worker}.json"));
        let mut state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("Worker state"))
                .expect("Worker state JSON");
        state["status"] = "busy".into();
        state["agent"] = "claude".into();
        state["ticket"] = ticket.into();
        state["pid"] = serde_json::json!(run.id());
        state["started_at"] = "2026-10-19T00:00:00Z".into();
        state["branch_name"] = ticket.to_lowercase().into();
        fs::write(&path, state.to_string()).expect("running Worker state");
        run
    }

    /// Writes the queue an interruption leaves behind, through the queue's own
    /// API: one step that completed, and one claimed and begun on `worker`.
    fn seed_interrupted_queue(&self, repository: &Repository, worker: &WorkerId, pid: u32) {
        let queue = repository.work_queue();
        let at = WireTimestamp::new("2026-10-19T00:00:00Z");
        queue
            .enqueue(&[
                QueuedStep::dispatch("ENG-1", at.clone(), None),
                QueuedStep::dispatch("ENG-2", at.clone(), None),
            ])
            .expect("queue the steps");
        queue
            .settle(
                &StepKey::dispatch("ENG-1"),
                QueueSettlement::Done,
                at.clone(),
            )
            .expect("settle the completed step");
        queue
            .claim(
                &StepKey::dispatch("ENG-2"),
                InFlightRecord::for_run(
                    worker.clone(),
                    repository
                        .worker_workspace_path(worker)
                        .to_string_lossy()
                        .into_owned(),
                    "eng-2",
                    &AgentRuntimeProfile::new(AgentRuntime::Claude),
                ),
                at.clone(),
            )
            .expect("claim the interrupted step");
        queue
            .begin(&StepKey::dispatch("ENG-2"), None, Some(i64::from(pid)), at)
            .expect("begin the interrupted step");
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The Run Records the Pool holds, in append order.
fn run_records(repository: &Repository) -> Vec<RunRecord> {
    repository
        .state_store()
        .runs()
        .records()
        .expect("Run Records")
}

/// Kills the child and reaps it, so a test that starts a stand-in for a Run
/// leaves no process behind.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The record of an interrupted Run survives the process that wrote it, and the
/// resumption re-runs that step without redoing the one that completed.
#[test]
fn an_interrupted_step_is_resumed_without_redoing_completed_work() {
    let fixture = Fixture::new();
    fixture.with_fake_agent();
    let (repository, interrupted, _free) = fixture.repository_with_two_workers();
    let mut run = fixture.start_run(&interrupted, "ENG-2");
    fixture.seed_interrupted_queue(&repository, &interrupted, run.id());
    drop(repository);

    // The interruption: the Run's process is killed where it stands, mid-flight.
    run.kill().expect("kill the in-flight Run");
    run.wait().expect("reap the killed Run");

    // A fresh handle over the same Repository is the restart: the record is read
    // from disk, with nothing of the writing process's memory left.
    let reopened = Repository::open(fixture.path()).expect("reopen the Repository");
    let items = reopened.work_queue().items().expect("read the queue");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].state, QueueItemState::Done, "ENG-1 completed");
    let in_flight = &items[1];
    assert_eq!(
        in_flight.state,
        QueueItemState::InFlight,
        "ENG-2 was in flight"
    );
    let record = in_flight
        .in_flight
        .as_ref()
        .expect("the in-flight record must survive the interruption");
    assert_eq!(
        record.worker.as_ref().map(WorkerId::as_str),
        Some(interrupted.as_str())
    );
    assert_eq!(record.pid, Some(i64::from(run.id())));
    assert_eq!(record.branch.as_deref(), Some("eng-2"));
    assert_eq!(record.runtime.as_deref(), Some("claude"));
    assert!(
        record
            .workspace
            .as_deref()
            .is_some_and(|workspace| !workspace.is_empty()),
        "the record must name where the Run was, not just that it was"
    );

    // An operator asks for the resumption; nothing re-runs on its own.
    let resumed = fixture.jjfx(&["resume", "--fg"]);
    assert!(
        resumed.status.success(),
        "resume failed:\nstdout:\n{}\nstderr:\n{}",
        stdout(&resumed),
        stderr(&resumed)
    );
    let report = stderr(&resumed);
    assert!(
        report.contains("Resumed 1 step(s) in pool 'default': ENG-2"),
        "{report}"
    );
    assert!(report.contains("ENG-1 already done"), "{report}");

    // The resumed step ran to a conclusion; the completed one did not re-run.
    let after = Repository::open(fixture.path()).expect("reopen the Repository");
    let items = after.work_queue().items().expect("read the queue");
    assert_eq!(
        items[0].state,
        QueueItemState::Done,
        "the completed step must be left exactly as it was"
    );
    assert_eq!(
        items[0].finished_at.as_ref().map(WireTimestamp::as_str),
        Some("2026-10-19T00:00:00Z"),
        "the completed step must not be re-settled"
    );
    assert_eq!(items[1].state, QueueItemState::Done, "the resumed step ran");

    // The resumed step really ran, and the completed one was not redone: the
    // interrupted attempt and the resumed one are both Run Records, and ENG-1
    // has none at all.
    let records = run_records(&after);
    assert!(
        !records
            .iter()
            .any(|record| record.ticket.as_deref() == Some("ENG-1")),
        "a completed step must not be redone: {records:?}"
    );
    let eng_2 = records
        .iter()
        .filter(|record| record.ticket.as_deref() == Some("ENG-2"))
        .collect::<Vec<_>>();
    assert_eq!(
        eng_2.len(),
        2,
        "the interrupted attempt and the resumed one are both recorded: {records:?}"
    );
    assert_eq!(eng_2[0].conclusion, RunRecordConclusion::Failed);
    assert_eq!(eng_2[1].conclusion, RunRecordConclusion::Succeeded);
}

/// Resumption is explicit and its reach is the queue's own state: a step whose
/// Run reconciliation has just called live is left running, so a resumption
/// never races a Run that may still have side effects.
#[test]
fn a_step_whose_run_is_still_live_is_left_running() {
    let fixture = Fixture::new();
    fixture.with_fake_agent();
    let (repository, running, _free) = fixture.repository_with_two_workers();

    // A real process, leading its own process group, stands in for the live Run:
    // the recorded PID is what reconciliation reads, and it really is alive.
    let live = KillOnDrop(fixture.start_run(&running, "ENG-2"));
    fixture.seed_interrupted_queue(&repository, &running, live.0.id());
    drop(repository);

    let resumed = fixture.jjfx(&["resume", "--fg"]);

    assert!(
        resumed.status.success(),
        "resume failed:\nstdout:\n{}\nstderr:\n{}",
        stdout(&resumed),
        stderr(&resumed)
    );
    let report = stderr(&resumed);
    assert!(report.contains("Resumed 0 step(s)"), "{report}");
    assert!(report.contains("still in flight"), "{report}");

    // Nothing ran: the live step is untouched and no Run was launched for it.
    let after = Repository::open(fixture.path()).expect("reopen the Repository");
    let items = after.work_queue().items().expect("read the queue");
    assert_eq!(items[1].state, QueueItemState::InFlight);
    assert_eq!(items[1].finished_at, None);
    assert!(
        run_records(&after).is_empty(),
        "a Run jjfx believes is live must not be restarted"
    );
}

/// A second resumption, once everything has finished, is a no-op that says so
/// rather than an error or a silent re-run; and a Ticket named by hand with no
/// recorded step is reported rather than dispatched.
#[test]
fn resuming_nothing_is_a_no_op_and_names_what_it_could_not_resume() {
    let fixture = Fixture::new();
    fixture.with_fake_agent();
    let (repository, interrupted, _free) = fixture.repository_with_two_workers();
    let mut run = fixture.start_run(&interrupted, "ENG-2");
    fixture.seed_interrupted_queue(&repository, &interrupted, run.id());
    drop(repository);
    run.kill().expect("kill the in-flight Run");
    run.wait().expect("reap the killed Run");

    let first = fixture.jjfx(&["resume", "--fg"]);
    assert!(
        first.status.success(),
        "resume failed:\nstdout:\n{}\nstderr:\n{}",
        stdout(&first),
        stderr(&first)
    );

    // Everything the queue holds is done now, so a resumption redoes nothing.
    let again = fixture.jjfx(&["resume", "--fg"]);
    assert!(again.status.success(), "{}", stderr(&again));
    let report = stderr(&again);
    assert!(report.contains("Resumed 0 step(s)"), "{report}");
    assert_eq!(report.matches("already done").count(), 2, "{report}");

    // A Ticket with no recorded step is not dispatched by `resume`: `dispatch`
    // is the command that runs work jjfx has not accepted.
    let unrecorded = fixture.jjfx(&["resume", "ENG-9", "--fg"]);
    assert!(
        unrecorded.status.success(),
        "resume failed:\nstdout:\n{}\nstderr:\n{}",
        stdout(&unrecorded),
        stderr(&unrecorded)
    );
    let report = stderr(&unrecorded);
    assert!(report.contains("ENG-9"), "{report}");
    assert!(report.contains("no step is recorded"), "{report}");

    // A queue that holds nothing at all is a no-op too.
    let empty = Fixture::new();
    empty.with_fake_agent();
    let nothing = empty.jjfx(&["resume", "--fg"]);
    assert!(nothing.status.success(), "{}", stderr(&nothing));
    assert!(
        stderr(&nothing).contains("No unfinished steps recorded"),
        "{}",
        stderr(&nothing)
    );
}

/// Dogfood-2 ticket 06: a step that settled `failed` and was then dispatched
/// again to success reads `done`, not the first failure - and a resumption
/// leaves the completed step alone rather than re-running landed work.
///
/// A step is keyed by identity, so the second Dispatch of the same Ticket is
/// the *same* step. Without the fix the first claim settled the step `failed`
/// and every later transition was refused as terminal, so the queue reported
/// the first failure forever and `resume` re-opened work that had already
/// landed.
#[test]
fn a_redispatch_that_succeeds_supersedes_the_earlier_failure() {
    let fixture = Fixture::new();
    fixture.with_fake_agent();
    let (repository, worker, _free) = fixture.repository_with_two_workers();

    // The failure the anchor bug left behind: the step settled `failed` with its
    // reason, exactly as `queue.json` read in the report.
    let at = WireTimestamp::new("2026-10-19T00:00:00Z");
    let queue = repository.work_queue();
    queue
        .enqueue(&[QueuedStep::dispatch("ENG-1", at.clone(), None)])
        .expect("queue the step");
    queue
        .claim(
            &StepKey::dispatch("ENG-1"),
            InFlightRecord::for_run(
                worker.clone(),
                repository
                    .worker_workspace_path(&worker)
                    .to_string_lossy()
                    .into_owned(),
                "eng-1",
                &AgentRuntimeProfile::new(AgentRuntime::Claude),
            ),
            at.clone(),
        )
        .expect("claim the first attempt");
    queue
        .begin(&StepKey::dispatch("ENG-1"), None, None, at.clone())
        .expect("begin the first attempt");
    queue
        .settle(
            &StepKey::dispatch("ENG-1"),
            QueueSettlement::Failed {
                detail: "cannot anchor Worker worker-01's opencode session".to_owned(),
            },
            at.clone(),
        )
        .expect("settle the first attempt failed");
    drop(queue);

    // A second Dispatch of the same Ticket is the same step: it re-runs and
    // settles `done`.
    let dispatched = fixture.jjfx(&["dispatch", "--text", "ENG-1", "--id", "ENG-1", "--fg"]);
    assert!(
        dispatched.status.success(),
        "re-dispatch failed:\nstdout:\n{}\nstderr:\n{}",
        stdout(&dispatched),
        stderr(&dispatched)
    );

    let after = Repository::open(fixture.path()).expect("reopen the Repository");
    let item = after
        .work_queue()
        .item(&StepKey::dispatch("ENG-1"))
        .expect("item")
        .expect("recorded");
    assert_eq!(
        item.state,
        QueueItemState::Done,
        "the queue must reflect the latest outcome, not the first failure"
    );
    assert_eq!(
        item.detail, None,
        "the stale failure's reason must not survive a successful re-dispatch"
    );
    assert!(item.finished_at.is_some(), "the successful attempt ended");

    // `resume` reads the queue, which now says done: it leaves the completed
    // step alone instead of re-dispatching landed work.
    let resumed = fixture.jjfx(&["resume", "--fg"]);
    assert!(
        resumed.status.success(),
        "resume failed:\nstdout:\n{}\nstderr:\n{}",
        stdout(&resumed),
        stderr(&resumed)
    );
    let report = stderr(&resumed);
    assert!(report.contains("Resumed 0 step(s)"), "{report}");
    assert!(report.contains("ENG-1 already done"), "{report}");

    // Nothing re-ran: the seeded attempt never launched, so the successful
    // re-dispatch is the only Run Record, and the resumption added none.
    let records = run_records(&after);
    assert_eq!(
        records
            .iter()
            .filter(|record| record.ticket.as_deref() == Some("ENG-1"))
            .count(),
        1,
        "the resumption must not re-run the completed step: {records:?}"
    );
    assert_eq!(records[0].conclusion, RunRecordConclusion::Succeeded);
}
