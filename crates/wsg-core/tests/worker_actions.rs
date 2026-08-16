use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{
    Pid, Signal, kill_process_group, test_kill_process, test_kill_process_group,
};
use tempfile::TempDir;
use wsg_core::{
    AgentModel, AgentRuntime, AgentSessionResolution, DismissOutcome, Expected, FollowUpExecution,
    Loaded, PoolCapacity, Repository, RunMode, RunReset, StateChange, WireAgent, WireStatus,
    WireTimestamp, WorkerActions, WorkerId, WorkspaceRestoration,
};

const HELPER_REPOSITORY: &str = "WSG_ACTION_REPOSITORY";
const HELPER_WORKER: &str = "WSG_ACTION_WORKER";
const HELPER_RESULT: &str = "WSG_ACTION_RESULT";
const HELPER_CAPTURE: &str = "WSG_ACTION_CAPTURE";
const HELPER_MODE: &str = "WSG_ACTION_MODE";
const HELPER_PROVIDER: &str = "WSG_ACTION_PROVIDER";
const HELPER_MODEL: &str = "WSG_ACTION_MODEL";
const HELPER_PROCESS: &str = "WSG_ACTION_PROCESS";
const HELPER_DESCENDANT: &str = "WSG_ACTION_DESCENDANT";
const HELPER_DIAGNOSTIC: &str = "WSG_ACTION_DIAGNOSTIC";
const HELPER_EXIT: &str = "WSG_ACTION_EXIT";

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
    assert_eq!(worker_state.status(), wsg_core::WorkerStatus::Idle);
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
        wsg_core::WorkerStatus::Busy
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
fn rebase_and_open_pull_request_use_compatible_typed_actions() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("repository-actions.log");
    fs::write(&log, "{}\n").expect("Worker log");
    set_terminal_worker(&repository, &worker, &log);

    let bin = temporary_directory.path().join("bin");
    fs::create_dir(&bin).expect("fake command directory");
    let command_script =
        "#!/bin/sh\nprintf '%s %s\\n' \"$(basename \"$0\")\" \"$*\" >> \"$WSG_ACTION_CAPTURE\"\n";
    write_executable(&bin.join("jj"), command_script);
    write_executable(&bin.join("gh"), command_script);
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
    assert_eq!(
        fs::read_to_string(&result).expect("action result"),
        "owner/eng-301-action\nowner/eng-301-action\n"
    );
    assert_eq!(
        fs::read_to_string(&capture).expect("command capture"),
        concat!(
            "jj rebase -b owner/eng-301-action -d main\n",
            "jj git push -b owner/eng-301-action\n",
            "gh -R owner/repo pr view owner/eng-301-action --web\n"
        )
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
            "printf 'jj %s\\n' \"$*\" >> \"$WSG_ACTION_CAPTURE\"\n",
            "if [ \"$1\" = git ] && [ \"$2\" = push ]; then echo rejected >&2; exit 1; fi\n"
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
        fs::read_to_string(&capture).expect("command capture"),
        concat!(
            "jj rebase -b owner/eng-301-action -d main\n",
            "jj git push -b owner/eng-301-action\n",
            "jj op undo\n"
        )
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
            "printf 'kitten %s\\n' \"$*\" >> \"$WSG_ACTION_CAPTURE\"\n",
            "case \"$*\" in\n",
            "  *--type=tab*) echo 42 ;;\n",
            "  *--location=vsplit*) echo 43 ;;\n",
            "  *--location=hsplit*) echo 44 ;;\n",
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
    assert!(commands.contains("claude --resume 'session-mount'; exec zsh"));
    assert!(commands.contains("--location=vsplit"));
    assert!(commands.contains("--location=hsplit"));
    assert!(commands.contains("focus-window --match id:42"));
}

#[test]
fn mount_opens_a_resumed_pi_session_with_the_trusted_worker_policy() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let log = repository.root().join("pi-mount.log");
    let session_id = "session 'pi'; echo unsafe";
    fs::write(
        &log,
        format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":{session_id:?},\"timestamp\":\"2026-08-13T10:00:00Z\",\"cwd\":{:?}}}\n",
            worker_workspace_path(&repository, &worker).to_string_lossy()
        ),
    )
    .expect("Pi Worker log");
    set_terminal_worker_for_runtime(&repository, &worker, &log, AgentRuntime::Pi);

    let bin = temporary_directory.path().join("pi-mount-bin");
    fs::create_dir(&bin).expect("fake command directory");
    let pi_capture = temporary_directory.path().join("unused-pi-run");
    write_fake_pi(&bin.join("pi"), &pi_capture);
    write_executable(
        &bin.join("kitten"),
        concat!(
            "#!/bin/sh\n",
            "printf 'kitten %s\\n' \"$*\" >> \"$WSG_ACTION_CAPTURE\"\n",
            "case \"$*\" in\n",
            "  *--type=tab*) echo 52 ;;\n",
            "  *--location=vsplit*) echo 53 ;;\n",
            "  *--location=hsplit*) echo 54 ;;\n",
            "esac\n"
        ),
    );
    let capture = temporary_directory.path().join("pi-mount-commands");
    let result = temporary_directory.path().join("pi-mount-result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );
    let provider = "test provider";
    let unsafe_path = temporary_directory.path().join("must-not-exist");
    let model = format!("model 'quoted'; $(touch {})", unsafe_path.display());

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "mount_action_helper"])
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env(HELPER_PROVIDER, provider)
        .env(HELPER_MODEL, &model)
        .env("KITTY_LISTEN_ON", "unix:/tmp/fake-kitty")
        .env("PATH", path)
        .output()
        .expect("Pi Mount action helper should run");

    assert!(
        output.status.success(),
        "Pi Mount helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(&result).expect("Pi Mount result"),
        format!("pi\nresumed:{session_id}\n52\n")
    );
    let commands = fs::read_to_string(&capture).expect("Pi mount command capture");
    assert!(commands.contains("exec 'pi'"));
    assert!(commands.contains("'--provider' 'test provider'"));
    assert!(commands.contains(&format!(
        "'--model' 'model '\\''quoted'\\''; $(touch {})'",
        unsafe_path.display()
    )));
    assert!(commands.contains("'--session' 'session '\\''pi'\\''; echo unsafe'"));
    assert!(commands.contains("'--session-dir'"));
    assert!(commands.contains(".jj/pool/pi-sessions"));
    assert!(commands.contains("'--no-extensions' '--no-skills' '--no-prompt-templates' '--no-themes' '--no-context-files' '--no-approve'"));
    assert!(commands.contains("'--tools' 'read,bash,edit,write,grep,find,ls'"));
    let agent_launch = commands
        .lines()
        .find(|line| line.contains("--type=tab"))
        .expect("agent tab launch");
    assert!(!agent_launch.contains("exec zsh"));
    assert!(
        !pi_capture.exists(),
        "interactive Pi is launched only by kitty"
    );
    assert!(!unsafe_path.exists());
}

#[test]
fn mount_opens_a_fresh_pi_session_without_a_resume_argument() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    set_pool_runtime(&repository, AgentRuntime::Pi);
    let bin = temporary_directory.path().join("fresh-pi-mount-bin");
    fs::create_dir(&bin).expect("fake command directory");
    let pi_capture = temporary_directory.path().join("unused-fresh-pi-run");
    write_fake_pi(&bin.join("pi"), &pi_capture);
    write_executable(
        &bin.join("kitten"),
        concat!(
            "#!/bin/sh\n",
            "printf 'kitten %s\\n' \"$*\" >> \"$WSG_ACTION_CAPTURE\"\n",
            "case \"$*\" in *--type=tab*) echo 62 ;; esac\n"
        ),
    );
    let capture = temporary_directory.path().join("fresh-pi-mount-commands");
    let result = temporary_directory.path().join("fresh-pi-mount-result");
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
        .env(HELPER_PROVIDER, "test-provider")
        .env(HELPER_MODEL, "test-model")
        .env("KITTY_LISTEN_ON", "unix:/tmp/fake-kitty")
        .env("PATH", path)
        .output()
        .expect("fresh Pi Mount action helper should run");

    assert!(
        output.status.success(),
        "fresh Pi Mount helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(&result).expect("fresh Pi Mount result"),
        "pi\nfresh:no prior session log\n62\n"
    );
    let commands = fs::read_to_string(&capture).expect("fresh Pi mount command capture");
    let agent_launch = commands
        .lines()
        .find(|line| line.contains("--type=tab"))
        .expect("agent tab launch");
    assert!(agent_launch.contains("exec 'pi'"));
    assert!(!agent_launch.contains("'--session'"));
}

#[test]
fn mount_rejects_pi_without_a_model_before_opening_kitty() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    set_pool_runtime(&repository, AgentRuntime::Pi);
    let bin = temporary_directory.path().join("invalid-pi-mount-bin");
    fs::create_dir(&bin).expect("fake command directory");
    let pi_capture = temporary_directory.path().join("unused-invalid-pi-run");
    write_fake_pi(&bin.join("pi"), &pi_capture);
    let kitten_capture = temporary_directory.path().join("unexpected-kitten-launch");
    write_executable(
        &bin.join("kitten"),
        &format!("#!/bin/sh\ntouch {}\n", kitten_capture.display()),
    );
    let result = temporary_directory.path().join("invalid-pi-mount-result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "failed_mount_action_helper"])
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env("KITTY_LISTEN_ON", "unix:/tmp/fake-kitty")
        .env("PATH", path)
        .output()
        .expect("failed Pi Mount helper should run");

    assert!(output.status.success());
    assert!(
        fs::read_to_string(result)
            .expect("failed Pi Mount result")
            .contains("pi command requires a model")
    );
    assert!(
        !kitten_capture.exists(),
        "kitty must not launch without a Pi model"
    );
}

#[test]
fn mount_reports_pi_probe_failure_before_opening_kitty() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    set_pool_runtime(&repository, AgentRuntime::Pi);
    let bin = temporary_directory.path().join("failed-pi-probe-bin");
    fs::create_dir(&bin).expect("fake command directory");
    write_executable(
        &bin.join("pi"),
        "#!/bin/sh\necho 'unsupported Pi build' >&2\nexit 2\n",
    );
    let kitten_capture = temporary_directory
        .path()
        .join("unexpected-probe-kitten-launch");
    write_executable(
        &bin.join("kitten"),
        &format!("#!/bin/sh\ntouch {}\n", kitten_capture.display()),
    );
    let result = temporary_directory.path().join("failed-pi-probe-result");
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var("PATH").expect("PATH should exist")
    );

    let output = Command::new(env::current_exe().expect("current test executable"))
        .args(["--ignored", "--exact", "failed_mount_action_helper"])
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env("KITTY_LISTEN_ON", "unix:/tmp/fake-kitty")
        .env("PATH", path)
        .output()
        .expect("failed Pi probe helper should run");

    assert!(output.status.success());
    assert!(
        fs::read_to_string(result)
            .expect("failed Pi probe result")
            .contains("pi version capability probe failed with status 2")
    );
    assert!(
        !kitten_capture.exists(),
        "kitty must not launch after probe failure"
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
fn reset_terminates_a_background_pi_follow_up_and_its_descendant() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    set_pool_runtime(&repository, AgentRuntime::Pi);
    let bin = temporary_directory.path().join("stubborn-pi-bin");
    fs::create_dir(&bin).expect("fake runtime directory");
    write_executable(
        &bin.join("pi"),
        concat!(
            "#!/bin/bash\n",
            "if [ \"$1\" = \"--version\" ]; then echo 0.84.1; exit 0; fi\n",
            "if [ \"$1\" = \"--help\" ]; then echo '--mode --provider --model --session --session-dir --system-prompt --name --tools --no-extensions --no-skills --no-prompt-templates --no-themes --no-context-files --no-approve'; exit 0; fi\n",
            "if [ \"$1\" = \"--mode\" ] && [ \"$2\" = \"rpc\" ]; then /bin/cp \"$WSG_WORKER_PROFILE_FIXTURE\" \"$JJFX_PI_PROFILE_PROBE_OUTPUT\"; exit 0; fi\n",
            "( trap '' TERM; while :; do sleep 0.05; done ) &\n",
            "printf '%s\\n' \"$!\" > \"$WSG_ACTION_DESCENDANT\"\n",
            "trap '' TERM\n",
            "printf '%s\\n' \"$$\" > \"$WSG_ACTION_PROCESS\"\n",
            "printf started > \"$WSG_ACTION_DIAGNOSTIC\"\n",
            "while :; do sleep 0.05; done\n"
        ),
    );
    let (agent_dir, profile_fixture) = install_valid_pi_profile(temporary_directory.path());
    let result = temporary_directory.path().join("pi-background-pid");
    let process = temporary_directory.path().join("pi-background-process");
    let descendant = temporary_directory.path().join("pi-background-descendant");
    let diagnostic = temporary_directory.path().join("pi-background-diagnostic");
    let exit = temporary_directory.path().join("pi-background-exit");
    let path = env::join_paths([
        bin.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let mut command = Command::new(env::current_exe().expect("test executable"));
    command
        .args(["--ignored", "--exact", "background_send_action_helper"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_PROVIDER, "test-provider")
        .env(HELPER_MODEL, "test-model")
        .env("PI_CODING_AGENT_DIR", &agent_dir)
        .env("WSG_WORKER_PROFILE_FIXTURE", &profile_fixture)
        .env(HELPER_PROCESS, &process)
        .env(HELPER_DESCENDANT, &descendant)
        .env(HELPER_DIAGNOSTIC, &diagnostic)
        .env(HELPER_EXIT, &exit)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut helper = BackgroundActionGuard::spawn(&mut command, &result);
    wait_for_file(&result);
    wait_for_file(&diagnostic);
    wait_for_file(&descendant);
    let leader = read_pid(&result);
    let descendant_pid = read_pid(&descendant);

    let outcome = WorkerActions::new(repository.clone())
        .reset(&worker)
        .expect("Pi Follow-up Reset should succeed");
    assert_eq!(
        outcome.run(),
        RunReset::Abandoned {
            terminated_pid: Some(leader)
        }
    );
    let WorkspaceRestoration::Pending(restoration) = outcome.into_restoration() else {
        panic!("existing Worker Workspace should be restored");
    };
    restoration
        .wait()
        .expect("Worker Workspace restoration should succeed");

    let output = helper.wait_with_output();
    assert!(
        output.status.success(),
        "background Pi helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    wait_for_process_exit(leader);
    wait_for_process_exit(descendant_pid);
    assert!(test_kill_process_group(unix_pid(leader)).is_err());
    let snapshot = repository.worker_pool().snapshot();
    let state = snapshot.worker(worker.as_str()).expect("reset Worker");
    assert_eq!(state.status(), wsg_core::WorkerStatus::Idle);
    assert_eq!(state.pid(), None);
    assert_eq!(state.agent_runtime(), None);
    assert!(
        fs::read_to_string(exit)
            .expect("waiter outcome")
            .contains("exit=")
    );
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
    write_executable(
        &bin.join("gh"),
        "#!/bin/sh\ncase \"$*\" in\n  *\"pr list\"*) printf '%s\\n' '[{\"number\":42,\"url\":\"https://example/pr/42\",\"headRefName\":\"owner/eng-301-action\",\"mergeable\":\"CONFLICTING\",\"reviewDecision\":\"CHANGES_REQUESTED\"}]' ;;\n  *\"pr checks\"*) printf '%s\\n' '[{\"name\":\"tests\",\"conclusion\":\"FAILURE\"},{\"name\":\"lint\",\"conclusion\":\"SUCCESS\"}]'; exit 1 ;;\nesac\n",
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

#[test]
fn send_resumes_the_prior_claude_session_and_reports_it() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let prior_log = repository
        .root()
        .join(".jj/pool")
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
fn send_resumes_the_prior_pi_session_with_the_configured_model() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let prior_log = repository.root().join("pi-prior.log");
    fs::write(
        &prior_log,
        format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"session-pi-301\",\"timestamp\":\"2026-08-13T10:00:00Z\",\"cwd\":{:?}}}\n",
            worker_workspace_path(&repository, &worker).to_string_lossy()
        ),
    )
    .expect("prior Pi Session log");
    set_terminal_worker_for_runtime(&repository, &worker, &prior_log, AgentRuntime::Pi);

    let bin = temporary_directory.path().join("pi-bin");
    fs::create_dir(&bin).expect("fake executable directory");
    let captured = temporary_directory.path().join("captured-pi-args");
    write_fake_pi(&bin.join("pi"), &captured);
    let (agent_dir, profile_fixture) = install_valid_pi_profile(temporary_directory.path());
    let result = temporary_directory.path().join("pi-result");
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
        .env(HELPER_PROVIDER, "test-provider")
        .env(HELPER_MODEL, "test-model")
        .env("PI_CODING_AGENT_DIR", &agent_dir)
        .env("WSG_WORKER_PROFILE_FIXTURE", &profile_fixture)
        .stdin(Stdio::null())
        .output()
        .expect("Pi Send helper should run");

    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("Pi Send result"),
        "runtime=pi; session=resumed:session-pi-301; completed=true"
    );
    let args = fs::read_to_string(captured).expect("captured Pi arguments");
    assert!(args.contains("--provider\ntest-provider\n--model\ntest-model"));
    assert!(args.contains("--session\nsession-pi-301"));
    assert!(args.contains("--session-dir"));
    assert!(args.contains(".jj/pool/pi-sessions"));
    assert!(args.ends_with("continue the work\n"));
}

#[test]
fn send_on_an_idle_pi_worker_starts_fresh_with_the_configured_model() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    set_pool_runtime(&repository, AgentRuntime::Pi);
    let bin = temporary_directory.path().join("fresh-pi-bin");
    fs::create_dir(&bin).expect("fake executable directory");
    let captured = temporary_directory.path().join("captured-fresh-pi-args");
    write_fake_pi(&bin.join("pi"), &captured);
    let (agent_dir, profile_fixture) = install_valid_pi_profile(temporary_directory.path());
    let result = temporary_directory.path().join("fresh-pi-result");
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
        .env(HELPER_PROVIDER, "test-provider")
        .env(HELPER_MODEL, "test-model")
        .env("PI_CODING_AGENT_DIR", &agent_dir)
        .env("WSG_WORKER_PROFILE_FIXTURE", &profile_fixture)
        .stdin(Stdio::null())
        .output()
        .expect("fresh Pi Send helper should run");

    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("fresh Pi Send result"),
        "runtime=pi; session=fresh:no prior session log; completed=true"
    );
    let args = fs::read_to_string(captured).expect("captured fresh Pi arguments");
    assert!(args.contains("--provider\ntest-provider\n--model\ntest-model"));
    assert!(!args.contains("--session\n"));
    assert!(args.contains("--system-prompt"));
    assert!(args.ends_with("continue the work\n"));
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
fn invalid_pi_follow_up_profile_fails_before_beginning_follow_up() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let prior_log = repository.root().join("prior-profile-pi.log");
    fs::write(
        &prior_log,
        "{\"type\":\"session\",\"version\":3,\"id\":\"session-pi-profile\",\"timestamp\":\"2026-08-13T10:00:00Z\",\"cwd\":\"/tmp/project\"}\n",
    )
    .expect("prior Pi Session log");
    set_terminal_worker_for_runtime(&repository, &worker, &prior_log, AgentRuntime::Pi);

    let agent_dir = temporary_directory.path().join("pi-agent");
    let package = agent_dir.join("npm/node_modules/pi-mcp-adapter");
    fs::create_dir_all(&package).expect("Pi adapter package");
    fs::write(
        package.join("package.json"),
        r#"{"name":"pi-mcp-adapter","version":"2.11.0"}"#,
    )
    .expect("Pi adapter manifest");
    fs::write(package.join("index.ts"), "export default function () {}\n")
        .expect("Pi adapter entry");
    let fixture = temporary_directory.path().join("profile-fixture.json");
    fs::write(
        &fixture,
        r#"{
            "allTools": [
                {"name":"linear_get_issue","parameters":{"type":"object","properties":{"id":{"type":"string"}}}},
                {"name":"linear_update_issue","parameters":{"type":"object","properties":{"id":{"type":"string"},"status":{"type":"string"},"assignee":{"type":"string"}}}}
            ],
            "activeTools": ["linear_get_issue","linear_update_issue"]
        }"#,
    )
    .expect("Pi profile fixture");
    let bin = temporary_directory.path().join("profile-pi-bin");
    fs::create_dir(&bin).expect("fake executable directory");
    let runtime_marker = temporary_directory.path().join("runtime-started");
    write_executable(
        &bin.join("pi"),
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo 0.84.1; exit 0; fi
if [ "$1" = "--help" ]; then echo '--mode --provider --model --session --session-dir --system-prompt --name --tools --no-extensions --no-skills --no-prompt-templates --no-themes --no-context-files --no-approve'; exit 0; fi
if [ "$1" = "--mode" ] && [ "$2" = "rpc" ]; then /bin/cp "$WSG_WORKER_PROFILE_FIXTURE" "$JJFX_PI_PROFILE_PROBE_OUTPUT"; exit 0; fi
touch "$WSG_WORKER_RUNTIME_MARKER"
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done"}'
"#,
    );
    let result = temporary_directory
        .path()
        .join("failed-profile-send-result");
    let path = env::join_paths([
        bin.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");

    let output = Command::new(env::current_exe().expect("test executable"))
        .args(["--exact", "failed_send_action_helper", "--ignored"])
        .env("PATH", path)
        .env("PI_CODING_AGENT_DIR", &agent_dir)
        .env("WSG_WORKER_PROFILE_FIXTURE", &fixture)
        .env("WSG_WORKER_RUNTIME_MARKER", &runtime_marker)
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_PROVIDER, "test-provider")
        .env(HELPER_MODEL, "test-model")
        .stdin(Stdio::null())
        .output()
        .expect("failed Pi profile Send helper should run");

    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let error = fs::read_to_string(result).expect("failed Pi profile Send result");
    assert!(
        error.contains("linear_create_comment"),
        "unexpected error: {error}"
    );
    let worker = repository
        .worker_pool()
        .snapshot()
        .worker(worker.as_str())
        .expect("unchanged Worker")
        .clone();
    assert_eq!(worker.status(), wsg_core::WorkerStatus::Done);
    assert_eq!(
        worker.log_file(),
        Some(prior_log.to_string_lossy().as_ref())
    );
    assert!(!runtime_marker.exists(), "Pi Follow-up runtime started");
}

#[test]
fn failed_pi_send_validation_does_not_mutate_the_prior_terminal_worker() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let prior_log = repository.root().join("prior-pi.log");
    fs::write(
        &prior_log,
        "{\"type\":\"session\",\"version\":3,\"id\":\"session-pi-rollback\",\"timestamp\":\"2026-08-13T10:00:00Z\",\"cwd\":\"/tmp/project\"}\n",
    )
    .expect("prior Pi Session log");
    set_terminal_worker_for_runtime(&repository, &worker, &prior_log, AgentRuntime::Pi);
    let before = match repository
        .state_store()
        .worker(worker.clone())
        .load()
        .expect("load Pi Worker before Send")
    {
        wsg_core::Loaded::Present(worker) => worker.revision().clone(),
        wsg_core::Loaded::Missing => panic!("Pi Worker missing before Send"),
    };

    let bin = temporary_directory.path().join("failed-pi-bin");
    fs::create_dir(&bin).expect("fake executable directory");
    let capture = temporary_directory.path().join("unused-pi-capture");
    write_fake_pi(&bin.join("pi"), &capture);
    let result = temporary_directory.path().join("failed-pi-send-result");
    let path = env::join_paths([
        bin.as_os_str(),
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
        .expect("failed Pi Send helper should run");

    assert!(output.status.success());
    assert!(
        fs::read_to_string(result)
            .expect("failed Pi Send result")
            .contains("pi command requires a model")
    );
    let after = match repository
        .state_store()
        .worker(worker.clone())
        .load()
        .expect("load Pi Worker after rejected Send")
    {
        wsg_core::Loaded::Present(worker) => worker,
        wsg_core::Loaded::Missing => panic!("Pi Worker missing after rejected Send"),
    };
    assert_eq!(after.revision(), &before);
    assert_eq!(after.value.status.as_str(), "done");
    assert_eq!(after.value.agent, Some(wsg_core::WireAgent::new("pi")));
    assert_eq!(
        after.value.log_file.as_deref(),
        Some(prior_log.to_string_lossy().as_ref())
    );
    assert!(
        !capture.exists(),
        "Pi Run must not start after validation fails"
    );
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
    assert_eq!(restored.status(), wsg_core::WorkerStatus::Done);
    assert_eq!(restored.ticket(), Some("ENG-301"));
    assert_eq!(restored.branch_name(), Some("owner/eng-301-action"));
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
        .send(&worker, "continue the Pi work", RunMode::Background)
        .expect("background Pi Follow-up should launch");
    let FollowUpExecution::Background(run) = outcome.into_execution() else {
        panic!("background action should return a waiter");
    };
    fs::write(
        env::var_os(HELPER_RESULT).expect("result path"),
        run.pid().to_string(),
    )
    .expect("background Pi PID");
    let completed = run
        .wait()
        .expect("background Pi Follow-up should be reaped");
    fs::write(
        env::var_os(HELPER_EXIT).expect("exit path"),
        format!("exit={:?}", completed.exit_code()),
    )
    .expect("background Pi waiter outcome");
}

#[test]
#[ignore]
fn failed_mount_action_helper() {
    let repository =
        Repository::open(env::var_os(HELPER_REPOSITORY).expect("repository")).expect("repository");
    let worker = WorkerId::parse(env::var(HELPER_WORKER).expect("Worker ID")).expect("Worker ID");
    let error = WorkerActions::new(repository)
        .mount(&worker)
        .expect_err("Pi Mount without a model should fail");
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
    if mode != "repository" && mode != "rebase-failure" {
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
    } else {
        actions
            .rebase(&worker)
            .expect_err("Push should fail")
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

fn set_pool_runtime(repository: &Repository, runtime: AgentRuntime) {
    let state_repository = repository.state_store().pool();
    let loaded = match state_repository.load().expect("Pool state") {
        Loaded::Present(versioned) => versioned,
        Loaded::Missing => panic!("Pool state should exist"),
    };
    let (mut state, revision) = loaded.into_parts();
    state.agent = Some(WireAgent::new(runtime.as_str()));
    let outcome = state_repository
        .commit(Expected::Match(revision), StateChange::Replace(state))
        .expect("configured Pool runtime");
    assert!(matches!(outcome, wsg_core::CommitOutcome::Applied(_)));
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
    state.branch_name = Some("owner/eng-301-action".to_owned());
    state.exit_code = Some(0);
    let outcome = state_repository
        .commit(Expected::Match(revision), StateChange::Replace(state))
        .expect("terminal Worker state");
    assert!(matches!(outcome, wsg_core::CommitOutcome::Applied(_)));
}

struct BackgroundActionGuard {
    child: Option<Child>,
    leader_path: PathBuf,
}

impl BackgroundActionGuard {
    fn spawn(command: &mut Command, leader_path: &Path) -> Self {
        Self {
            child: Some(command.spawn().expect("background action helper")),
            leader_path: leader_path.to_owned(),
        }
    }

    fn wait_with_output(&mut self) -> std::process::Output {
        self.child
            .take()
            .expect("background action helper")
            .wait_with_output()
            .expect("background action helper output")
    }
}

impl Drop for BackgroundActionGuard {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        if let Ok(contents) = fs::read_to_string(&self.leader_path)
            && let Ok(raw_pid) = contents.trim().parse::<u32>()
        {
            let _ = kill_process_group(unix_pid(raw_pid), Signal::KILL);
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn unix_pid(pid: u32) -> Pid {
    i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .expect("PID should fit Unix range")
}

fn read_pid(path: &Path) -> u32 {
    fs::read_to_string(path)
        .expect("PID file")
        .trim()
        .parse()
        .expect("numeric PID")
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_process_exit(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while test_kill_process(unix_pid(pid)).is_ok() {
        assert!(Instant::now() < deadline, "process {pid} did not exit");
        thread::sleep(Duration::from_millis(10));
    }
}

fn install_valid_pi_profile(root: &Path) -> (PathBuf, PathBuf) {
    let agent_dir = root.join("valid-pi-agent");
    let package = agent_dir.join("npm/node_modules/pi-mcp-adapter");
    fs::create_dir_all(&package).expect("Pi adapter package");
    fs::write(
        package.join("package.json"),
        r#"{"name":"pi-mcp-adapter","version":"2.11.0"}"#,
    )
    .expect("Pi adapter manifest");
    fs::write(package.join("index.ts"), "export default function () {}\n")
        .expect("Pi adapter entry");
    let fixture = root.join("valid-pi-profile.json");
    fs::write(
        &fixture,
        r#"{
            "allTools": [
                {"name":"linear_get_issue","parameters":{"type":"object","properties":{"id":{"type":"string"}}}},
                {"name":"linear_update_issue","parameters":{"type":"object","properties":{"id":{"type":"string"},"status":{"type":"string"},"assignee":{"type":"string"}}}},
                {"name":"linear_create_comment","parameters":{"type":"object","properties":{"issueId":{"type":"string"},"body":{"type":"string"}}}}
            ],
            "activeTools": ["linear_get_issue","linear_update_issue","linear_create_comment"]
        }"#,
    )
    .expect("Pi profile fixture");
    (agent_dir, fixture)
}

fn write_fake_pi(path: &Path, capture: &Path) {
    write_executable(
        path,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 0.84.1; exit 0; fi\nif [ \"$1\" = \"--help\" ]; then echo '--mode --provider --model --session --session-dir --system-prompt --name --tools --no-extensions --no-skills --no-prompt-templates --no-themes --no-context-files --no-approve'; exit 0; fi\nif [ \"$1\" = \"--mode\" ] && [ \"$2\" = \"rpc\" ]; then /bin/cp \"$WSG_WORKER_PROFILE_FIXTURE\" \"$JJFX_PI_PROFILE_PROBE_OUTPUT\"; exit 0; fi\nprintf '%s\\n' \"$@\" > {}\nprintf '%s\\n' '{{\"type\":\"session\",\"version\":3,\"id\":\"session-pi-301\",\"timestamp\":\"2026-08-13T10:00:00Z\",\"cwd\":\"/tmp/project\"}}' '{{\"type\":\"message_end\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"done\"}}],\"provider\":\"test-provider\",\"model\":\"test-model\",\"stopReason\":\"stop\"}}}}'\n",
            capture.display()
        ),
    );
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
        .args(["--config", "signing.behavior=drop", "git", "init"])
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
