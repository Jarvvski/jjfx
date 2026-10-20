use std::env;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use jjfx::engine::{
    AgentModel, AgentRuntime, AgentSessionResolution, DismissOutcome, Expected, FollowUpExecution,
    Loaded, PaneLayout, PoolCapacity, Repository, RunMode, RunReset, StateChange, WireAgent,
    WireStatus, WireTimestamp, WorkerActions, WorkerId, WorkspaceRestoration,
};
use rustix::fs::{FlockOperation, flock};
use tempfile::TempDir;

const HELPER_REPOSITORY: &str = "JJFX_ACTION_REPOSITORY";
const HELPER_WORKER: &str = "JJFX_ACTION_WORKER";
const HELPER_RESULT: &str = "JJFX_ACTION_RESULT";
const HELPER_CAPTURE: &str = "JJFX_ACTION_CAPTURE";
const HELPER_MODE: &str = "JJFX_ACTION_MODE";
const HELPER_PROVIDER: &str = "JJFX_ACTION_PROVIDER";
const HELPER_MODEL: &str = "JJFX_ACTION_MODEL";
const HELPER_PANE_COMMAND: &str = "JJFX_ACTION_PANE_COMMAND";
const HELPER_LAYOUT: &str = "JJFX_ACTION_LAYOUT";
const HELPER_EXIT: &str = "JJFX_ACTION_EXIT";

#[test]
fn send_rejects_a_busy_worker_through_the_actions_facade() {
    let (temporary_directory, repository) = local_repository();
    let growth = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("Worker Workspace should be provisioned");
    let worker = growth.added_workers()[0].clone();
    repository
        .worker_pool()
        .reserve_named(worker.clone(), "ENG-301")
        .expect("initial Run reservation");

    let result =
        WorkerActions::new(repository).send(&worker, "continue the work", RunMode::Background);

    assert!(result.is_err());
    drop(temporary_directory);
}

#[test]
fn send_any_reports_that_no_idle_worker_is_available() {
    let (_temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    repository
        .worker_pool()
        .reserve_named(worker, "ENG-303")
        .expect("initial Run reservation");

    let error = WorkerActions::new(repository)
        .send_any("continue the work", RunMode::Background)
        .expect_err("an all-busy Pool cannot accept an arbitrary task");

    assert!(error.to_string().contains("no idle Worker"));
}

#[test]
fn send_any_uses_the_first_idle_worker_and_reports_its_identity() {
    let (temporary_directory, repository) = local_repository();
    let growth = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(2).expect("capacity"))
        .expect("Worker Workspaces should be provisioned");
    let busy = growth.added_workers()[0].clone();
    let idle = growth.added_workers()[1].clone();
    repository
        .worker_pool()
        .reserve_named(busy, "ENG-304")
        .expect("first Worker should be busy");

    let bin = temporary_directory.path().join("send-any-bin");
    fs::create_dir(&bin).expect("fake executable directory");
    write_executable(
        &bin.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n",
    );
    let result = temporary_directory.path().join("send-any-result");
    let path = env::join_paths([
        bin.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let output = Command::new(env::current_exe().expect("test executable"))
        .args(["--exact", "send_any_action_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .output()
        .expect("Send-any helper should run");

    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("Send-any result"),
        format!("worker={idle}; completed=true")
    );
}

/// A Follow-up must take the Worker's Workspace-operation lock, so it can never
/// start a Run in a Workspace a Reset is still restoring.
#[test]
fn send_waits_for_the_workspace_operation_lock() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);

    let bin = temporary_directory.path().join("lock-bin");
    fs::create_dir(&bin).expect("fake executable directory");
    write_executable(
        &bin.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nexit 0\n",
    );
    let path = env::join_paths([
        bin.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");

    // Hold the same sidecar lock `lock_worker_operation` uses, across processes.
    let lock_path = repository
        .root()
        .join(format!(".jj/pools/default/{worker}.workspace.lock"));
    fs::create_dir_all(lock_path.parent().expect("lock parent")).expect("lock directory");
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("operation lock file");
    flock(&lock_file, FlockOperation::LockExclusive).expect("hold operation lock");

    let result = temporary_directory.path().join("lock-result");
    let mut child = Command::new(env::current_exe().expect("test executable"))
        .args(["--exact", "send_action_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Send helper should start");

    thread::sleep(Duration::from_millis(400));
    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "a Follow-up must wait for the Worker's Workspace operation lock"
    );

    drop(lock_file);
    let output = child.wait_with_output().expect("helper output");
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::read_to_string(&result)
            .expect("Send result")
            .contains("completed=true"),
        "the Follow-up should complete once the lock is released"
    );
}

/// A Rebase rewrites the Worker's working copy, so it must take the same
/// Workspace-operation lock a Reset holds while restoring. Without it a Rebase
/// can edit a working copy that is still mid-restore.
#[test]
fn rebase_waits_for_the_workspace_operation_lock() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("rebase-lock.log");
    fs::write(&log, "{}\n").expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    let bin = temporary_directory.path().join("rebase-lock-bin");
    fs::create_dir(&bin).expect("fake command directory");
    let command_script = format!(
        "#!/bin/sh\nprintf '%s %s\\n' \"$(basename \"$0\")\" \"$*\" >> \"$JJFX_ACTION_CAPTURE\"\ncase \" $* \" in\n{LANDING_JJ_ARMS}esac\n"
    );
    write_executable(&bin.join("jj"), &command_script);
    write_executable(&bin.join("gh"), &command_script);
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    // Hold the same sidecar lock `lock_worker_operation` uses, across processes.
    let lock_path = repository
        .root()
        .join(format!(".jj/pools/default/{worker}.workspace.lock"));
    fs::create_dir_all(lock_path.parent().expect("lock parent")).expect("lock directory");
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("operation lock file");
    flock(&lock_file, FlockOperation::LockExclusive).expect("hold operation lock");

    let capture = temporary_directory.path().join("rebase-lock-commands");
    let result = temporary_directory.path().join("rebase-lock-result");
    let mut child = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "repository_action_helper"])
        .env(HELPER_MODE, "repository")
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Rebase helper should start");

    thread::sleep(Duration::from_millis(400));
    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "a Rebase must wait for the Worker's Workspace operation lock"
    );

    drop(lock_file);
    let output = child.wait_with_output().expect("helper output");
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        jj_operations(&fs::read_to_string(&capture).expect("command capture")),
        vec![
            "bookmark list -T if(remote, \"\", name ++ \"\\n\")",
            "rebase -b owner/eng-301-action -d main",
            "git push --bookmark owner/eng-301-action",
            "bookmark list -T if(remote, \"\", name ++ \"\\n\")",
            // The persisted `gh_repo` is checked against `origin` before any
            // `gh` call, so a fabricated one never reaches the network.
            "git remote list",
            "gh -R owner/repo pr view owner/eng-301-action --web",
        ]
    );
}

#[test]
fn dismiss_removes_an_idle_worker_and_reduces_pool_capacity() {
    let (_temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);

    let outcome = WorkerActions::new(repository.clone())
        .dismiss(&worker)
        .expect("idle Worker should be removable");

    assert_eq!(outcome, DismissOutcome::Removed { capacity: 0 });
    let snapshot = repository.worker_pool().snapshot();
    assert!(snapshot.worker(worker.as_str()).is_none());
    assert_eq!(snapshot.pool().expect("Pool state").size(), 0);
}

#[test]
fn dismiss_clears_terminal_worker_without_restoring_or_removing_workspace() {
    let (_temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let workspace = worker_workspace_path(&repository, &worker);
    let log = repository.root().join("dismiss-terminal.log");
    fs::write(&log, "{}\n").expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    let outcome = WorkerActions::new(repository.clone())
        .dismiss(&worker)
        .expect("terminal Worker should be clearable");

    assert_eq!(outcome, DismissOutcome::Reset);
    assert!(workspace.is_dir());
    let snapshot = repository.worker_pool().snapshot();
    let worker_state = snapshot
        .worker(worker.as_str())
        .expect("Worker remains in Pool");
    assert_eq!(worker_state.status(), jjfx::engine::WorkerStatus::Idle);
}

#[test]
fn dismiss_rejects_a_busy_worker_without_changing_its_lifecycle() {
    let (_temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    repository
        .worker_pool()
        .reserve_named(worker.clone(), "ENG-302")
        .expect("Run reservation");

    let error = WorkerActions::new(repository.clone())
        .dismiss(&worker)
        .expect_err("busy Worker must require Reset instead");

    assert!(error.to_string().contains("not idle"));
    assert_eq!(
        repository
            .worker_pool()
            .snapshot()
            .worker(worker.as_str())
            .expect("Worker remains")
            .status(),
        jjfx::engine::WorkerStatus::Busy
    );
}

#[test]
fn logs_returns_a_typed_provider_log_without_rendering_it() {
    let (_temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("typed-worker.log");
    fs::write(
        &log,
        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}\n",
    )
    .expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    let logs = WorkerActions::new(repository)
        .logs(&worker)
        .expect("Logs should resolve");

    assert_eq!(logs.runtime(), AgentRuntime::Claude);
    assert_eq!(logs.path(), log);
    assert!(logs.open().final_result().expect("Run result").is_some());
}

#[test]
fn repository_actions_reject_a_worker_without_a_branch() {
    let (_temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let actions = WorkerActions::new(repository);

    assert!(actions.rebase(&worker).is_err());
    assert!(actions.open_pull_request(&worker).is_err());
}

#[test]
fn rebase_and_open_pull_request_resolve_the_prefixed_bookmark_the_agent_created() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("repository-actions.log");
    fs::write(&log, "{}\n").expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    let bin = temporary_directory.path().join("bin");
    fs::create_dir(&bin).expect("fake command directory");
    let command_script = format!(
        "#!/bin/sh\nprintf '%s %s\\n' \"$(basename \"$0\")\" \"$*\" >> \"$JJFX_ACTION_CAPTURE\"\ncase \" $* \" in\n{LANDING_JJ_ARMS}esac\n"
    );
    write_executable(&bin.join("jj"), &command_script);
    write_executable(&bin.join("gh"), &command_script);
    let capture = temporary_directory.path().join("commands");
    let result = temporary_directory.path().join("result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "repository_action_helper"])
        .env(HELPER_MODE, "repository")
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env("PATH", path)
        .output()
        .expect("repository action helper should run");

    assert!(
        output.status.success(),
        "repository action helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Worker state names the bare Ticket (`eng-301`), the repository names the
    // prefixed bookmark the dispatch prompt asked the agent to create. Both
    // verbs must find the latter, not fail on the former.
    assert_eq!(
        fs::read_to_string(&result).expect("action result"),
        "owner/eng-301-action\nowner/eng-301-action\n"
    );
    assert_eq!(
        jj_operations(&fs::read_to_string(&capture).expect("command capture")),
        vec![
            "bookmark list -T if(remote, \"\", name ++ \"\\n\")",
            "rebase -b owner/eng-301-action -d main",
            "git push --bookmark owner/eng-301-action",
            "bookmark list -T if(remote, \"\", name ++ \"\\n\")",
            // The persisted `gh_repo` is checked against `origin` before any
            // `gh` call, so a fabricated one never reaches the network.
            "git remote list",
            "gh -R owner/repo pr view owner/eng-301-action --web",
        ]
    );
}

/// A Pool created before `dogfood/03` still carries the slug the old derivation
/// mined from a non-GitHub `origin`. `open-pr` must refuse that value by name,
/// before it contacts `gh`: reaching the network to discover that a slug is not
/// a slug is the failure being fixed.
#[test]
fn open_pull_request_refuses_a_legacy_fabricated_gh_repo_before_calling_gh() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("legacy-gh-repo.log");
    fs::write(&log, "{}\n").expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);
    jj_at(
        repository.root(),
        &["bookmark", "create", "owner/eng-301-action", "-r", "@"],
    );
    let origin = TempDir::new().expect("origin repository");
    install_legacy_fabricated_gh_repo(&repository, origin.path());

    let bin = temporary_directory.path().join("legacy-gh-repo-bin");
    fs::create_dir(&bin).expect("fake command directory");
    write_executable(
        &bin.join("gh"),
        "#!/bin/sh\nprintf 'gh %s\\n' \"$*\" >> \"$JJFX_ACTION_CAPTURE\"\nexit 1\n",
    );
    let capture = temporary_directory.path().join("legacy-gh-repo-commands");
    let result = temporary_directory.path().join("legacy-gh-repo-result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "repository_action_helper"])
        .env(HELPER_MODE, "open-pr")
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env("PATH", path)
        .output()
        .expect("open-pr helper should run");

    assert!(
        output.status.success(),
        "open-pr helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reported = fs::read_to_string(&result).expect("open-pr result");
    assert!(
        reported.contains("origin is not a GitHub remote"),
        "the failure must reuse the message `dogfood/03` introduced: {reported}"
    );
    assert!(
        reported.contains("dogfood-site-origin"),
        "the failure must name the remote it refused: {reported}"
    );
    // The fake `gh` records every invocation, so its absence is proof the
    // refusal happened before the network was reached.
    assert!(
        !fs::read_to_string(&capture)
            .unwrap_or_default()
            .contains("gh "),
        "`gh` must not be contacted for a fabricated slug"
    );
}

/// A `gh_repo` the operator configured by hand is not the fabricated value, so a
/// non-GitHub `origin` must not refuse it: the slug must still reach `gh`.
#[test]
fn open_pull_request_still_reaches_gh_with_a_configured_gh_repo() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("configured-gh-repo.log");
    fs::write(&log, "{}\n").expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);
    jj_at(
        repository.root(),
        &["bookmark", "create", "owner/eng-301-action", "-r", "@"],
    );
    let origin = TempDir::new().expect("origin repository");
    install_local_origin(&repository, origin.path());

    let bin = temporary_directory.path().join("configured-gh-repo-bin");
    fs::create_dir(&bin).expect("fake command directory");
    write_executable(
        &bin.join("gh"),
        "#!/bin/sh\nprintf 'gh %s\\n' \"$*\" >> \"$JJFX_ACTION_CAPTURE\"\nexit 1\n",
    );
    let capture = temporary_directory
        .path()
        .join("configured-gh-repo-commands");
    let result = temporary_directory.path().join("configured-gh-repo-result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "repository_action_helper"])
        .env(HELPER_MODE, "open-pr")
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env("PATH", path)
        .output()
        .expect("open-pr helper should run");

    assert!(
        output.status.success(),
        "open-pr helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let captured = fs::read_to_string(&capture).expect("gh capture");
    assert!(
        captured.contains("gh -R owner/repo pr view owner/eng-301-action --web"),
        "a configured gh_repo must still reach `gh`: {captured}"
    );
    let reported = fs::read_to_string(&result).expect("open-pr result");
    assert!(
        !reported.contains("origin is not a GitHub remote"),
        "a configured gh_repo must not be refused: {reported}"
    );
}

#[test]
fn absorb_folds_working_copy_edits_into_their_base_commits_and_pushes() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("absorb.log");
    fs::write(&log, "{}\n").expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    let bin = temporary_directory.path().join("absorb-bin");
    fs::create_dir(&bin).expect("fake command directory");
    write_executable(
        &bin.join("jj"),
        &format!(
            "#!/bin/sh\nprintf 'jj %s\\n' \"$*\" >> \"$JJFX_ACTION_CAPTURE\"\ncase \" $* \" in\n{LANDING_JJ_ARMS}  *' --count '*) printf '1\\n' ;;\n  *' log '*) printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\\n' ;;\n  *' absorb '*) printf 'Absorbed changes into 2 revisions:\\n  aaa111 A: add x\\n  bbb222 B: bump y\\n' >&2 ;;\nesac\n"
        ),
    );
    let capture = temporary_directory.path().join("absorb-commands");
    let result = temporary_directory.path().join("absorb-result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "repository_action_helper"])
        .env(HELPER_MODE, "absorb")
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env("PATH", path)
        .output()
        .expect("absorb helper should run");

    assert!(
        output.status.success(),
        "absorb helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(&result).expect("absorb result"),
        "owner/eng-301-action\nabsorbed=2 remaining=false\n"
    );
    let commands = jj_operations(&fs::read_to_string(&capture).expect("command capture"));
    assert_eq!(commands.len(), 6, "{commands:?}");
    // The Ticket's bookmark is resolved by a read before anything is mutated, so
    // the prefixed name the agent created is what every later command uses.
    assert_eq!(
        commands[0],
        "bookmark list -T if(remote, \"\", name ++ \"\\n\")"
    );
    // The precondition is read before anything is mutated, so a repository with
    // no trunk is refused before absorb can run.
    assert_eq!(commands[1], "log -r trunk() ~ root() --count");
    // The mainline is read either side of the fold, so a rewrite of it is
    // caught rather than reported as success.
    assert!(
        commands[2].starts_with("log -r ") && commands[2].ends_with(" --no-graph -T commit_id"),
        "{commands:?}"
    );
    // The destination is pinned rather than left at jj's `mutable()` default.
    assert!(commands[3].starts_with("absorb --into "), "{commands:?}");
    assert_eq!(commands[4], commands[2], "the mainline is re-read");
    assert_eq!(commands[5], "git push --bookmark owner/eng-301-action");
}

/// A dependent Worker materialised on a Blocker's commit has that commit recorded
/// as its stack base, and `jjfx absorb` excludes it from the destination. Without
/// it the Blocker's commit - another Ticket's deliverable - is a legal fold
/// target, so this asserts the recorded base reaches the revset that pins it.
#[test]
fn absorb_excludes_the_recorded_stack_base_from_the_destination() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("absorb-stacked.log");
    fs::write(&log, "{}\n").expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    // The commit the dependent was stacked on, recorded when it was materialised.
    let blocker = "cccccccccccccccccccccccccccccccccccccccc";
    repository
        .state_store()
        .stack_bases()
        .record(&worker, vec![blocker.to_owned()])
        .expect("record the stack base");

    let bin = temporary_directory.path().join("absorb-stacked-bin");
    fs::create_dir(&bin).expect("fake command directory");
    write_executable(
        &bin.join("jj"),
        &format!(
            "#!/bin/sh\nprintf 'jj %s\\n' \"$*\" >> \"$JJFX_ACTION_CAPTURE\"\ncase \" $* \" in\n{LANDING_JJ_ARMS}  *' --count '*) printf '1\\n' ;;\n  *' log '*) printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\\n' ;;\n  *' absorb '*) printf 'Nothing changed.\\n' >&2 ;;\nesac\n"
        ),
    );
    let capture = temporary_directory.path().join("absorb-stacked-commands");
    let result = temporary_directory.path().join("absorb-stacked-result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "repository_action_helper"])
        .env(HELPER_MODE, "absorb")
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env("PATH", path)
        .output()
        .expect("absorb helper should run");

    assert!(
        output.status.success(),
        "absorb helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let commands = jj_operations(&fs::read_to_string(&capture).expect("command capture"));
    let destination = commands
        .iter()
        .find(|command| command.starts_with("absorb --into "))
        .expect("the absorb destination is pinned");
    assert!(
        destination.contains(&format!("~::({blocker})")),
        "the recorded stack base must be excluded from the destination: {destination}"
    );
}

#[test]
fn absorb_that_moves_nothing_skips_the_push() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("absorb-nothing.log");
    fs::write(&log, "{}\n").expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    let bin = temporary_directory.path().join("absorb-nothing-bin");
    fs::create_dir(&bin).expect("fake command directory");
    write_executable(
        &bin.join("jj"),
        &format!(
            "#!/bin/sh\nprintf 'jj %s\\n' \"$*\" >> \"$JJFX_ACTION_CAPTURE\"\ncase \" $* \" in\n{LANDING_JJ_ARMS}  *' --count '*) printf '1\\n' ;;\n  *' log '*) printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\\n' ;;\n  *' absorb '*) printf 'Nothing changed.\\n' >&2 ;;\nesac\n"
        ),
    );
    let capture = temporary_directory.path().join("absorb-nothing-commands");
    let result = temporary_directory.path().join("absorb-nothing-result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "repository_action_helper"])
        .env(HELPER_MODE, "absorb")
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env("PATH", path)
        .output()
        .expect("absorb helper should run");

    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(&result).expect("absorb result"),
        "owner/eng-301-action\nabsorbed=0 remaining=false\n"
    );
    let commands = jj_operations(&fs::read_to_string(&capture).expect("command capture"));
    assert_eq!(commands.len(), 5, "{commands:?}");
    assert_eq!(
        commands[0],
        "bookmark list -T if(remote, \"\", name ++ \"\\n\")"
    );
    assert!(commands[3].starts_with("absorb --into "), "{commands:?}");
    assert!(
        !commands
            .iter()
            .any(|command| command.starts_with("git push")),
        "a run that absorbs nothing must not push: {commands:?}"
    );
}

#[test]
fn absorb_reports_hunks_that_stayed_in_the_working_copy() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("absorb-remaining.log");
    fs::write(&log, "{}\n").expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    let bin = temporary_directory.path().join("absorb-remaining-bin");
    fs::create_dir(&bin).expect("fake command directory");
    write_executable(
        &bin.join("jj"),
        &format!(
            "#!/bin/sh\nprintf 'jj %s\\n' \"$*\" >> \"$JJFX_ACTION_CAPTURE\"\ncase \" $* \" in\n{LANDING_JJ_ARMS}  *' --count '*) printf '1\\n' ;;\n  *' log '*) printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\\n' ;;\n  *' absorb '*) printf 'Absorbed changes into 1 revisions:\\n  aaa111 A: add x\\nRemaining changes:\\nA new.rs\\n' >&2 ;;\nesac\n"
        ),
    );
    let capture = temporary_directory.path().join("absorb-remaining-commands");
    let result = temporary_directory.path().join("absorb-remaining-result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "repository_action_helper"])
        .env(HELPER_MODE, "absorb")
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env("PATH", path)
        .output()
        .expect("absorb helper should run");

    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(&result).expect("absorb result"),
        "owner/eng-301-action\nabsorbed=1 remaining=true\n"
    );
}

#[test]
fn failed_absorb_push_rolls_back_the_local_operation() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("absorb-failure.log");
    fs::write(&log, "{}\n").expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    let bin = temporary_directory.path().join("absorb-failure-bin");
    fs::create_dir(&bin).expect("fake command directory");
    write_executable(
        &bin.join("jj"),
        concat!(
            "#!/bin/sh\n",
            "printf 'jj %s\\n' \"$*\" >> \"$JJFX_ACTION_CAPTURE\"\n",
            "case \" $* \" in\n",
            "  *' bookmark list '*) printf 'owner/eng-301-action\\n' ;;\n",
            "  *' --count '*) printf '1\\n' ;;\n",
            "  *' log '*) printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\\n' ;;\n",
            "  *' absorb '*) printf 'Absorbed changes into 1 revisions:\\n' >&2 ;;\n",
            "  *' git push '*) echo rejected >&2; exit 1 ;;\n",
            "esac\n"
        ),
    );
    let capture = temporary_directory.path().join("absorb-failure-commands");
    let result = temporary_directory.path().join("absorb-failure-result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "repository_action_helper"])
        .env(HELPER_MODE, "absorb")
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env("PATH", path)
        .output()
        .expect("absorb helper should run");

    assert!(output.status.success());
    assert!(
        fs::read_to_string(&result)
            .expect("absorb result")
            .contains("cannot push absorbed branch"),
        "result should name the absorb push failure"
    );
    let commands = jj_operations(&fs::read_to_string(&capture).expect("command capture"));
    assert_eq!(commands.len(), 7, "{commands:?}");
    assert_eq!(
        commands[0],
        "bookmark list -T if(remote, \"\", name ++ \"\\n\")"
    );
    assert!(commands[3].starts_with("absorb --into "), "{commands:?}");
    assert_eq!(commands[5], "git push --bookmark owner/eng-301-action");
    assert_eq!(commands[6], "undo --allow-cross-workspace");
}

/// The repository `jjfx absorb` was observed to destroy: `origin` exists but no
/// `main@origin` has been fetched, so `trunk()` does not resolve, `immutable_heads()`
/// is empty, and jj calls every commit - `main` included - mutable. A Worker
/// whose change is already committed must not be folded into trunk: absorb
/// refuses, naming what is missing, and both commits are left exactly as they
/// were.
#[test]
fn absorb_refuses_when_trunk_does_not_resolve_and_leaves_main_alone() {
    let (temporary_directory, repository) = local_repository();
    let jj_config = temporary_directory.path().join("jj-config.toml");
    fs::write(
        &jj_config,
        "user.name = \"jjfx test\"\nuser.email = \"jjfx@example.com\"\n",
    )
    .expect("jj config");

    // `main` needs a real base commit first: a Worker's edit to existing lines
    // is what absorb would fold into a mutable main. A brand-new file has no
    // ancestor to attach to, so absorb leaves it alone whatever the destination.
    fs::write(
        repository.root().join("site.css"),
        "body { color: black; }\n",
    )
    .expect("base file");
    jj_with_config(
        repository.root(),
        &["describe", "-m", "Lantern static site"],
        &jj_config,
    );

    let worker = grow_one_worker(&repository);
    let log = repository.root().join("absorb-trunk.log");
    fs::write(&log, "{}\n").expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    // The bookmark the agent created for the Ticket: `absorb` resolves the
    // Worker's real bookmark before it mutates anything, so it must exist here.
    jj_with_config(
        repository.root(),
        &["bookmark", "create", "owner/eng-301-action", "-r", "main"],
        &jj_config,
    );

    let workspace = worker_workspace_path(&repository, &worker);
    // Dispatch bases a Worker on the mainline (`prepare_for_dispatch` runs
    // `jj new main`); `jj workspace add` alone leaves it on the root commit.
    jj_with_config(&workspace, &["new", "main"], &jj_config);

    // The Worker's deliverable, committed as `@`, exactly as the drive had it.
    fs::write(
        workspace.join("site.css"),
        "body { color: black; }\nform { margin: 1em; }\n",
    )
    .expect("Worker edit");
    jj_with_config(
        &workspace,
        &["describe", "-m", "contact-form: Add a contact form"],
        &jj_config,
    );

    assert!(
        !jj_trunk_resolves(&workspace, &jj_config),
        "the fixture must be the dangerous repository: no fetched mainline"
    );
    let main_before = jj_commit_id(repository.root(), "main", &jj_config);
    let worker_before = jj_commit_id(&workspace, "@", &jj_config);

    let result = temporary_directory.path().join("absorb-trunk-result");
    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "repository_action_helper"])
        .env(HELPER_MODE, "absorb")
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env("JJ_CONFIG", &jj_config)
        .output()
        .expect("absorb helper should run");

    assert!(
        output.status.success(),
        "absorb helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let outcome = fs::read_to_string(&result).expect("absorb result");
    assert!(
        outcome.contains("no resolvable trunk"),
        "the refusal must name the precondition: {outcome}"
    );
    assert!(
        outcome.contains("jj git fetch"),
        "the refusal must say how to get a trunk: {outcome}"
    );
    assert_eq!(
        jj_commit_id(repository.root(), "main", &jj_config),
        main_before,
        "`main` must not be rewritten"
    );
    assert_eq!(
        jj_commit_id(&workspace, "@", &jj_config),
        worker_before,
        "the Worker's commit must survive"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("site.css")).expect("Worker file"),
        "body { color: black; }\nform { margin: 1em; }\n",
        "the Worker's change must stay in its commit"
    );
}

/// The belt to the pinned destination's braces: if `jj absorb` rewrites the
/// mainline anyway - because a future jj ignores `--into`, say - the rewrite is
/// detected, undone, and reported as a failure rather than as success.
#[test]
fn an_absorb_that_rewrites_the_mainline_is_rolled_back_and_fails() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("absorb-rollback.log");
    fs::write(&log, "{}\n").expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    let bin = temporary_directory.path().join("absorb-rollback-bin");
    fs::create_dir(&bin).expect("fake command directory");
    write_executable(
        &bin.join("jj"),
        concat!(
            "#!/bin/sh\n",
            "printf 'jj %s\\n' \"$*\" >> \"$JJFX_ACTION_CAPTURE\"\n",
            "case \" $* \" in\n",
            "  *' bookmark list '*) printf 'owner/eng-301-action\\n' ;;\n",
            "  *' --count '*) printf '1\\n' ;;\n",
            "  *' absorb '*) printf 'Absorbed changes into 1 revisions:\\n' >&2 ;;\n",
            "  *' log '*) if [ -e \"$JJFX_ACTION_CAPTURE.mark\" ]; then\n",
            "      printf 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\\n'\n",
            "    else\n",
            "      printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\\n'\n",
            "      : > \"$JJFX_ACTION_CAPTURE.mark\"\n",
            "    fi ;;\n",
            "esac\n"
        ),
    );
    let capture = temporary_directory.path().join("absorb-rollback-commands");
    let result = temporary_directory.path().join("absorb-rollback-result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "repository_action_helper"])
        .env(HELPER_MODE, "absorb")
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env("PATH", path)
        .output()
        .expect("absorb helper should run");

    assert!(output.status.success());
    let outcome = fs::read_to_string(&result).expect("absorb result");
    assert!(
        outcome.contains("rewrote the mainline"),
        "a moved mainline must be reported, never success: {outcome}"
    );
    let commands = jj_operations(&fs::read_to_string(&capture).expect("command capture"));
    assert_eq!(commands.len(), 6, "{commands:?}");
    assert_eq!(
        commands[0],
        "bookmark list -T if(remote, \"\", name ++ \"\\n\")"
    );
    assert_eq!(commands[5], "undo --allow-cross-workspace");
    assert!(
        !commands
            .iter()
            .any(|command| command.starts_with("git push")),
        "a rolled-back absorb must not push: {commands:?}"
    );
}

#[test]
fn failed_rebase_push_rolls_back_the_local_operation() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("failed-rebase.log");
    fs::write(&log, "{}\n").expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    let bin = temporary_directory.path().join("bin");
    fs::create_dir(&bin).expect("fake command directory");
    write_executable(
        &bin.join("jj"),
        concat!(
            "#!/bin/sh\n",
            "printf 'jj %s\\n' \"$*\" >> \"$JJFX_ACTION_CAPTURE\"\n",
            "case \" $* \" in\n",
            "  *' bookmark list '*) printf 'owner/eng-301-action\\n' ;;\n",
            "  *' git push '*) echo rejected >&2; exit 1 ;;\n",
            "esac\n"
        ),
    );
    let capture = temporary_directory.path().join("commands");
    let result = temporary_directory.path().join("result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "repository_action_helper"])
        .env(HELPER_MODE, "rebase-failure")
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env("PATH", path)
        .output()
        .expect("repository action helper should run");

    assert!(output.status.success());
    assert!(
        fs::read_to_string(&result)
            .expect("action result")
            .contains("cannot push rebased branch")
    );
    assert_eq!(
        jj_operations(&fs::read_to_string(&capture).expect("command capture")),
        vec![
            "bookmark list -T if(remote, \"\", name ++ \"\\n\")",
            "rebase -b owner/eng-301-action -d main",
            "git push --bookmark owner/eng-301-action",
            "undo --allow-cross-workspace",
        ]
    );
}

/// The drive's failure shape, in reverse: when no bookmark names the Worker's
/// Ticket the verb must fail on a *read*, before it rebases the Workspace. The
/// old code rebased first and looked the bookmark up second, leaving a rebased
/// (and sometimes conflicted) Workspace behind a failure.
#[test]
fn a_missing_worker_bookmark_fails_before_the_workspace_is_touched() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("missing-bookmark.log");
    fs::write(&log, "{}\n").expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    let bin = temporary_directory.path().join("missing-bookmark-bin");
    fs::create_dir(&bin).expect("fake command directory");
    // A repository with no bookmark for the Ticket: `bookmark list` is empty.
    write_executable(
        &bin.join("jj"),
        "#!/bin/sh\nprintf 'jj %s\\n' \"$*\" >> \"$JJFX_ACTION_CAPTURE\"\n",
    );
    let capture = temporary_directory.path().join("missing-bookmark-commands");
    let result = temporary_directory.path().join("missing-bookmark-result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "repository_action_helper"])
        .env(HELPER_MODE, "missing-bookmark")
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env("PATH", path)
        .output()
        .expect("repository action helper should run");

    assert!(output.status.success());
    let outcome = fs::read_to_string(&result).expect("action result");
    assert!(outcome.contains("no bookmark names Worker"), "{outcome}");
    assert!(
        outcome.contains("ENG-301"),
        "the refusal names the Ticket: {outcome}"
    );
    assert_eq!(
        jj_operations(&fs::read_to_string(&capture).expect("command capture")),
        vec!["bookmark list -T if(remote, \"\", name ++ \"\\n\")"],
        "resolution is a read, so a missing bookmark must fail before any rebase"
    );
}

/// More than one bookmark naming the Ticket is the same hazard as none: choosing
/// one would be a guess, and the guess is what put the wrong name in a landing
/// verb. The verb must refuse on the read, before it mutates.
#[test]
fn an_ambiguous_worker_bookmark_fails_before_the_workspace_is_touched() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("ambiguous-bookmark.log");
    fs::write(&log, "{}\n").expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    let bin = temporary_directory.path().join("ambiguous-bookmark-bin");
    fs::create_dir(&bin).expect("fake command directory");
    write_executable(
        &bin.join("jj"),
        concat!(
            "#!/bin/sh\n",
            "printf 'jj %s\\n' \"$*\" >> \"$JJFX_ACTION_CAPTURE\"\n",
            "case \" $* \" in\n",
            "  *' bookmark list '*) printf 'owner/eng-301-action\\nowner/eng-301-other\\n' ;;\n",
            "esac\n"
        ),
    );
    let capture = temporary_directory
        .path()
        .join("ambiguous-bookmark-commands");
    let result = temporary_directory.path().join("ambiguous-bookmark-result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "repository_action_helper"])
        .env(HELPER_MODE, "missing-bookmark")
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env("PATH", path)
        .output()
        .expect("repository action helper should run");

    assert!(output.status.success());
    let outcome = fs::read_to_string(&result).expect("action result");
    assert!(
        outcome.contains("named by more than one bookmark"),
        "{outcome}"
    );
    assert!(outcome.contains("owner/eng-301-action"), "{outcome}");
    assert!(outcome.contains("owner/eng-301-other"), "{outcome}");
    assert_eq!(
        jj_operations(&fs::read_to_string(&capture).expect("command capture")),
        vec!["bookmark list -T if(remote, \"\", name ++ \"\\n\")"],
        "resolution is a read, so an ambiguous bookmark must fail before any rebase"
    );
}

/// `jj git push` exits zero and moves nothing when the name matches no bookmark,
/// so a verb must classify what the push did rather than trust the exit code.
/// Here the bookmark resolved and then vanished before the push (a concurrent
/// delete): nothing could have been pushed, so `absorb` must report the failure
/// and roll the local fold back, never claim it published.
#[test]
fn a_push_that_matched_no_bookmark_is_not_reported_as_success() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("absorb-vanished-push.log");
    fs::write(&log, "{}\n").expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    let bin = temporary_directory.path().join("absorb-vanished-push-bin");
    fs::create_dir(&bin).expect("fake command directory");
    write_executable(
        &bin.join("jj"),
        concat!(
            "#!/bin/sh\n",
            "printf 'jj %s\\n' \"$*\" >> \"$JJFX_ACTION_CAPTURE\"\n",
            "case \" $* \" in\n",
            // Resolution sees the bookmark; by the time the push is classified
            // it is gone, which is the race the classification has to survive.
            "  *' bookmark list '*)\n",
            "    if [ -e \"$JJFX_ACTION_CAPTURE.resolved\" ]; then :; else\n",
            "      printf 'owner/eng-301-action\\n'\n",
            "      : > \"$JJFX_ACTION_CAPTURE.resolved\"\n",
            "    fi ;;\n",
            "  *' --count '*) printf '1\\n' ;;\n",
            "  *' remote_bookmarks() '*) printf 'origin/owner/eng-301-action aaaa\\n' ;;\n",
            "  *' log '*) printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\\n' ;;\n",
            "  *' absorb '*) printf 'Absorbed changes into 1 revisions:\\n' >&2 ;;\n",
            "esac\n"
        ),
    );
    let capture = temporary_directory
        .path()
        .join("absorb-vanished-push-commands");
    let result = temporary_directory
        .path()
        .join("absorb-vanished-push-result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "repository_action_helper"])
        .env(HELPER_MODE, "absorb")
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env("PATH", path)
        .output()
        .expect("absorb helper should run");

    assert!(output.status.success());
    let outcome = fs::read_to_string(&result).expect("absorb result");
    assert!(
        outcome.contains("no bookmark named `owner/eng-301-action`"),
        "a push that matched no bookmark must not read as success: {outcome}"
    );
    let commands = jj_operations(&fs::read_to_string(&capture).expect("command capture"));
    assert!(
        commands
            .iter()
            .any(|command| command == "git push --bookmark owner/eng-301-action"),
        "{commands:?}"
    );
    assert_eq!(
        commands.last().map(String::as_str),
        Some("undo --allow-cross-workspace"),
        "an unpublished fold must be rolled back: {commands:?}"
    );
}

/// The other half of the same classification: the bookmark exists and is already
/// published, so the push has nothing to move. That is a no-op *success* - the
/// desired state already holds - so re-running `rebase` must report success and
/// must not undo the rebase it just did.
#[test]
fn re_running_rebase_on_an_already_pushed_worker_succeeds_without_rolling_back() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("rebase-already-pushed.log");
    fs::write(&log, "{}\n").expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    let bin = temporary_directory.path().join("rebase-already-pushed-bin");
    fs::create_dir(&bin).expect("fake command directory");
    write_executable(
        &bin.join("jj"),
        concat!(
            "#!/bin/sh\n",
            "printf 'jj %s\\n' \"$*\" >> \"$JJFX_ACTION_CAPTURE\"\n",
            "case \" $* \" in\n",
            "  *' bookmark list '*) printf 'owner/eng-301-action\\n' ;;\n",
            // The remote already carries the bookmark, and the push moves
            // nothing, so the snapshot is identical either side of it.
            "  *' remote_bookmarks() '*) printf 'origin/owner/eng-301-action aaaa\\n' ;;\n",
            "esac\n"
        ),
    );
    let capture = temporary_directory
        .path()
        .join("rebase-already-pushed-commands");
    let result = temporary_directory
        .path()
        .join("rebase-already-pushed-result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "repository_action_helper"])
        .env(HELPER_MODE, "rebase")
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env("PATH", path)
        .output()
        .expect("rebase helper should run");

    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(&result).expect("rebase result"),
        "rebased=owner/eng-301-action\n"
    );
    let commands = jj_operations(&fs::read_to_string(&capture).expect("command capture"));
    assert!(
        !commands.iter().any(|command| command.starts_with("undo")),
        "an already-published bookmark must not roll the rebase back: {commands:?}"
    );
}

/// The flow the review prompt prescribes for feedback that belongs in a base
/// commit: start an edit on top of the commit that should own the fix, then let
/// `jj absorb` fold the hunk into it. This drives real jj, so the guidance
/// cannot rot into something jj does not do.
#[test]
fn the_documented_base_commit_flow_lands_feedback_in_the_base_commit() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let run = |arguments: &[&str]| {
        let output = Command::new("jj")
            .args([
                "--config",
                "signing.behavior=drop",
                "--config",
                "user.name=jjfx test",
                "--config",
                "user.email=jjfx@example.com",
            ])
            .args(arguments)
            .current_dir(directory.path())
            .output()
            .expect("jj should be installed");
        assert!(
            output.status.success(),
            "jj {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };

    run(&["git", "init", "--no-colocate"]);
    fs::write(directory.path().join("a.rs"), "let a = 1;\n").expect("base file");
    fs::write(directory.path().join("b.rs"), "let b = 2;\n").expect("later file");
    run(&["describe", "-m", "A: add a and b"]);
    run(&["new", "-m", "B: bump b"]);
    fs::write(directory.path().join("b.rs"), "let b = 3;\n").expect("later edit");
    let base = run(&["log", "--no-graph", "-r", "@-", "-T", "change_id"]);
    let later = run(&["log", "--no-graph", "-r", "@", "-T", "change_id"]);

    // Review feedback: `a.rs` line 1 was introduced by the base commit, so the
    // author starts an edit on top of it rather than stacking a new commit.
    run(&["new", "@-", "-m", ""]);
    fs::write(directory.path().join("a.rs"), "let a = 0;\n").expect("feedback edit");
    run(&["absorb"]);

    assert_eq!(
        run(&["file", "show", "-r", &base, "a.rs"]),
        "let a = 0;",
        "the fix should land in the base commit"
    );
    assert_eq!(
        run(&["file", "show", "-r", &later, "b.rs"]),
        "let b = 3;",
        "the descendant commit should keep its own change"
    );
    let described = run(&[
        "log",
        "--no-graph",
        "-r",
        "all()",
        "-T",
        "description.first_line() ++ \"\\n\"",
    ]);
    assert_eq!(
        described.lines().filter(|line| !line.is_empty()).count(),
        2,
        "the edit commit should be abandoned, leaving only the two described commits: {described}"
    );
}

#[test]
fn mount_opens_a_resumable_provider_session_and_reports_the_tab() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("mount.log");
    fs::write(
        &log,
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"session-mount\"}\n",
    )
    .expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    let bin = temporary_directory.path().join("bin");
    fs::create_dir(&bin).expect("fake command directory");
    write_executable(
        &bin.join("claude"),
        "#!/bin/sh\necho --forward-subagent-text\n",
    );
    write_executable(
        &bin.join("kitten"),
        concat!(
            "#!/bin/sh\n",
            "printf 'kitten %s\\n' \"$*\" >> \"$JJFX_ACTION_CAPTURE\"\n",
            "case \"$*\" in\n",
            "  *--type=tab*) echo 42 ;;\n",
            "  *--location=vsplit*) echo 43 ;;\n",
            "esac\n"
        ),
    );
    let capture = temporary_directory.path().join("commands");
    let result = temporary_directory.path().join("result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "mount_action_helper"])
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env("KITTY_LISTEN_ON", "unix:/tmp/fake-kitty")
        .env("PATH", path)
        .output()
        .expect("Mount action helper should run");

    assert!(
        output.status.success(),
        "Mount action helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(&result).expect("Mount result"),
        "claude\nresumed:session-mount\n42\n"
    );
    let commands = fs::read_to_string(&capture).expect("command capture");
    assert!(commands.contains("@ --to=unix:/tmp/fake-kitty launch --type=tab"));
    assert!(commands.contains("clear; exec zsh"));
    assert!(commands.contains("claude --resume 'session-mount'; exec zsh"));
    assert!(commands.contains("--location=vsplit"));
    assert!(commands.contains("--bias=81"));
    assert!(commands.contains("--location=hsplit"));
    assert!(commands.contains("--bias=50"));
    assert!(commands.contains("focus-window --match id:43"));
}

#[test]
fn mount_runs_the_configured_command_in_the_first_pane() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("mount-pane.log");
    fs::write(
        &log,
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"session-mount\"}\n",
    )
    .expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    let bin = temporary_directory.path().join("bin");
    fs::create_dir(&bin).expect("fake command directory");
    write_executable(
        &bin.join("claude"),
        "#!/bin/sh\necho --forward-subagent-text\n",
    );
    write_executable(
        &bin.join("kitten"),
        concat!(
            "#!/bin/sh\n",
            "printf 'kitten %s\\n' \"$*\" >> \"$JJFX_ACTION_CAPTURE\"\n",
            "case \"$*\" in\n",
            "  *--type=tab*) echo 42 ;;\n",
            "  *--location=vsplit*) echo 43 ;;\n",
            "esac\n"
        ),
    );
    let capture = temporary_directory.path().join("commands");
    let result = temporary_directory.path().join("result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "mount_action_helper"])
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env(HELPER_PANE_COMMAND, "dev-server")
        .env("KITTY_LISTEN_ON", "unix:/tmp/fake-kitty")
        .env("PATH", path)
        .output()
        .expect("Mount action helper should run");

    assert!(
        output.status.success(),
        "Mount action helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let commands = fs::read_to_string(&capture).expect("command capture");
    // The configured command rides on the tab launch (the first, top-left
    // pane); the second left pane and the agent pane are unaffected.
    let tab_launch = commands
        .lines()
        .find(|line| line.contains("--type=tab"))
        .expect("tab launch");
    assert!(tab_launch.contains("dev-server"), "{tab_launch}");
    assert!(!tab_launch.contains("clear; exec zsh"), "{tab_launch}");
    let pane_split = commands
        .lines()
        .find(|line| line.contains("--location=hsplit"))
        .expect("pane split");
    assert!(pane_split.contains("clear; exec zsh"), "{pane_split}");
    assert!(commands.contains("claude --resume 'session-mount'; exec zsh"));
}

#[test]
fn mount_builds_the_configured_layout_from_the_plan() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("mount-layout.log");
    fs::write(
        &log,
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"session-layout\"}\n",
    )
    .expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    let bin = temporary_directory.path().join("layout-bin");
    fs::create_dir(&bin).expect("fake command directory");
    write_executable(
        &bin.join("claude"),
        "#!/bin/sh\necho --forward-subagent-text\n",
    );
    write_executable(
        &bin.join("kitten"),
        concat!(
            "#!/bin/sh\n",
            "printf 'kitten %s\\n' \"$*\" >> \"$JJFX_ACTION_CAPTURE\"\n",
            "case \"$*\" in\n",
            "  *--type=tab*) echo 42 ;;\n",
            "  *--location=vsplit*) echo 43 ;;\n",
            "  *--location=hsplit*) echo 44 ;;\n",
            "esac\n"
        ),
    );
    let capture = temporary_directory.path().join("layout-commands");
    let result = temporary_directory.path().join("layout-result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "mount_action_helper"])
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env(
            HELPER_LAYOUT,
            concat!(
                "[[panes]]\n",
                "role = \"first\"\n",
                "[[panes]]\n",
                "role = \"agent\"\n",
                "location = \"vsplit\"\n",
                "bias = 70\n",
                "[[panes]]\n",
                "role = \"shell\"\n",
                "location = \"hsplit\"\n",
            ),
        )
        .env("KITTY_LISTEN_ON", "unix:/tmp/fake-kitty")
        .env("PATH", path)
        .output()
        .expect("Mount action helper should run");

    assert!(
        output.status.success(),
        "Mount action helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let commands = fs::read_to_string(&capture).expect("command capture");
    assert!(commands.contains("--bias=70"), "{commands}");
    assert!(!commands.contains("--bias=81"), "{commands}");
    // The plan anchors the shell pane to the tab window, not the agent, so the
    // left column stays a column when the bias changes.
    let shell_split = commands
        .lines()
        .find(|line| line.contains("--location=hsplit"))
        .expect("shell split");
    assert!(
        shell_split.contains("--match window_id:42"),
        "{shell_split}"
    );
    assert!(shell_split.contains("--next-to id:42"), "{shell_split}");
    // Focus still lands on the agent pane from the plan.
    assert!(
        commands.contains("focus-window --match id:43"),
        "{commands}"
    );
}

#[test]
fn mount_honors_a_single_pane_layout() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("mount-single.log");
    fs::write(
        &log,
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"session-single\"}\n",
    )
    .expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    let bin = temporary_directory.path().join("single-bin");
    fs::create_dir(&bin).expect("fake command directory");
    write_executable(
        &bin.join("claude"),
        "#!/bin/sh\necho --forward-subagent-text\n",
    );
    write_executable(
        &bin.join("kitten"),
        concat!(
            "#!/bin/sh\n",
            "printf 'kitten %s\\n' \"$*\" >> \"$JJFX_ACTION_CAPTURE\"\n",
            "case \"$*\" in\n",
            "  *--type=tab*) echo 42 ;;\n",
            "esac\n"
        ),
    );
    let capture = temporary_directory.path().join("single-commands");
    let result = temporary_directory.path().join("single-result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "mount_action_helper"])
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env(HELPER_LAYOUT, "[[panes]]\nrole = \"agent\"\n")
        .env("KITTY_LISTEN_ON", "unix:/tmp/fake-kitty")
        .env("PATH", path)
        .output()
        .expect("Mount action helper should run");

    assert!(
        output.status.success(),
        "Mount action helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let commands = fs::read_to_string(&capture).expect("command capture");
    // A single-pane plan launches the agent in the tab itself: no splits layout
    // to force and no split launches.
    let tab_launch = commands
        .lines()
        .find(|line| line.contains("--type=tab"))
        .expect("tab launch");
    assert!(
        tab_launch.contains("claude --resume 'session-single'"),
        "{tab_launch}"
    );
    assert!(!commands.contains("goto-layout"), "{commands}");
    assert!(!commands.contains("--location"), "{commands}");
    assert!(
        commands.contains("focus-window --match id:42"),
        "{commands}"
    );
}

#[test]
fn mount_rejects_a_missing_worker_workspace() {
    let (_temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    fs::remove_dir_all(worker_workspace_path(&repository, &worker))
        .expect("remove Worker Workspace");

    let error = WorkerActions::new(repository)
        .mount(&worker)
        .expect_err("Mount without a Workspace should fail");

    assert!(error.to_string().contains("Workspace directory is missing"));
}

#[test]
fn reset_reports_a_missing_workspace_without_hiding_released_capacity() {
    let (_temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let workspace = worker_workspace_path(&repository, &worker);
    fs::remove_dir_all(workspace).expect("remove Worker Workspace");

    let outcome = WorkerActions::new(repository)
        .reset(&worker)
        .expect("Reset should succeed");

    assert_eq!(outcome.run(), RunReset::AlreadyIdle);
    assert!(matches!(
        outcome.restoration(),
        WorkspaceRestoration::SkippedMissingWorkspace
    ));
}

#[test]
fn reset_returns_a_handle_for_successful_workspace_restoration() {
    let (_temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);

    let restoration = WorkerActions::new(repository)
        .reset(&worker)
        .expect("Reset should succeed")
        .into_restoration();

    let WorkspaceRestoration::Pending(handle) = restoration else {
        panic!("existing Workspace should be restored asynchronously");
    };
    handle.wait().expect("Workspace restoration should succeed");
}

#[test]
fn reset_handle_reports_workspace_command_failure() {
    let (_temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let workspace = worker_workspace_path(&repository, &worker);
    fs::remove_dir_all(workspace.join(".jj")).expect("break Worker Workspace marker");

    let restoration = WorkerActions::new(repository)
        .reset(&worker)
        .expect("Run cleanup should still succeed")
        .into_restoration();
    let WorkspaceRestoration::Pending(handle) = restoration else {
        panic!("existing Workspace should attempt restoration");
    };
    let error = handle.wait().expect_err("broken Workspace should fail");
    assert!(error.to_string().contains("restore Workspace"));
}

#[test]
fn review_rejects_a_worker_without_a_branch_before_launch() {
    let (_temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);

    let error = WorkerActions::new(repository)
        .review(&worker, RunMode::Background)
        .expect_err("review without a branch should fail");

    assert!(error.to_string().contains("has no branch"));
}

#[test]
fn review_builds_one_provider_neutral_follow_up_from_pr_state() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let prior_log = repository.root().join("review-prior.log");
    fs::write(
        &prior_log,
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"review-session\"}\n",
    )
    .expect("prior Session log");
    set_terminal_worker(&repository, &worker, &prior_log);
    let bin = temporary_directory.path().join("review-bin");
    fs::create_dir(&bin).expect("fake executable directory");
    let captured = temporary_directory.path().join("review-prompt");
    // Review resolves the Worker's bookmark the same way the landing verbs do,
    // so the fake `jj` answers the resolution read.
    write_executable(
        &bin.join("jj"),
        "#!/bin/sh\ncase \" $* \" in\n  *' bookmark list '*) printf 'owner/eng-301-action\\n' ;;\nesac\n",
    );
    write_executable(
        &bin.join("gh"),
        "#!/bin/sh\ncase \"$*\" in\n  *\"pr list\"*) printf '%s\\n' '[{\"number\":42,\"url\":\"https://example/pr/42\",\"headRefName\":\"owner/eng-301-action\",\"mergeable\":\"CONFLICTING\",\"reviewDecision\":\"CHANGES_REQUESTED\"}]' ;;\n  *\"pr checks\"*) printf '%s\\n' '[{\"bucket\":\"fail\",\"name\":\"tests\",\"state\":\"FAILURE\"},{\"bucket\":\"pass\",\"name\":\"lint\",\"state\":\"SUCCESS\"}]'; exit 1 ;;\nesac\n",
    );
    write_executable(
        &bin.join("claude"),
        &format!(
            "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf '%s\\n' \"$@\" > {}\nprintf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}}'\n",
            captured.display()
        ),
    );
    let result = temporary_directory.path().join("review-result");
    let path = env::join_paths([
        bin.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("review PATH");
    let output = Command::new(env::current_exe().expect("test executable"))
        .args(["--exact", "review_action_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .output()
        .expect("Review helper should run");
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("Review result"),
        "session=resumed:review-session; completed=true"
    );
    let prompt = fs::read_to_string(captured).expect("captured Review prompt");
    assert!(prompt.contains("Current review state: changes requested"));
    assert!(prompt.contains("has merge conflicts"));
    assert!(prompt.contains("tests"));
    assert!(!prompt.contains("   - lint"));
}

/// A `gh` whose `pr checks` output jjfx cannot read must fail by naming the
/// command and quoting gh's own output, never by surfacing a bare serde error -
/// otherwise a field rename is invisible behind "missing field".
#[test]
fn review_fails_loudly_when_gh_returns_an_unrecognised_checks_shape() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let prior_log = repository.root().join("review-unreadable-prior.log");
    fs::write(
        &prior_log,
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"review-session\"}\n",
    )
    .expect("prior Session log");
    set_terminal_worker(&repository, &worker, &prior_log);
    let bin = temporary_directory.path().join("review-unreadable-bin");
    fs::create_dir(&bin).expect("fake executable directory");
    write_executable(
        &bin.join("jj"),
        "#!/bin/sh\ncase \" $* \" in\n  *' bookmark list '*) printf 'owner/eng-301-action\\n' ;;\nesac\n",
    );
    // A shape with none of the outcome fields gh has used.
    write_executable(
        &bin.join("gh"),
        "#!/bin/sh\ncase \"$*\" in\n  *\"pr list\"*) printf '%s\\n' '[{\"number\":42,\"url\":\"https://example/pr/42\",\"headRefName\":\"owner/eng-301-action\",\"mergeable\":\"MERGEABLE\",\"reviewDecision\":\"REVIEW_REQUIRED\"}]' ;;\n  *\"pr checks\"*) printf '%s\\n' '[{\"name\":\"tests\"}]' ;;\nesac\n",
    );
    let result = temporary_directory.path().join("review-unreadable-result");
    let path = env::join_paths([
        bin.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("review PATH");
    let output = Command::new(env::current_exe().expect("test executable"))
        .args(["--exact", "failed_review_action_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .output()
        .expect("failed Review helper should run");
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reported = fs::read_to_string(&result).expect("failed Review result");
    assert!(
        reported.contains("gh -R owner/repo pr checks 42 --json name,bucket,state"),
        "the failure must name the command: {reported}"
    );
    assert!(
        reported.contains(r#"[{"name":"tests"}]"#),
        "the failure must quote gh's own output: {reported}"
    );
}

#[test]
fn send_resumes_the_prior_claude_session_and_reports_it() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let prior_log = repository
        .root()
        .join(".jj/pools/default")
        .join(format!("{worker}.log"));
    fs::write(
        &prior_log,
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"session-301\"}\n",
    )
    .expect("prior Session log");
    set_terminal_worker(&repository, &worker, &prior_log);

    let bin = temporary_directory.path().join("bin");
    fs::create_dir(&bin).expect("fake executable directory");
    let captured = temporary_directory.path().join("captured-args");
    write_executable(
        &bin.join("claude"),
        &format!(
            "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf '%s\\n' \"$@\" > {}\nprintf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}}'\n",
            captured.display()
        ),
    );
    let result = temporary_directory.path().join("result");
    let path = env::join_paths([
        bin.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let output = Command::new(env::current_exe().expect("test executable"))
        .args(["--exact", "send_action_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .output()
        .expect("Send helper should run");
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("Send result"),
        "runtime=claude; session=resumed:session-301; completed=true"
    );
    let args = fs::read_to_string(captured).expect("captured Claude arguments");
    assert!(args.contains("--resume\nsession-301\n--fork-session"));
}

#[test]
fn send_on_an_idle_worker_starts_fresh_and_reports_the_reason() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let bin = temporary_directory.path().join("bin");
    fs::create_dir(&bin).expect("fake executable directory");
    let captured = temporary_directory.path().join("captured-fresh-args");
    write_executable(
        &bin.join("claude"),
        &format!(
            "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf '%s\\n' \"$@\" > {}\nprintf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}}'\n",
            captured.display()
        ),
    );
    let result = temporary_directory.path().join("fresh-result");
    let path = env::join_paths([
        bin.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let output = Command::new(env::current_exe().expect("test executable"))
        .args(["--exact", "send_action_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .output()
        .expect("fresh Send helper should run");
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("fresh Send result"),
        "runtime=claude; session=fresh:no prior session log; completed=true"
    );
    let args = fs::read_to_string(captured).expect("captured fresh Claude arguments");
    assert!(args.contains("--append-system-prompt"));
    assert!(!args.contains("--resume"));
}

#[test]
fn failed_send_launch_restores_the_prior_terminal_worker() {
    let (_temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let prior_log = repository.root().join("prior.log");
    fs::write(
        &prior_log,
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"session-rollback\"}\n",
    )
    .expect("prior Session log");
    set_terminal_worker(&repository, &worker, &prior_log);

    let result = repository.root().join("failed-send-result");
    let path = env::join_paths([
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let output = Command::new(env::current_exe().expect("test executable"))
        .args(["--exact", "failed_send_action_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .output()
        .expect("failed Send helper should run");
    assert!(output.status.success());
    assert!(
        fs::read_to_string(result)
            .expect("failed Send result")
            .contains("claude")
    );

    let snapshot = repository.worker_pool().snapshot();
    let restored = snapshot.worker(worker.as_str()).expect("restored Worker");
    assert_eq!(restored.status(), jjfx::engine::WorkerStatus::Done);
    assert_eq!(restored.ticket(), Some("ENG-301"));
    assert_eq!(restored.branch_name(), Some("eng-301"));
    assert_eq!(
        restored.log_file(),
        Some(prior_log.to_string_lossy().as_ref())
    );
}

#[test]
#[ignore]
fn background_send_action_helper() {
    let repository =
        Repository::open(env::var_os(HELPER_REPOSITORY).expect("repository")).expect("repository");
    let worker = WorkerId::parse(env::var(HELPER_WORKER).expect("Worker ID")).expect("Worker ID");
    let provider = env::var(HELPER_PROVIDER).expect("provider");
    let model = env::var(HELPER_MODEL).expect("model");
    let outcome = WorkerActions::new(repository)
        .with_model(AgentModel::new(model).with_provider(provider))
        .send(&worker, "continue the work", RunMode::Background)
        .expect("background Follow-up should launch");
    let FollowUpExecution::Background(run) = outcome.into_execution() else {
        panic!("background action should return a waiter");
    };
    fs::write(
        env::var_os(HELPER_RESULT).expect("result path"),
        run.pid().to_string(),
    )
    .expect("background PID");
    let completed = run.wait().expect("background Follow-up should be reaped");
    fs::write(
        env::var_os(HELPER_EXIT).expect("exit path"),
        format!("exit={:?}", completed.exit_code()),
    )
    .expect("background waiter outcome");
}

#[test]
#[ignore]
fn failed_mount_action_helper() {
    let repository =
        Repository::open(env::var_os(HELPER_REPOSITORY).expect("repository")).expect("repository");
    let worker = WorkerId::parse(env::var(HELPER_WORKER).expect("Worker ID")).expect("Worker ID");
    let error = WorkerActions::new(repository)
        .mount(&worker)
        .expect_err("Mount without a model should fail");
    fs::write(
        env::var_os(HELPER_RESULT).expect("result path"),
        error.to_string(),
    )
    .expect("failed Mount result");
}

#[test]
#[ignore]
fn mount_action_helper() {
    let repository =
        Repository::open(env::var_os(HELPER_REPOSITORY).expect("repository")).expect("repository");
    let worker = WorkerId::parse(env::var(HELPER_WORKER).expect("Worker ID")).expect("Worker ID");
    let mut actions = WorkerActions::new(repository);
    if let (Ok(provider), Ok(model)) = (env::var(HELPER_PROVIDER), env::var(HELPER_MODEL)) {
        actions = actions.with_model(AgentModel::new(model).with_provider(provider));
    }
    if let Ok(command) = env::var(HELPER_PANE_COMMAND) {
        actions = actions.with_first_pane_command(vec![command]);
    }
    if let Ok(layout) = env::var(HELPER_LAYOUT) {
        let layout: PaneLayout = toml::from_str(&layout).expect("layout TOML should parse");
        actions = actions.with_layout(layout);
    }
    let outcome = actions.mount(&worker).expect("Mount should succeed");
    let session = match outcome.session() {
        AgentSessionResolution::Resumed { session_id } => format!("resumed:{session_id}"),
        AgentSessionResolution::Fresh { reason } => format!("fresh:{reason}"),
    };
    fs::write(
        env::var_os(HELPER_RESULT).expect("result path"),
        format!(
            "{}\n{}\n{}\n",
            outcome.runtime().as_str(),
            session,
            outcome.tab_id()
        ),
    )
    .expect("Mount result");
}

#[test]
#[ignore]
fn repository_action_helper() {
    let mode = env::var(HELPER_MODE).unwrap_or_default();
    if mode != "repository"
        && mode != "rebase-failure"
        && mode != "rebase"
        && mode != "absorb"
        && mode != "open-pr"
        && mode != "missing-bookmark"
    {
        return;
    }
    let repository =
        Repository::open(env::var_os(HELPER_REPOSITORY).expect("repository")).expect("repository");
    let worker = WorkerId::parse(env::var(HELPER_WORKER).expect("Worker ID")).expect("Worker ID");
    let actions = WorkerActions::new(repository);
    let result = if mode == "repository" {
        let rebase = actions.rebase(&worker).expect("Rebase should succeed");
        let open = actions
            .open_pull_request(&worker)
            .expect("Open PR should succeed");
        format!("{}\n{}\n", rebase.branch(), open.branch())
    } else if mode == "rebase" {
        match actions.rebase(&worker) {
            Ok(rebase) => format!("rebased={}\n", rebase.branch()),
            Err(error) => error.to_string(),
        }
    } else if mode == "open-pr" {
        match actions.open_pull_request(&worker) {
            Ok(open) => format!("opened={}\n", open.branch()),
            Err(error) => error.to_string(),
        }
    } else if mode == "absorb" {
        match actions.absorb(&worker) {
            Ok(absorb) => {
                let report = absorb.report();
                format!(
                    "{}\nabsorbed={} remaining={}\n",
                    absorb.branch(),
                    report.revisions(),
                    report.has_remaining_changes()
                )
            }
            Err(error) => error.to_string(),
        }
    } else {
        actions
            .rebase(&worker)
            .expect_err("Rebase should fail")
            .to_string()
    };
    fs::write(env::var_os(HELPER_RESULT).expect("result path"), result).expect("action result");
}

#[test]
#[ignore]
fn review_action_helper() {
    let repository =
        Repository::open(env::var_os(HELPER_REPOSITORY).expect("repository")).expect("repository");
    let worker = WorkerId::parse(env::var(HELPER_WORKER).expect("Worker ID")).expect("Worker ID");
    let outcome = WorkerActions::new(repository)
        .review(&worker, RunMode::Foreground)
        .expect("Review should launch");
    let session = match outcome.session() {
        AgentSessionResolution::Resumed { session_id } => format!("resumed:{session_id}"),
        AgentSessionResolution::Fresh { reason } => format!("fresh:{reason}"),
    };
    let completed = matches!(outcome.execution(), FollowUpExecution::Foreground(_));
    fs::write(
        env::var_os(HELPER_RESULT).expect("result path"),
        format!("session={session}; completed={completed}"),
    )
    .expect("Review result");
}

#[test]
#[ignore]
fn failed_review_action_helper() {
    let repository =
        Repository::open(env::var_os(HELPER_REPOSITORY).expect("repository")).expect("repository");
    let worker = WorkerId::parse(env::var(HELPER_WORKER).expect("Worker ID")).expect("Worker ID");
    let error = WorkerActions::new(repository)
        .review(&worker, RunMode::Foreground)
        .expect_err("an unreadable checks body should fail");
    fs::write(
        env::var_os(HELPER_RESULT).expect("result path"),
        error.to_string(),
    )
    .expect("failed Review result");
}

#[test]
#[ignore]
fn failed_send_action_helper() {
    let repository =
        Repository::open(env::var_os(HELPER_REPOSITORY).expect("repository")).expect("repository");
    let worker = WorkerId::parse(env::var(HELPER_WORKER).expect("Worker ID")).expect("Worker ID");
    let mut actions = WorkerActions::new(repository);
    if let (Ok(provider), Ok(model)) = (env::var(HELPER_PROVIDER), env::var(HELPER_MODEL)) {
        actions = actions.with_model(AgentModel::new(model).with_provider(provider));
    }
    let error = actions
        .send(&worker, "continue", RunMode::Background)
        .expect_err("invalid runtime profile should fail");
    fs::write(
        env::var_os(HELPER_RESULT).expect("result path"),
        error.to_string(),
    )
    .expect("failed Send result");
}

#[test]
#[ignore]
fn send_any_action_helper() {
    let repository =
        Repository::open(env::var_os(HELPER_REPOSITORY).expect("repository")).expect("repository");
    let outcome = WorkerActions::new(repository)
        .send_any("continue the work", RunMode::Foreground)
        .expect("Send-any should launch");
    let completed = matches!(outcome.execution(), FollowUpExecution::Foreground(_));
    fs::write(
        env::var_os(HELPER_RESULT).expect("result path"),
        format!("worker={}; completed={completed}", outcome.worker()),
    )
    .expect("Send-any result");
}

#[test]
#[ignore]
fn send_action_helper() {
    let repository =
        Repository::open(env::var_os(HELPER_REPOSITORY).expect("repository")).expect("repository");
    let worker = WorkerId::parse(env::var(HELPER_WORKER).expect("Worker ID")).expect("Worker ID");
    let mut actions = WorkerActions::new(repository);
    if let (Ok(provider), Ok(model)) = (env::var(HELPER_PROVIDER), env::var(HELPER_MODEL)) {
        actions = actions.with_model(AgentModel::new(model).with_provider(provider));
    }
    let outcome = actions
        .send(&worker, "continue the work", RunMode::Foreground)
        .expect("Send should launch");
    let session = match outcome.session() {
        AgentSessionResolution::Resumed { session_id } => format!("resumed:{session_id}"),
        AgentSessionResolution::Fresh { reason } => format!("fresh:{reason}"),
    };
    let completed = matches!(outcome.execution(), FollowUpExecution::Foreground(_));
    fs::write(
        env::var_os(HELPER_RESULT).expect("result path"),
        format!(
            "runtime={}; session={session}; completed={completed}",
            outcome.runtime().as_str()
        ),
    )
    .expect("Send result");
}

fn worker_workspace_path(repository: &Repository, worker: &WorkerId) -> PathBuf {
    let name = repository
        .root()
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("repo");
    repository
        .root()
        .parent()
        .unwrap_or(repository.root())
        .join(format!("{name}-workspaces"))
        .join(worker.as_str())
}

fn grow_one_worker(repository: &Repository) -> WorkerId {
    repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("Worker Workspace should be provisioned")
        .added_workers()[0]
        .clone()
}

/// Run one jj command against the fixture repository, asserting success.
fn jj_at(root: &Path, args: &[&str]) {
    let output = Command::new("jj")
        .args(["--config", "signing.behavior=drop"])
        .args(args)
        .current_dir(root)
        .output()
        .expect("jj should be installed");
    assert!(
        output.status.success(),
        "jj {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Re-point `origin` at a real repository on disk - the shape the dogfood
/// drive's remote had, whose last two path segments (`personal/dogfood-site-origin`)
/// are not a GitHub identity.
fn install_local_origin(repository: &Repository, root: &Path) {
    let origin = root.join("personal").join("dogfood-site-origin.git");
    fs::create_dir_all(origin.parent().expect("origin parent")).expect("origin parent directory");
    jj_at(
        root,
        &["git", "init", "--colocate", &origin.to_string_lossy()],
    );
    jj_at(repository.root(), &["git", "remote", "remove", "origin"]);
    jj_at(
        repository.root(),
        &["git", "remote", "add", "origin", &origin.to_string_lossy()],
    );
}

/// Leave the Repository in the state a Pool created before `dogfood/03` is in:
/// `origin` is a filesystem path, and the Pool still carries the slug the old
/// derivation mined from it.
fn install_legacy_fabricated_gh_repo(repository: &Repository, root: &Path) {
    install_local_origin(repository, root);
    let state = repository.state_store().pool();
    let Loaded::Present(versioned) = state.load().expect("load Pool") else {
        panic!("Pool state should exist");
    };
    let (mut current, revision) = versioned.into_parts();
    current.gh_repo = "personal/dogfood-site-origin".to_owned();
    let outcome = state
        .commit(Expected::Match(revision), StateChange::Replace(current))
        .expect("rewrite the Pool gh_repo");
    assert!(matches!(outcome, jjfx::engine::CommitOutcome::Applied(_)));
}

/// The `jj` subcommands a fake-`jj` capture recorded, with the global flags
/// jjfx adds (`--config signing.behavior=drop`, `--ignore-working-copy`)
/// stripped, so an assertion names the operation rather than the environment it
/// ran in.
fn jj_subcommands(capture: &str) -> Vec<String> {
    capture
        .lines()
        .map(|line| {
            let mut rest = line.strip_prefix("jj ").unwrap_or(line).trim();
            while let Some(tail) = rest
                .strip_prefix("--ignore-working-copy ")
                .or_else(|| rest.strip_prefix("--config signing.behavior=drop "))
            {
                rest = tail;
            }
            rest.to_owned()
        })
        .collect()
}

/// The `jj` operations a capture recorded, without the remote-bookmark reads the
/// push classification makes: an assertion should name the operation, not the
/// probe that decides whether it pushed.
fn jj_operations(capture: &str) -> Vec<String> {
    jj_subcommands(capture)
        .into_iter()
        .filter(|command| !command.starts_with("log -r remote_bookmarks()"))
        .collect()
}

/// The fake-`jj` arms every landing verb needs: the bookmark the Worker's Ticket
/// actually carries - prefixed and described, unlike the bare id Worker state
/// records - and a push that moves a remote bookmark so the classified outcome
/// is `Pushed`. The `$JJFX_ACTION_CAPTURE.mark` barrier flips the remote
/// snapshot after the push, so the before/after comparison sees a real change.
const LANDING_JJ_ARMS: &str = concat!(
    "  *' bookmark list '*) printf 'owner/eng-301-action\\n' ;;\n",
    "  *' remote_bookmarks() '*) if [ -e \"$JJFX_ACTION_CAPTURE.mark\" ]; then\n",
    "      printf 'origin/owner/eng-301-action bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\\n'\n",
    "    else\n",
    "      printf 'origin/owner/eng-301-action aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\\n'\n",
    "    fi ;;\n",
    "  *' git push '*) : > \"$JJFX_ACTION_CAPTURE.mark\" ;;\n",
);

/// Run one jj command against a fixture repository with its own `JJ_CONFIG`,
/// asserting success.
fn jj_with_config(dir: &Path, args: &[&str], config: &Path) {
    let output = Command::new("jj")
        .args(["--config", "signing.behavior=drop"])
        .args(args)
        .current_dir(dir)
        .env("JJ_CONFIG", config)
        .output()
        .expect("jj should be installed");
    assert!(
        output.status.success(),
        "jj {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The commit id `revset` resolves to in `dir`, read against `config`.
fn jj_commit_id(dir: &Path, revset: &str, config: &Path) -> String {
    let output = Command::new("jj")
        .args([
            "--ignore-working-copy",
            "log",
            "-r",
            revset,
            "--no-graph",
            "-T",
            "commit_id",
        ])
        .current_dir(dir)
        .env("JJ_CONFIG", config)
        .output()
        .expect("jj should be installed");
    assert!(
        output.status.success(),
        "jj log -r {revset} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Whether jj's `trunk()` resolves in `dir`: a fetched mainline, not the root
/// commit it degrades to when there is none.
fn jj_trunk_resolves(dir: &Path, config: &Path) -> bool {
    let output = Command::new("jj")
        .args([
            "--ignore-working-copy",
            "log",
            "-r",
            "trunk() ~ root()",
            "--count",
        ])
        .current_dir(dir)
        .env("JJ_CONFIG", config)
        .output()
        .expect("jj should be installed");
    assert!(
        output.status.success(),
        "jj log -r 'trunk() ~ root()' failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim() != "0"
}

fn set_terminal_worker(repository: &Repository, worker: &WorkerId, prior_log: &Path) {
    set_terminal_worker_for_runtime(repository, worker, prior_log, AgentRuntime::Claude);
}

fn set_terminal_worker_for_runtime(
    repository: &Repository,
    worker: &WorkerId,
    prior_log: &Path,
    runtime: AgentRuntime,
) {
    let state_repository = repository.state_store().worker(worker.clone());
    let loaded = match state_repository.load().expect("Worker state") {
        Loaded::Present(versioned) => versioned,
        Loaded::Missing => panic!("Worker state should exist"),
    };
    let (mut state, revision) = loaded.into_parts();
    state.status = WireStatus::new("done");
    state.agent = Some(WireAgent::new(runtime.as_str()));
    state.ticket = Some("ENG-301".to_owned());
    state.started_at = Some(WireTimestamp::new("2026-07-31T10:00:00Z"));
    state.completed_at = Some(WireTimestamp::new("2026-07-31T10:05:00Z"));
    state.log_file = Some(prior_log.to_string_lossy().into_owned());
    // Worker state records the bare Ticket id, exactly as reservation does
    // (`branch_name: ticket.to_lowercase()`); the bookmark the agent actually
    // created carries the operator's prefix and a description the state never
    // learns, so every landing verb has to resolve it.
    state.branch_name = Some("eng-301".to_owned());
    state.exit_code = Some(0);
    let outcome = state_repository
        .commit(Expected::Match(revision), StateChange::Replace(state))
        .expect("terminal Worker state");
    assert!(matches!(outcome, jjfx::engine::CommitOutcome::Applied(_)));
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("fake executable");
    let mut permissions = fs::metadata(path)
        .expect("fake executable metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("fake executable permissions");
}

fn local_repository() -> (TempDir, Repository) {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let output = Command::new("jj")
        .args([
            "--config",
            "signing.behavior=drop",
            "git",
            "init",
            "--no-colocate",
        ])
        .arg(temporary_directory.path())
        .output()
        .expect("jj should be installed");
    assert!(
        output.status.success(),
        "jj init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new("jj")
        .args([
            "git",
            "remote",
            "add",
            "origin",
            "git@github.com:owner/repo.git",
        ])
        .current_dir(temporary_directory.path())
        .output()
        .expect("jj remote add should run");
    assert!(
        output.status.success(),
        "jj remote add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new("jj")
        .args(["bookmark", "create", "main", "-r", "@"])
        .current_dir(temporary_directory.path())
        .output()
        .expect("jj bookmark create should run");
    assert!(
        output.status.success(),
        "jj bookmark create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let repository = Repository::open(temporary_directory.path()).expect("repository");
    (temporary_directory, repository)
}
