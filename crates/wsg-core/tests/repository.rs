use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use wsg_core::{Loaded, MigrationCapabilities, Repository, RepositoryError, WorkerId};

fn local_repository() -> (TempDir, Repository) {
    let temporary_directory = tempfile::tempdir().expect("temporary directory should be created");
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
    let repository = Repository::open(temporary_directory.path()).expect("repository should open");
    (temporary_directory, repository)
}

fn default_worker_path(root: &Path, worker: &str) -> std::path::PathBuf {
    root.parent()
        .expect("temporary repository should have a parent")
        .join(format!(
            "{}-workspaces/{worker}",
            root.file_name()
                .expect("temporary repository should have a name")
                .to_string_lossy()
        ))
}

#[test]
#[ignore]
fn workspace_path_environment_helper() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env::var_os("WSG_TEST_ROOT").ok_or("test root should be set")?);
    let result = PathBuf::from(env::var_os("WSG_TEST_RESULT").ok_or("test result should be set")?);
    let worker = WorkerId::parse("worker-env")?;
    let repository = Repository::open(root)?;
    let workspace = repository.provision_worker_workspace(&worker)?;
    fs::write(
        result,
        workspace.path().as_os_str().to_string_lossy().as_bytes(),
    )?;
    Ok(())
}

fn workspace_names(root: &Path) -> Vec<String> {
    let output = Command::new("jj")
        .args(["workspace", "list"])
        .current_dir(root)
        .output()
        .expect("jj workspace list should run");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once(':').map(|(name, _)| name.trim().to_owned()))
        .collect()
}

#[test]
fn reports_missing_repository_with_typed_context() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory should be created");

    let error =
        Repository::open(temporary_directory.path()).expect_err("repository should be missing");

    assert!(matches!(error, RepositoryError::NotFound { .. }));
}

#[test]
fn opens_nested_repository_and_reports_foundation_status() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory should be created");
    let nested = temporary_directory.path().join("workspace/src");
    std::fs::create_dir_all(&nested).expect("nested directory should be created");
    std::fs::create_dir(temporary_directory.path().join(".jj"))
        .expect("repository marker should be created");

    let repository = Repository::open(&nested).expect("repository should be discovered");

    assert_eq!(
        repository.root(),
        temporary_directory
            .path()
            .canonicalize()
            .expect("path should resolve")
    );
    assert_eq!(
        repository.migration_capabilities(),
        MigrationCapabilities::ReadOnlyWorkerPool
    );
}

#[test]
fn opens_a_secondary_workspace_at_the_default_workspace_root() {
    let (temporary_directory, repository) = local_repository();
    let secondary = temporary_directory.path().join("secondary-workspace");
    let output = Command::new("jj")
        .args(["workspace", "add", "--name", "secondary"])
        .arg(&secondary)
        .current_dir(repository.root())
        .output()
        .expect("jj workspace add should run");
    assert!(
        output.status.success(),
        "jj workspace add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let secondary_repository =
        Repository::open(&secondary).expect("secondary repository should open");

    assert_eq!(secondary_repository.root(), repository.root());
}

#[test]
fn creates_and_removes_an_ad_hoc_workspace_through_the_shared_repository() {
    let (_temporary_directory, repository) = local_repository();

    let workspace = repository
        .create_ad_hoc_workspace(" feat ")
        .expect("Ad Hoc Workspace should be created");
    let expected_path = repository.root().with_file_name(format!(
        "{}-feat",
        repository
            .root()
            .file_name()
            .expect("repository name")
            .to_string_lossy()
    ));

    assert_eq!(workspace.name(), "feat");
    assert_eq!(workspace.path(), expected_path);
    assert!(workspace_names(repository.root()).contains(&"feat".to_owned()));
    assert!(
        fs::read_to_string(repository.root().join(".jj/ws-cache"))
            .expect("workspace cache")
            .lines()
            .any(|line| line == format!("feat\t{}", expected_path.display()))
    );

    repository
        .remove_ad_hoc_workspace(workspace.name(), Some(workspace.path()))
        .expect("Ad Hoc Workspace should be removed");

    assert!(!workspace_names(repository.root()).contains(&"feat".to_owned()));
    assert!(!expected_path.exists());
    assert!(
        !fs::read_to_string(repository.root().join(".jj/ws-cache"))
            .expect("workspace cache")
            .lines()
            .any(|line| line.starts_with("feat\t"))
    );
}

#[test]
fn ad_hoc_workspace_cache_failures_do_not_reverse_successful_jj_mutations() {
    let (_temporary_directory, repository) = local_repository();
    let cache = repository.root().join(".jj/ws-cache");
    fs::create_dir(&cache).expect("cache collision directory");

    let workspace = repository
        .create_ad_hoc_workspace("feat")
        .expect("jj creation should remain successful");

    assert!(workspace.path().is_dir());
    assert!(workspace_names(repository.root()).contains(&"feat".to_owned()));
}

#[test]
fn ad_hoc_workspace_removal_protects_default() {
    let (_temporary_directory, repository) = local_repository();

    let error = repository
        .remove_ad_hoc_workspace("default", Some(repository.root()))
        .expect_err("default Workspace should be protected");

    assert_eq!(error.to_string(), "the default workspace cannot be deleted");
    assert!(repository.root().is_dir());
}

#[test]
fn provisions_a_compatible_worker_workspace_and_idle_state() {
    let (_temporary_directory, repository) = local_repository();
    let worker = WorkerId::parse("worker-01").expect("Worker ID should be valid");

    let workspace = repository
        .provision_worker_workspace(&worker)
        .expect("Worker Workspace should be provisioned");

    let expected_path = default_worker_path(repository.root(), "worker-01");
    assert_eq!(workspace.worker_id(), &worker);
    assert_eq!(workspace.path(), expected_path);
    assert!(workspace.path().is_dir());
    assert!(workspace_names(repository.root()).contains(&"worker-01".to_owned()));

    let cache = fs::read_to_string(repository.root().join(".jj/ws-cache"))
        .expect("ws-cache should be projected");
    assert_eq!(
        cache,
        format!(
            "default\t{}\nworker-01\t{}\n",
            repository.root().display(),
            expected_path.display()
        )
    );
    let state = repository
        .state_store()
        .worker(worker)
        .load()
        .expect("Worker state should load");
    let Loaded::Present(state) = state else {
        panic!("Worker state should exist");
    };
    assert_eq!(state.value.status.as_str(), "idle");
    assert_eq!(state.value.agent, None);
    assert_eq!(state.value.ticket, None);
    assert_eq!(state.value.pid, None);
    assert_eq!(state.value.branch_name, None);
}

fn assert_workspace_directory_environment(
    value: &Path,
    repository: &Repository,
    expected: PathBuf,
) {
    let result = repository.root().join("workspace-path-result");
    let mut command = Command::new(env::current_exe().expect("test executable should exist"));
    command
        .args(["--exact", "workspace_path_environment_helper", "--ignored"])
        .env("JJ_WS_DIR", value)
        .env("WSG_TEST_ROOT", repository.root())
        .env("WSG_TEST_RESULT", &result);
    let output = command.output().expect("workspace helper should run");
    assert!(
        output.status.success(),
        "workspace helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        PathBuf::from(fs::read_to_string(result).expect("workspace result should exist")),
        expected,
    );
    let _ = fs::remove_dir_all(expected);
}

#[test]
fn uses_a_relative_workspace_directory_from_environment() {
    let (_temporary_directory, repository) = local_repository();
    let value = PathBuf::from("custom-workspaces");
    let expected = repository.root().join(&value).join("worker-env");
    assert_workspace_directory_environment(&value, &repository, expected);
}

#[test]
fn uses_an_absolute_workspace_directory_from_environment() {
    let (_temporary_directory, repository) = local_repository();
    let value = repository
        .root()
        .parent()
        .expect("temporary repository should have a parent")
        .join("absolute-workspaces");
    let expected = value.join("worker-env");
    assert_workspace_directory_environment(&value, &repository, expected);
}

#[test]
fn provisions_present_setup_sources_without_copying_synapse_git_metadata() {
    let (temporary_directory, repository) = local_repository();
    fs::write(
        temporary_directory.path().join(".env"),
        "DATABASE_URL=test\n",
    )
    .expect("environment source should be written");
    let synapse = temporary_directory
        .path()
        .join("tools/dev-cli/synapse/clone");
    fs::create_dir_all(synapse.join(".git")).expect("Synapse git directory should be created");
    fs::write(synapse.join(".git/config"), "private").expect("git metadata should be written");
    fs::write(synapse.join("prompt.txt"), "prompt").expect("Synapse file should be written");
    let worker = WorkerId::parse("worker-01").expect("Worker ID should be valid");

    let workspace = repository
        .provision_worker_workspace(&worker)
        .expect("Worker Workspace should be provisioned");

    assert_eq!(
        fs::read_to_string(workspace.path().join(".env")).expect("environment should copy"),
        "DATABASE_URL=test\n"
    );
    assert_eq!(
        fs::read_to_string(
            workspace
                .path()
                .join("tools/dev-cli/synapse/clone/prompt.txt")
        )
        .expect("Synapse file should copy"),
        "prompt"
    );
    assert!(
        !workspace
            .path()
            .join("tools/dev-cli/synapse/clone/.git")
            .exists()
    );
}

#[test]
fn provisioning_rejects_a_claimed_worker_without_touching_it() {
    let (_temporary_directory, repository) = local_repository();
    let worker = WorkerId::parse("worker-01").expect("Worker ID should be valid");
    let existing = default_worker_path(repository.root(), "worker-01");
    fs::create_dir_all(&existing).expect("existing path should be created");

    let error = repository
        .provision_worker_workspace(&worker)
        .expect_err("existing Worker Workspace should be rejected");

    assert!(
        error
            .to_string()
            .contains("Worker Workspace path already exists")
    );
    assert!(existing.is_dir());
    assert!(!workspace_names(repository.root()).contains(&"worker-01".to_owned()));
    assert!(!repository.root().join(".jj/pool/worker-01.json").exists());
}

#[test]
fn provisioning_compensates_when_cache_projection_fails() {
    let (_temporary_directory, repository) = local_repository();
    let temporary_cache = repository
        .root()
        .join(format!(".jj/ws-cache.{}.tmp", std::process::id()));
    fs::create_dir(&temporary_cache).expect("cache temporary collision should be created");
    let worker = WorkerId::parse("worker-01").expect("Worker ID should be valid");

    let error = repository
        .provision_worker_workspace(&worker)
        .expect_err("cache projection should fail");

    assert!(error.to_string().contains("write ws-cache temporary file"));
    assert!(!workspace_names(repository.root()).contains(&"worker-01".to_owned()));
    assert!(!default_worker_path(repository.root(), "worker-01").exists());
    assert!(!repository.root().join(".jj/pool/worker-01.json").exists());
    assert!(!repository.root().join(".jj/ws-cache").exists());
}

#[test]
fn provisioning_compensates_when_an_existing_environment_source_cannot_be_copied() {
    let (temporary_directory, repository) = local_repository();
    fs::create_dir(temporary_directory.path().join(".env"))
        .expect("invalid environment source should be created");
    let worker = WorkerId::parse("worker-01").expect("Worker ID should be valid");

    let error = repository
        .provision_worker_workspace(&worker)
        .expect_err("copying a directory as .env should fail");

    assert!(error.to_string().contains("copy .env"));
    assert!(!workspace_names(repository.root()).contains(&"worker-01".to_owned()));
    assert!(!default_worker_path(repository.root(), "worker-01").exists());
    assert!(!repository.root().join(".jj/pool/worker-01.json").exists());
    assert!(!repository.root().join(".jj/ws-cache").exists());
}
