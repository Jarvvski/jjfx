use wsg_core::{MigrationCapabilities, Repository, RepositoryError};

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
