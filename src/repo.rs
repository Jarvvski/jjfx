//! Locate the jj repository a jjfx session is running against.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

/// Walk up from `start` to the nearest ancestor containing a `.jj` directory
/// and return that directory (the workspace root - where `.jj/` lives).
pub fn discover(start: &Path) -> anyhow::Result<PathBuf> {
    let mut dir = start
        .canonicalize()
        .with_context(|| format!("resolving start path {}", start.display()))?;
    loop {
        if dir.join(".jj").is_dir() {
            return Ok(dir);
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
}
