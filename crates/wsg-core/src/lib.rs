//! Shared Workspace Dispatch foundations for the `jjfx` and `wsg` binaries.

use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// The migration capabilities currently exposed by a discovered repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationCapabilities {
    /// The shared Workspace Dispatch implementation is not available yet.
    NotImplemented,
}

/// A Jujutsu repository discovered from a starting path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    root: PathBuf,
}

impl Repository {
    /// Discovers the nearest ancestor containing a `.jj` directory.
    pub fn open(start: impl AsRef<Path>) -> Result<Self, RepositoryError> {
        let start = start.as_ref().to_path_buf();
        let mut directory = start.clone();
        if !directory.is_absolute() {
            directory = std::env::current_dir()
                .map_err(|source| RepositoryError::PathResolution {
                    path: start.clone(),
                    source,
                })?
                .join(directory);
        }
        let mut directory =
            directory
                .canonicalize()
                .map_err(|source| RepositoryError::PathResolution {
                    path: start.clone(),
                    source,
                })?;

        loop {
            if directory.join(".jj").is_dir() {
                return Ok(Self { root: directory });
            }
            if !directory.pop() {
                return Err(RepositoryError::NotFound { start });
            }
        }
    }

    /// Returns the canonical path of the repository workspace root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Reports which migration capabilities are available for this foundation.
    pub fn migration_capabilities(&self) -> MigrationCapabilities {
        MigrationCapabilities::NotImplemented
    }
}

/// Errors that can occur while discovering a Jujutsu repository.
#[derive(Debug, Error)]
pub enum RepositoryError {
    /// The starting path could not be resolved.
    #[error("cannot resolve repository path {path}")]
    PathResolution {
        /// The path supplied to [`Repository::open`].
        path: PathBuf,
        /// The operating-system error that prevented resolution.
        #[source]
        source: io::Error,
    },
    /// No repository marker was found at or above the starting path.
    #[error("not inside a Jujutsu repository: no .jj directory found above {start}")]
    NotFound {
        /// The path supplied to [`Repository::open`].
        start: PathBuf,
    },
}
