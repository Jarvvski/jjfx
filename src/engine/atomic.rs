//! Shared durability primitives for persisted Workspace Dispatch state: atomic
//! replacement of one file and exclusive advisory file locks.
//!
//! The state repositories, the Worker Workspace cache, and Dispatch Group
//! orchestration each write their own documents and take their own sidecar
//! locks. They route through these helpers so the tree has one
//! atomic-replacement implementation and one lock implementation, and a
//! durability fix lands at every caller at once.

use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use rustix::fs::{FlockOperation, flock};
use tempfile::NamedTempFile;

/// The step of an atomic replacement that failed, so callers can phrase their
/// own contextual error instead of re-deriving the operation from a message.
#[derive(Debug)]
pub(crate) enum AtomicWriteError {
    /// The target path has no parent directory to hold a temporary file.
    MissingParent,
    /// The target's parent directory could not be created.
    CreateDirectory(io::Error),
    /// The uniquely named temporary file could not be created.
    CreateTemporary(io::Error),
    /// The temporary file's mode could not be set to `0644`.
    SetPermissions(io::Error),
    /// The document bytes could not be written to the temporary file.
    Write(io::Error),
    /// The temporary file could not be flushed.
    Flush(io::Error),
    /// The temporary file could not be synced to disk.
    Sync(io::Error),
    /// The temporary file could not be renamed over the target.
    Rename(io::Error),
}

/// The step of taking an exclusive advisory lock that failed.
#[derive(Debug)]
pub(crate) enum LockError {
    /// The lock sidecar's directory could not be created.
    CreateDirectory(io::Error),
    /// The lock sidecar could not be opened.
    Open(io::Error),
    /// The lock could not be taken.
    Lock(io::Error),
}

impl LockError {
    /// The operating-system error that caused the failure.
    pub(crate) fn source(&self) -> &io::Error {
        match self {
            Self::CreateDirectory(error) | Self::Open(error) | Self::Lock(error) => error,
        }
    }
}

/// The result of a non-blocking exclusive lock attempt.
#[derive(Debug)]
pub(crate) enum LockOutcome {
    /// The lock was taken; the returned file holds it until dropped.
    Acquired(File),
    /// Another holder has the lock.
    Busy,
}

/// Atomically replaces `path` with `bytes`.
///
/// The bytes are written to a uniquely named temporary file in the target
/// directory, synced, and renamed over the target. A failure at any step leaves
/// the previous target intact and removes the temporary file.
pub(crate) fn write_atomic_bytes(path: &Path, bytes: &[u8]) -> Result<(), AtomicWriteError> {
    let parent = path.parent().ok_or(AtomicWriteError::MissingParent)?;
    fs::create_dir_all(parent).map_err(AtomicWriteError::CreateDirectory)?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(AtomicWriteError::CreateTemporary)?;
    temporary
        .as_file()
        .set_permissions(Permissions::from_mode(0o644))
        .map_err(AtomicWriteError::SetPermissions)?;
    temporary
        .write_all(bytes)
        .map_err(AtomicWriteError::Write)?;
    temporary.flush().map_err(AtomicWriteError::Flush)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(AtomicWriteError::Sync)?;
    temporary
        .into_temp_path()
        .persist(path)
        .map_err(|error| AtomicWriteError::Rename(error.error))?;
    Ok(())
}

/// Takes a blocking exclusive advisory lock on `path`, creating it if needed.
///
/// The lock is held until the returned file is dropped. Lock files are stable
/// sidecars because atomic replacement changes the target's inode.
pub(crate) fn lock_exclusive(path: &Path) -> Result<File, LockError> {
    let file = open_lock(path)?;
    flock(&file, FlockOperation::LockExclusive)
        .map_err(|error| LockError::Lock(io_error(error)))?;
    Ok(file)
}

/// Takes an exclusive advisory lock on `path` without blocking.
pub(crate) fn try_lock_exclusive(path: &Path) -> Result<LockOutcome, LockError> {
    let file = open_lock(path)?;
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(LockOutcome::Acquired(file)),
        Err(error) if error == rustix::io::Errno::WOULDBLOCK => Ok(LockOutcome::Busy),
        Err(error) => Err(LockError::Lock(io_error(error))),
    }
}

fn io_error(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}

fn open_lock(path: &Path) -> Result<File, LockError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(LockError::CreateDirectory)?;
    }
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(LockError::Open)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn a_failed_replacement_leaves_the_previous_target_intact() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let target = directory.path().join("document.json");
        fs::write(&target, b"previous").expect("previous target");
        let original = fs::metadata(directory.path())
            .expect("metadata")
            .permissions();
        let mut read_only = original.clone();
        read_only.set_mode(0o555);
        fs::set_permissions(directory.path(), read_only).expect("make target directory read-only");

        let result = write_atomic_bytes(&target, b"next");

        fs::set_permissions(directory.path(), original).expect("restore directory permissions");
        assert!(result.is_err(), "the temporary file cannot be created");
        let mut current = String::new();
        fs::File::open(&target)
            .expect("open target")
            .read_to_string(&mut current)
            .expect("read target");
        assert_eq!(current, "previous");
        assert_eq!(
            fs::read_dir(directory.path())
                .expect("read directory")
                .count(),
            1,
            "a failed replacement leaves no temporary file behind"
        );
    }

    #[test]
    fn a_missing_parent_is_reported_without_touching_the_filesystem() {
        let result = write_atomic_bytes(Path::new(""), b"next");
        assert!(matches!(result, Err(AtomicWriteError::MissingParent)));
    }
}
