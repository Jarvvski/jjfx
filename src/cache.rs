//! The `.jj/ws-cache` mirror: a lossy `name\tpath` projection of the workspace
//! store that the coexisting bash tools read and write (ADR 0006). jjfx reads it
//! to fold in shell-created workspaces and writes through to keep the bash tools
//! consistent. It is a mirror, never the source of truth.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Path of the ws-cache within a repo root.
pub fn path(repo_root: &Path) -> PathBuf {
    repo_root.join(".jj").join("ws-cache")
}

/// Read the ws-cache into `(name, path)` pairs. A missing file is an empty list,
/// not an error - the cache is optional and often absent until first written.
/// Blank and malformed lines (no tab) are skipped.
pub fn read(cache_path: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    let text = match fs::read_to_string(cache_path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(parse(&text))
}

fn parse(text: &str) -> Vec<(String, PathBuf)> {
    text.lines()
        .filter_map(|line| {
            let (name, path) = line.split_once('\t')?;
            if name.is_empty() || path.is_empty() {
                return None;
            }
            Some((name.to_string(), PathBuf::from(path)))
        })
        .collect()
}

/// Render `(name, path)` pairs to the exact ws-cache byte format: one
/// `name\tpath\n` line per entry, in the order given.
pub fn render(entries: &[(String, PathBuf)]) -> String {
    let mut out = String::new();
    for (name, path) in entries {
        out.push_str(name);
        out.push('\t');
        out.push_str(&path.to_string_lossy());
        out.push('\n');
    }
    out
}

/// Write the ws-cache atomically (temp file in the same dir, then rename), but
/// only if the on-disk bytes differ - so writing through does not churn the file
/// or ping our own watcher when nothing changed. Returns whether a write happened.
pub fn write_through(cache_path: &Path, entries: &[(String, PathBuf)]) -> io::Result<bool> {
    let desired = render(entries);
    if let Ok(existing) = fs::read_to_string(cache_path)
        && existing == desired
    {
        return Ok(false);
    }
    let dir = cache_path
        .parent()
        .ok_or_else(|| io::Error::other("ws-cache path has no parent"))?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("ws-cache.{}.tmp", std::process::id()));
    fs::write(&tmp, desired.as_bytes())?;
    // Rename is atomic within a filesystem: readers see either the old or the
    // new file, never a partial write.
    fs::rename(&tmp, cache_path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jjfx-cache-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips_name_tab_path_bytes() {
        let dir = scratch("rt");
        let cache = dir.join("ws-cache");
        let entries = vec![
            ("default".to_string(), PathBuf::from("/repo")),
            (
                "feature-x".to_string(),
                PathBuf::from("/repo/../wt/feature-x"),
            ),
        ];

        let wrote = write_through(&cache, &entries).unwrap();
        assert!(wrote);
        // Exact byte format the bash tools expect.
        assert_eq!(
            std::fs::read_to_string(&cache).unwrap(),
            "default\t/repo\nfeature-x\t/repo/../wt/feature-x\n"
        );
        assert_eq!(read(&cache).unwrap(), entries);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_cache_reads_empty() {
        let dir = scratch("missing");
        let got = read(&dir.join("ws-cache")).unwrap();
        assert!(got.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_through_skips_when_unchanged() {
        let dir = scratch("skip");
        let cache = dir.join("ws-cache");
        let entries = vec![("default".to_string(), PathBuf::from("/repo"))];
        assert!(write_through(&cache, &entries).unwrap());
        assert!(!write_through(&cache, &entries).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_skips_blank_and_malformed_lines() {
        let parsed = parse("default\t/repo\n\nno-tab-here\nfeat\t/w/feat\n");
        assert_eq!(
            parsed,
            vec![
                ("default".to_string(), PathBuf::from("/repo")),
                ("feat".to_string(), PathBuf::from("/w/feat")),
            ]
        );
    }
}
