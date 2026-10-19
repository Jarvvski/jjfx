//! The durable work queue: what jjfx has queued, claimed, run and settled.
//!
//! The queue is the record that lets an interrupted Run be resumed and a
//! repeated step be recognised, so these tests are about the properties the
//! record must have. It is written before a Run starts and again as it ends, it
//! survives the process that wrote it rather than being recomputed from live
//! Worker state, reconciliation reads it (so work whose Worker has gone is still
//! resolved), and `status --json` exposes it as structure with stable names.
#![cfg(unix)]

use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use jjfx::engine::{
    DirectDispatchOutcome, DirectDispatchRequest, PoolCapacity, QueueItemState, Repository,
    RunMode, StepKey, Ticket, TicketId, TicketStatus, TicketTitle,
};
use tempfile::TempDir;

const HELPER_REPOSITORY: &str = "JJFX_WORK_QUEUE_REPOSITORY";
const HELPER_RESULT: &str = "JJFX_WORK_QUEUE_RESULT";

/// A jj Repository with an identity, an origin remote and a `main` bookmark.
///
/// The identity is passed as `JJ_CONFIG` rather than set with `jj config set
/// --repo`: jj stores repo config under a directory derived from
/// `XDG_CONFIG_HOME`, so a `--repo` write can land where jjfx never looks - and a
/// developer's own user config would hide that.
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

    /// `PATH` with the fixture's `bin` ahead of the inherited one.
    fn env_path(&self) -> OsString {
        let mut paths = vec![self.bin.path().to_path_buf()];
        paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
        env::join_paths(paths).expect("fixture PATH")
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

    fn jjfx(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_jjfx"))
            .args(args)
            .current_dir(self.path())
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .env("JJ_CONFIG", self.jj_config_path())
            .env("PATH", self.env_path())
            .output()
            .expect("jjfx should run")
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn ticket(id: &str, title: &str) -> Ticket {
    Ticket::new(
        TicketId::parse(id).expect("Ticket ID"),
        TicketTitle::parse(title).expect("Ticket title"),
        TicketStatus::parse("Todo").expect("Ticket status"),
    )
}

/// Dispatches a batch in its own process and reports what the queue held
/// immediately afterwards.
///
/// A separate process is the point: the parent then reads the queue from disk
/// with nothing of this process's memory left, which is the restart the record
/// has to survive.
#[test]
#[ignore]
fn work_queue_dispatch_helper() {
    let repository = Repository::open(env::var_os(HELPER_REPOSITORY).expect("Repository path"))
        .expect("open Repository");
    repository
        .worker_pool()
        .resize_to(PoolCapacity::new(2).expect("capacity"))
        .expect("grow Worker Pool");
    let requests = [
        DirectDispatchRequest::new(ticket("ENG-1", "First queued Ticket"), RunMode::Foreground),
        DirectDispatchRequest::new(ticket("ENG-2", "Second queued Ticket"), RunMode::Foreground),
    ];
    let result = repository
        .direct_dispatch()
        .dispatch(&requests)
        .expect("dispatch a batch");
    let fired = result
        .outcomes()
        .iter()
        .filter(|outcome| matches!(outcome, DirectDispatchOutcome::Succeeded(_)))
        .count();
    let queue = repository.work_queue().items().expect("read the queue");
    let summary = queue
        .iter()
        .map(|item| format!("{}={}", item.step.as_str(), item.state.as_str()))
        .collect::<Vec<_>>()
        .join(",");
    fs::write(
        env::var_os(HELPER_RESULT).expect("result path"),
        format!("fired={fired};queue={summary}"),
    )
    .expect("write helper result");
}

#[test]
fn a_dispatched_step_is_queued_durably_and_readable_as_structure() {
    let fixture = Fixture::new();
    fixture.with_fake_agent();
    let result_path = fixture.path().join("queue-result");
    let output = Command::new(env::current_exe().expect("test executable"))
        .args(["--exact", "work_queue_dispatch_helper", "--ignored"])
        .env("PATH", fixture.env_path())
        .env("JJ_CONFIG", fixture.jj_config_path())
        .env(HELPER_REPOSITORY, fixture.path())
        .env(HELPER_RESULT, &result_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run the dispatch helper");
    assert!(
        output.status.success(),
        "the dispatch helper failed:\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(
        fs::read_to_string(&result_path).expect("helper result"),
        "fired=2;queue=dispatch:ENG-1=done,dispatch:ENG-2=done"
    );

    // A fresh handle over the same Repository reads the same record from disk:
    // nothing here is recomputed from live Worker state.
    let repository = Repository::open(fixture.path()).expect("reopen repository");
    let items = repository.work_queue().items().expect("read the queue");
    assert_eq!(items.len(), 2);
    for (item, ticket) in items.iter().zip(["ENG-1", "ENG-2"]) {
        assert_eq!(item.step.as_str(), StepKey::dispatch(ticket).as_str());
        assert_eq!(item.kind.as_str(), "dispatch");
        assert_eq!(item.subject, ticket);
        assert_eq!(item.state, QueueItemState::Done);
        assert_eq!(item.sequence, if ticket == "ENG-1" { 0 } else { 1 });
        assert!(item.claimed_by.is_some(), "{ticket} was never claimed");
        assert!(item.claimed_at.is_some(), "{ticket} has no claim time");
        assert!(item.started_at.is_some(), "{ticket} never started");
        assert!(item.finished_at.is_some(), "{ticket} has no end");
    }

    // The same record is structure a script can read.
    let status = fixture.jjfx(&["status", "--json"]);
    assert!(
        status.status.success(),
        "status --json failed:\nstdout:\n{}\nstderr:\n{}",
        stdout(&status),
        stderr(&status)
    );
    let value: serde_json::Value = serde_json::from_str(&stdout(&status)).expect("status JSON");
    let queue = value["queue"].as_array().expect("a queue array");
    assert_eq!(queue.len(), 2, "{value}");
    assert_eq!(queue[0]["step"], "dispatch:ENG-1");
    assert_eq!(queue[0]["kind"], "dispatch");
    assert_eq!(queue[0]["subject"], "ENG-1");
    assert_eq!(queue[0]["state"], "done");
    assert_eq!(queue[0]["pool"], "default");
    assert!(queue[0]["claimed_by"].is_string(), "{value}");
    assert_eq!(queue[0]["sequence"], 0);
    assert!(queue[0]["queued_at"].is_string(), "{value}");
    assert!(queue[0]["claimed_at"].is_string(), "{value}");
    assert!(queue[0]["started_at"].is_string(), "{value}");
    assert!(queue[0]["finished_at"].is_string(), "{value}");
    assert!(queue[0]["in_flight"]["worker"].is_string(), "{value}");
    assert_eq!(queue[1]["step"], "dispatch:ENG-2");
    assert!(queue[1]["claimed_by"].is_string(), "{value}");
    assert_ne!(queue[0]["claimed_by"], queue[1]["claimed_by"], "{value}");
}

/// Reconciliation reads the queue, not live state alone: a step whose Worker has
/// left the Pool, or has moved on to other work, is still resolved - and the
/// resolution is written back, so "what happened to this?" has a durable answer
/// even when nothing live remembers the step (ticket 07's premise).
#[test]
fn reconciliation_resolves_the_queue_rather_than_forgetting_it() {
    let temp = TempDir::new().expect("temporary repository");
    let pool = temp.path().join(".jj/pools/default");
    fs::create_dir_all(&pool).expect("pool directory");
    fs::write(
        pool.join("pool.json"),
        br#"{"size":2,"gh_repo":"owner/repo","workers":["worker-01","worker-02"],"created_at":"2026-10-19T00:00:00Z","names":{}}"#,
    )
    .expect("pool state");
    fs::write(
        pool.join("worker-01.json"),
        br#"{"status":"done","agent":"claude","ticket":"ENG-1","pid":null,"started_at":"2026-10-19T00:00:00Z","completed_at":"2026-10-19T00:01:00Z","log_file":null,"branch_name":"eng-1","exit_code":0,"error":null}"#,
    )
    .expect("finished Worker state");
    fs::write(
        pool.join("worker-02.json"),
        br#"{"status":"idle","agent":"claude","ticket":null,"pid":null,"started_at":null,"completed_at":null,"log_file":null,"branch_name":null,"exit_code":null,"error":null}"#,
    )
    .expect("idle Worker state");
    // The queue a killed process left behind: one step whose Run finished, one
    // whose Worker has left the Pool, and one whose Worker has moved on.
    fs::write(
        pool.join("queue.json"),
        br#"{"items":[
            {"step":"dispatch:ENG-1","kind":"dispatch","subject":"ENG-1","state":"in_flight","sequence":0,"queued_at":"2026-10-19T00:00:00Z","claimed_at":"2026-10-19T00:00:00Z","claimed_by":"worker-01","started_at":"2026-10-19T00:00:00Z","in_flight":{"worker":"worker-01","branch":"eng-1","runtime":"claude"}},
            {"step":"dispatch:ENG-2","kind":"dispatch","subject":"ENG-2","state":"in_flight","sequence":1,"queued_at":"2026-10-19T00:00:00Z","claimed_at":"2026-10-19T00:00:00Z","claimed_by":"worker-09","in_flight":{"worker":"worker-09","branch":"eng-2"}},
            {"step":"dispatch:ENG-3","kind":"dispatch","subject":"ENG-3","state":"claimed","sequence":2,"queued_at":"2026-10-19T00:00:00Z","claimed_at":"2026-10-19T00:00:00Z","claimed_by":"worker-02","in_flight":{"worker":"worker-02","branch":"eng-3"}}
        ]}"#,
    )
    .expect("queue state");

    let repository = Repository::open(temp.path()).expect("repository");
    let snapshot = repository.worker_pool().reconcile_runs();
    let items = snapshot.queue();

    assert_eq!(items.len(), 3);
    assert_eq!(items[0].state, QueueItemState::Done);
    assert!(items[0].finished_at.is_some());
    assert_eq!(items[1].state, QueueItemState::Abandoned);
    assert!(
        items[1]
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("worker-09")),
        "{:?}",
        items[1].detail
    );
    assert_eq!(items[2].state, QueueItemState::Abandoned);
    assert!(
        items[2]
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("ENG-3")),
        "{:?}",
        items[2].detail
    );

    // The resolutions are durable, not a view: a later reader sees them too.
    let reopened = Repository::open(temp.path()).expect("reopen repository");
    let stored = reopened.work_queue().items().expect("read the queue");
    assert_eq!(
        stored.iter().map(|item| item.state).collect::<Vec<_>>(),
        [
            QueueItemState::Done,
            QueueItemState::Abandoned,
            QueueItemState::Abandoned
        ]
    );
}

/// A step that is still running is left in flight: reconciliation must not call
/// a live Run finished on a guess.
#[test]
fn reconciliation_leaves_a_running_step_in_flight() {
    let temp = TempDir::new().expect("temporary repository");
    let pool = temp.path().join(".jj/pools/default");
    fs::create_dir_all(&pool).expect("pool directory");
    fs::write(
        pool.join("pool.json"),
        br#"{"size":1,"gh_repo":"owner/repo","workers":["worker-01"],"created_at":"2026-10-19T00:00:00Z","names":{}}"#,
    )
    .expect("pool state");
    fs::write(
        pool.join("worker-01.json"),
        br#"{"status":"busy","agent":"opencode","ticket":"ENG-1","pid":null,"started_at":"2026-10-19T00:00:00Z","completed_at":null,"log_file":null,"branch_name":"eng-1","exit_code":null,"error":null}"#,
    )
    .expect("busy Worker state");
    fs::write(
        pool.join("queue.json"),
        br#"{"items":[{"step":"dispatch:ENG-1","kind":"dispatch","subject":"ENG-1","state":"in_flight","sequence":0,"queued_at":"2026-10-19T00:00:00Z","claimed_at":"2026-10-19T00:00:00Z","claimed_by":"worker-01","in_flight":{"worker":"worker-01","branch":"eng-1","runtime":"opencode","session":"ses_live"}}]}"#,
    )
    .expect("queue state");

    let repository = Repository::open(temp.path()).expect("repository");
    let snapshot = repository.worker_pool().reconcile_runs();

    let item = &snapshot.queue()[0];
    assert_eq!(item.state, QueueItemState::InFlight);
    assert_eq!(item.finished_at, None);
    assert_eq!(
        item.in_flight
            .as_ref()
            .and_then(|record| record.session.as_deref()),
        Some("ses_live"),
        "the record a resumption needs must survive reconciliation"
    );
}
