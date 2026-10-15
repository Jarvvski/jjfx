//! The cross-workspace migration-order check (ticket 17).
//!
//! Parallel Workers each edit their own workspace, and nothing compares what
//! they are about to land. For an ordered-migration project (Flyway, Alembic,
//! Rails, any `V<n>__name` sequence) that is a merge hazard no per-PR check can
//! see: two workspaces can each add a migration, each be individually correct,
//! green and at zero open threads, and still be wrong as a *set*. Merging the
//! higher version first leaves a lower one landing after it, which Flyway
//! rejects on the next deploy when `outOfOrder` is unset - and CI never sees it,
//! because CI applies the whole set in version order on a fresh database.
//!
//! The check is read-only: it reads each open workspace's **working copy** (the
//! files on disk, which is where a staged-but-unpushed migration lives - not the
//! bookmark's committed tree, which lags) and reports. It never reorders or
//! rewrites anything; the owner decides.
//!
//! The unit is a workspace's *directory*, because that is the working copy jj
//! keeps per workspace: a migration an agent has written but not committed is on
//! disk, while `jj file list -r '<ws>@'` would still show only the baseline.

use std::path::{Path, PathBuf};

use crate::store::DEFAULT_WORKSPACE;

/// Directories the walk never descends into: the two VCS stores and the build
/// output trees that would otherwise dominate a repo-wide walk. Migrations live
/// in source directories, not in these.
const PRUNED: [&str; 4] = [".jj", ".git", "target", "node_modules"];

/// A migration version, parsed from its file name as the numeric components of
/// its leading number, so `V189` is `[189]` and Flyway's dotted `V1.2` is
/// `[1, 2]`. Compared component by component - the order the migration tool
/// applies them in - so `V1.2 < V1.10 < V2`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(Vec<u64>);

impl Version {
    /// Parse the version from a migration file name.
    ///
    /// The version is the first run of digits in the name plus any `.`/`_`
    /// separated digit runs that immediately follow it, so `V189__add.sql` is
    /// `[189]`, `V1.2__add.sql` and `V1_2__add.sql` are `[1, 2]`, and a
    /// timestamp-prefixed name still orders. `None` when the name carries no
    /// number at all - it cannot be ordered, so it is not a migration to compare.
    pub fn parse(file_name: &str) -> Option<Self> {
        let bytes = file_name.as_bytes();
        let mut index = bytes.iter().position(u8::is_ascii_digit)?;
        let mut components = Vec::new();
        loop {
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            components.push(file_name[start..index].parse().ok()?);
            // A `.`/`_` separator followed by another digit keeps the version
            // going; anything else (the `__` before the description, a letter, or
            // the end) ends it.
            let continues = index + 1 < bytes.len()
                && matches!(bytes[index], b'_' | b'.')
                && bytes[index + 1].is_ascii_digit();
            if !continues {
                break;
            }
            index += 1;
        }
        Some(Self(components))
    }
}

impl std::fmt::Display for Version {
    /// The version as the migration tool writes it: components joined by `.`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<String> = self.0.iter().map(u64::to_string).collect();
        f.write_str(&parts.join("."))
    }
}

/// One migration found in a workspace's working copy: which workspace added it,
/// its repo-relative path, and its parsed version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    /// The Workspace whose working copy holds the file.
    pub workspace: String,
    /// The file's path relative to the workspace root, `/`-separated.
    pub path: String,
    /// The version parsed from the file name.
    pub version: Version,
}

/// Every migration in one workspace's working copy, ordered by path.
pub fn read_workspace(workspace: &str, dir: &Path, pattern: &str) -> Vec<Placed> {
    let mut files = Vec::new();
    collect(dir, dir, pattern, &mut files);
    files.sort();
    files
        .into_iter()
        .filter_map(|path| {
            let name = Path::new(&path).file_name()?.to_str()?;
            let version = Version::parse(name)?;
            Some(Placed {
                workspace: workspace.to_owned(),
                path,
                version,
            })
        })
        .collect()
}

/// Every migration across the repository's workspaces, ordered by workspace then
/// path. The implicit `default` Workspace (the repository root) is included, so a
/// caller can tell a Workspace's additions from the baseline every Workspace
/// inherits.
pub fn read(repo_root: &Path, pattern: &str) -> anyhow::Result<Vec<Placed>> {
    let mut placed = Vec::new();
    for (name, dir) in workspaces(repo_root)? {
        placed.extend(read_workspace(&name, &dir, pattern));
    }
    Ok(placed)
}

/// Every workspace jj reports, as `(name, directory)`, with the implicit
/// `default` Workspace pinned to the repository root (ADR 0008). A workspace jj
/// reports without a recorded path is skipped: jjfx never guesses a location.
fn workspaces(repo_root: &Path) -> anyhow::Result<Vec<(String, PathBuf)>> {
    let mut workspaces = vec![(DEFAULT_WORKSPACE.to_owned(), repo_root.to_path_buf())];
    for workspace in crate::jj::workspaces(repo_root)? {
        if let Some(path) = workspace.path {
            workspaces.push((workspace.name, path));
        }
    }
    Ok(workspaces)
}

/// Every file under `dir` whose path, relative to `root`, matches `pattern`, as
/// `/`-separated paths. Unreadable directories are skipped rather than failing
/// the check - a permission error on one subtree must not hide the rest.
fn collect(root: &Path, dir: &Path, pattern: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            let name = entry.file_name();
            if PRUNED.contains(&name.to_string_lossy().as_ref()) {
                continue;
            }
            collect(root, &path, pattern, out);
        } else if file_type.is_file()
            && let Ok(relative) = path.strip_prefix(root)
        {
            let relative = relative.to_string_lossy().replace('\\', "/");
            if matches(pattern, &relative) {
                out.push(relative);
            }
        }
    }
}

/// Whether a repo-relative path matches the configured glob.
///
/// The subset jjfx needs: `**` matches zero or more whole path segments, `*`
/// matches any run of characters within one segment, `?` matches one character
/// within one segment, and everything else is literal. So the default
/// `**/V*__*.sql` matches `V1__x.sql` at the root and `db/migration/V1__x.sql`
/// alike.
fn matches(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    match_segments(&pattern, &path)
}

/// Match a segment list against a path segment list, `**` consuming any number
/// of segments (including none).
fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| match_segments(rest, &path[skip..])),
        Some((segment, rest)) => match path.split_first() {
            Some((first, tail)) if match_segment(segment, first) => match_segments(rest, tail),
            _ => false,
        },
    }
}

/// Match one path segment against one pattern segment, `*` (any run) and `?`
/// (one character) only. A `**` inside a segment behaves as `*`.
fn match_segment(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    match_from(&pattern, &name)
}

/// The wildcard match behind [`match_segment`], over char slices so multi-byte
/// names are handled without indexing into bytes.
fn match_from(pattern: &[char], name: &[char]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some(('*', rest)) => (0..=name.len())
            .rev()
            .any(|skip| match_from(rest, &name[skip..])),
        Some(('?', rest)) => match name.split_first() {
            Some((_, tail)) => match_from(rest, tail),
            None => false,
        },
        Some((literal, rest)) => match name.split_first() {
            Some((first, tail)) if first == literal => match_from(rest, tail),
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parses_flyway_and_dotted_shapes() {
        let v = |name: &str| Version::parse(name).map(|v| v.0);
        assert_eq!(v("V189__add_users.sql"), Some(vec![189]));
        assert_eq!(v("V190__add_posts.sql"), Some(vec![190]));
        assert_eq!(v("V1.2__add.sql"), Some(vec![1, 2]));
        assert_eq!(v("V1_2__add.sql"), Some(vec![1, 2]));
        // A timestamp-prefixed name still orders by its number.
        assert_eq!(
            v("20231015120000_create_users.rb"),
            Some(vec![20231015120000])
        );
        // No number at all: nothing to order.
        assert_eq!(v("README.sql"), None);
        assert_eq!(v("V__no_version.sql"), None);
    }

    #[test]
    fn version_orders_component_by_component() {
        let v = |name: &str| Version::parse(name).expect("a version");
        assert!(v("V1.2__a.sql") < v("V1.10__b.sql"));
        assert!(v("V1.10__a.sql") < v("V2__b.sql"));
        assert!(v("V189__a.sql") < v("V190__b.sql"));
        assert_eq!(v("V1.2__a.sql"), v("V1_2__b.sql"));
    }

    #[test]
    fn pattern_matches_nested_and_root_paths() {
        assert!(matches("**/V*__*.sql", "V189__add.sql"));
        assert!(matches("**/V*__*.sql", "db/migration/V189__add.sql"));
        assert!(matches("**/V*__*.sql", "a/b/c/V189__add.sql"));
        assert!(!matches("**/V*__*.sql", "db/migration/U189__add.sql"));
        assert!(!matches("**/V*__*.sql", "db/migration/V189__add.txt"));
    }

    #[test]
    fn pattern_supports_a_scoped_layout() {
        assert!(matches("db/migration/V*.sql", "db/migration/V1__a.sql"));
        assert!(!matches("db/migration/V*.sql", "other/V1__a.sql"));
        assert!(matches("*.sql", "V1__a.sql"));
        assert!(!matches("*.sql", "db/V1__a.sql"));
    }

    #[test]
    fn pattern_supports_single_character_wildcards() {
        assert!(matches("V?.sql", "V1.sql"));
        assert!(!matches("V?.sql", "V12.sql"));
    }

    #[test]
    fn collect_finds_migrations_and_prunes_vcs_and_build_dirs() {
        let dir = tempdir("collect");
        std::fs::create_dir_all(dir.join("db/migration")).expect("creating db/migration");
        std::fs::create_dir_all(dir.join("target/deep")).expect("creating target");
        std::fs::create_dir_all(dir.join(".jj/whatever")).expect("creating .jj");
        std::fs::write(dir.join("db/migration/V1__a.sql"), "").expect("writing a migration");
        std::fs::write(dir.join("target/deep/V2__b.sql"), "").expect("writing a pruned file");
        std::fs::write(dir.join(".jj/whatever/V3__c.sql"), "").expect("writing a pruned file");
        std::fs::write(dir.join("README.md"), "").expect("writing a non-migration");

        let mut files = Vec::new();
        collect(&dir, &dir, "**/V*__*.sql", &mut files);
        assert_eq!(files, ["db/migration/V1__a.sql"]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_workspace_parses_versions_and_skips_unversioned_files() {
        let dir = tempdir("read");
        std::fs::create_dir_all(dir.join("db/migration")).expect("creating db/migration");
        std::fs::write(dir.join("db/migration/V189__a.sql"), "").expect("writing");
        std::fs::write(dir.join("db/migration/V190__b.sql"), "").expect("writing");
        // Matches the pattern but carries no version: not a comparable migration.
        std::fs::write(dir.join("db/migration/V__c.sql"), "").expect("writing");

        let placed = read_workspace("ws", &dir, "**/V*__*.sql");
        assert_eq!(
            placed
                .iter()
                .map(|p| (p.workspace.as_str(), p.path.as_str(), p.version.0.clone()))
                .collect::<Vec<_>>(),
            [
                ("ws", "db/migration/V189__a.sql", vec![189]),
                ("ws", "db/migration/V190__b.sql", vec![190]),
            ]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_project_with_no_matching_files_reads_nothing() {
        let dir = tempdir("empty");
        std::fs::create_dir_all(dir.join("src")).expect("creating src");
        std::fs::write(dir.join("src/main.rs"), "").expect("writing");
        assert!(read_workspace("ws", &dir, "**/V*__*.sql").is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A throwaway directory under the temp dir.
    fn tempdir(tag: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "jjfx-migrations-{tag}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("creating the temp dir");
        dir
    }
}
