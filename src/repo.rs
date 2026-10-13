//! Locate the jj repository a jjfx session is running against.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

/// Walk up from `start` to the nearest ancestor containing a `.jj` directory
/// and return the **repository's default workspace root** - the workspace that
/// owns the shared `.jj/repo` store.
///
/// A secondary workspace created with `jj workspace add` has its own `.jj`
/// directory whose `repo` file points at the shared store; returning that
/// secondary path would let it shadow the default workspace. Normalizing here
/// means the TUI store, runtime, watcher, and work poller all agree on one root.
pub fn discover(start: &Path) -> anyhow::Result<PathBuf> {
    let mut dir = start
        .canonicalize()
        .with_context(|| format!("resolving start path {}", start.display()))?;
    loop {
        if dir.join(".jj").is_dir() {
            return crate::engine::Repository::open(&dir)
                .map(|repository| repository.root().to_path_buf())
                .with_context(|| format!("resolving repository root at {}", dir.display()));
        }
        if !dir.pop() {
            bail!(
                "not inside a jj repository: no .jj/ found above {}",
                start.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_dot_jj_from_a_nested_dir() {
        let tmp = std::env::temp_dir().join(format!("jjfx-repo-test-{}", std::process::id()));
        let nested = tmp.join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(tmp.join(".jj")).unwrap();

        let root = discover(&nested).unwrap();
        assert_eq!(root, tmp.canonicalize().unwrap());

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn normalizes_a_secondary_workspace_to_the_default_root() {
        let tmp = std::env::temp_dir().join(format!("jjfx-repo-secondary-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let default = tmp.join("repo");
        let secondary = tmp.join("ws-secondary");
        // The default workspace owns the store; a secondary workspace points its
        // `.jj/repo` file at it (exactly what `jj workspace add` writes).
        std::fs::create_dir_all(default.join(".jj/repo")).unwrap();
        std::fs::create_dir_all(secondary.join(".jj")).unwrap();
        std::fs::write(secondary.join(".jj/repo"), "../../repo/.jj/repo").unwrap();

        let root = discover(&secondary).unwrap();
        assert_eq!(root, default.canonicalize().unwrap());

        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
