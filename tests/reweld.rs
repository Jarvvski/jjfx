//! Re-welding a delivered change whose Worker has been reused.
//!
//! Two Workers delivered from one trunk in a batch; the first merged and moved
//! trunk under the second, whose Pull Request then read `CONFLICTING`. By the
//! time that was known the Worker that delivered it had been **reused for the
//! next Ticket**, so `jjfx deliver worker-a04066` named a different change and
//! no jjfx verb named `adam/tally--04-json-output` at all.
//!
//! The change is named by its bookmark, which is durable, rather than by the
//! Worker, which is a reusable slot. These cases drive the real binary against a
//! repository with a local bare `origin` (no network) and a change no Workspace
//! holds, and assert the whole round trip: re-weld, name the Workspace the
//! conflict must be resolved in, resolve, re-weld, push.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// A repository with a local bare `origin`, a `main` trunk, and one *delivered*
/// change - `adam/eng-1`, pushed, and held by no Workspace at all, which is the
/// state a reused Worker leaves behind. `sibling_merged` advances `main` over the
/// same line, which is what makes the re-weld conflict.
struct ReweldFixture {
    parent: TempDir,
    root: PathBuf,
    origin: PathBuf,
    config_home: TempDir,
}

/// The bookmark the fixture delivers - the durable name of the change.
const BRANCH: &str = "adam/eng-1";

/// The Workspace jjfx creates to hold it, named after the bookmark.
const CREATED_WORKSPACE: &str = "branch-adam-eng-1";

impl ReweldFixture {
    /// A repository whose `adam/eng-1` change is delivered and held by no
    /// Workspace, and whose trunk has not moved yet.
    fn delivered() -> Self {
        let parent = TempDir::new().expect("temporary directory");
        let root = parent.path().join("repo");
        fs::create_dir_all(&root).expect("repository directory");
        let config_home = TempDir::new().expect("temporary config home");
        // The fixture owns its jj identity: a delivery rewrites commits and jj
        // needs an author, which a clean-config runner has none of.
        let jj_config = config_home.path().join("jj");
        fs::create_dir_all(&jj_config).expect("jj config directory");
        fs::write(
            jj_config.join("config.toml"),
            "user.name = \"jjfx test\"\nuser.email = \"jjfx@example.com\"\n",
        )
        .expect("jj config");
        let jjfx_config = config_home.path().join("jjfx");
        fs::create_dir_all(&jjfx_config).expect("jjfx config directory");
        fs::write(
            jjfx_config.join("config.toml"),
            "[forge]\npull_requests = false\n\n[checks]\ncommand = \"true\"\n",
        )
        .expect("jjfx config");

        let origin = parent.path().join("origin.git");
        git_init_bare(&origin);

        let fixture = Self {
            parent,
            root,
            origin,
            config_home,
        };
        fixture.jj(&["git", "init", "--no-colocate", "."]);
        fixture.jj(&[
            "git",
            "remote",
            "add",
            "origin",
            &fixture.origin.to_string_lossy(),
        ]);
        fs::write(fixture.root.join("f.txt"), "base\n").expect("base file");
        fixture.jj(&["commit", "-m", "seed"]);
        fixture.jj(&["bookmark", "create", "main", "-r", "@-"]);
        fixture.jj(&["git", "push", "--bookmark", "main"]);

        // The delivered change, pushed, and then left behind: `jj new main` moves
        // the repository's only working copy off it, so no Workspace's chain
        // contains the bookmark - exactly what reusing the Worker did.
        fs::write(fixture.root.join("f.txt"), "delivered\n").expect("delivered file");
        fixture.jj(&["commit", "-m", "eng-1: the delivered change"]);
        fixture.jj(&["bookmark", "create", BRANCH, "-r", "@-"]);
        fixture.jj(&["git", "push", "--bookmark", BRANCH]);
        fixture.jj(&["new", "main"]);
        fixture.jj(&["git", "fetch"]);
        fixture
    }

    /// Advance `main` over the same line the delivered change rewrote - the
    /// sibling that merged first, and the reason the re-weld conflicts.
    fn merge_a_sibling_over_the_same_line(&self) {
        fs::write(self.root.join("f.txt"), "sibling\n").expect("sibling file");
        self.jj(&["commit", "-m", "eng-2: the sibling that merged first"]);
        self.jj(&["bookmark", "set", "main", "-r", "@-"]);
        self.jj(&["git", "push", "--bookmark", "main"]);
        self.jj(&["new", "main"]);
        self.jj(&["git", "fetch"]);
    }

    fn jj(&self, args: &[&str]) {
        let output = Command::new("jj")
            .args(["--config", "signing.behavior=drop"])
            .current_dir(&self.root)
            // The fixture owns its identity through XDG_CONFIG_HOME; a
            // developer's JJ_CONFIG would take precedence and could leave a
            // commit without an author, so it is dropped for hermeticity.
            .env_remove("JJ_CONFIG")
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .args(args)
            .output()
            .expect("jj should be installed");
        assert!(
            output.status.success(),
            "jj {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Run one `jjfx` verb from the repository root.
    fn jjfx(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_jjfx"))
            .args(args)
            .current_dir(&self.root)
            .env_remove("JJ_CONFIG")
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .output()
            .expect("jjfx should run")
    }

    /// The Workspace directories jjfx created beside the repository.
    fn workspace_directories(&self) -> Vec<String> {
        let base = self.parent.path().join("repo-workspaces");
        let Ok(entries) = fs::read_dir(base) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// The refs the bare `origin` holds.
    fn origin_refs(&self) -> String {
        let output = Command::new("git")
            .arg(format!("--git-dir={}", self.origin.display()))
            .args(["for-each-ref", "--format=%(refname)"])
            .output()
            .expect("git should be installed");
        assert!(output.status.success(), "git for-each-ref");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// The commit `origin` holds for `branch`, or an empty string when it holds
    /// none. The delivered change is on `origin` from the fixture's first push,
    /// so "did the re-weld push?" is a question about the *commit*, not the ref.
    fn origin_branch_commit(&self, branch: &str) -> String {
        let output = Command::new("git")
            .arg(format!("--git-dir={}", self.origin.display()))
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("refs/heads/{branch}"))
            .output()
            .expect("git should be installed");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// The contents of `f.txt` in the Workspace jjfx created.
    fn created_workspace_file(&self) -> String {
        fs::read_to_string(
            self.parent
                .path()
                .join("repo-workspaces")
                .join(CREATED_WORKSPACE)
                .join("f.txt"),
        )
        .expect("the created Workspace's file")
    }
}

/// A bare repository whose default branch is `main`, so jj's `trunk()` resolves
/// after a fetch.
fn git_init_bare(origin: &Path) {
    let init = Command::new("git")
        .args(["init", "--bare", "--quiet"])
        .arg(origin)
        .output()
        .expect("git should be installed");
    assert!(
        init.status.success(),
        "git init --bare: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let head = Command::new("git")
        .arg(format!("--git-dir={}", origin.display()))
        .args(["symbolic-ref", "HEAD", "refs/heads/main"])
        .output()
        .expect("git should be installed");
    assert!(head.status.success(), "git symbolic-ref HEAD");
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The criterion the ticket exists for: a delivered change whose Worker is gone
/// is re-welded by its bookmark, and the conflict it records names the Workspace
/// an operator has to resolve it in.
#[test]
fn a_delivered_change_is_re_welded_by_its_bookmark_and_names_the_workspace_to_resolve_in() {
    let fixture = ReweldFixture::delivered();
    fixture.merge_a_sibling_over_the_same_line();
    let pushed = fixture.origin_branch_commit(BRANCH);

    let output = fixture.jjfx(&["rebase", BRANCH]);

    assert!(
        !output.status.success(),
        "a conflicted re-weld must fail: {}",
        stderr(&output)
    );
    let stderr = stderr(&output);
    assert!(
        stderr.contains("Created Workspace branch-adam-eng-1"),
        "the verb must say it made a Workspace: {stderr}"
    );
    assert!(
        stderr.contains("rebased branch adam/eng-1 onto main"),
        "the refusal must name the change: {stderr}"
    );
    assert!(
        stderr.contains(&format!("resolve them in Workspace {CREATED_WORKSPACE}")),
        "the refusal must name the Workspace to resolve in: {stderr}"
    );
    assert!(
        stderr.contains(&format!("`jjfx resolve {CREATED_WORKSPACE}`")),
        "the refusal must name the command that resolves it: {stderr}"
    );
    // The rebase is left in place and nothing was pushed: a conflicted commit is
    // what jj refuses to push.
    assert_eq!(fixture.workspace_directories(), vec![CREATED_WORKSPACE]);
    assert_eq!(
        fixture.origin_branch_commit(BRANCH),
        pushed,
        "a conflicted re-weld must not push: {}",
        fixture.origin_refs()
    );
}

/// The whole round trip: re-weld, resolve in the named Workspace, re-weld again
/// and push. This is the manual flow the ticket records - "fetch, find the
/// branch, make a workspace, rebase it onto the new trunk, resolve, push" -
/// performed by jjfx.
#[test]
fn the_named_workspace_resolves_the_conflict_and_the_second_re_weld_pushes() {
    let fixture = ReweldFixture::delivered();
    fixture.merge_a_sibling_over_the_same_line();
    assert!(!fixture.jjfx(&["rebase", BRANCH]).status.success());

    let resolved = fixture.jjfx(&["resolve", CREATED_WORKSPACE, "--tool", ":ours"]);
    assert!(
        resolved.status.success(),
        "the named Workspace must resolve: {}",
        stderr(&resolved)
    );
    assert!(
        stderr(&resolved).contains(&format!("resolved {CREATED_WORKSPACE}")),
        "{}",
        stderr(&resolved)
    );

    let output = fixture.jjfx(&["rebase", BRANCH]);

    assert!(
        output.status.success(),
        "the re-weld must push once the conflict is resolved: {}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("Rebased adam/eng-1"),
        "{}",
        stderr(&output)
    );
    assert!(
        fixture.origin_refs().contains("refs/heads/adam/eng-1"),
        "the re-welded change must reach origin: {}",
        fixture.origin_refs()
    );
    // One resolve leaves one side whole, not conflict markers.
    let content = fixture.created_workspace_file();
    assert!(
        content == "delivered\n" || content == "sibling\n",
        "the conflict must be gone, got {content:?}"
    );
}

/// A second re-weld reuses the Workspace the first one made rather than
/// accumulating one per run, and a trunk that has not moved is not an error: the
/// push has nothing to move, which is the desired state already holding.
#[test]
fn re_welding_twice_reuses_the_workspace_and_a_still_trunk_succeeds() {
    let fixture = ReweldFixture::delivered();
    assert!(fixture.jjfx(&["rebase", BRANCH]).status.success());

    let again = fixture.jjfx(&["rebase", BRANCH]);

    assert!(
        again.status.success(),
        "a re-weld whose trunk has not moved must succeed: {}",
        stderr(&again)
    );
    assert!(
        !stderr(&again).contains("Created Workspace"),
        "the second run must reuse the Workspace: {}",
        stderr(&again)
    );
    assert_eq!(
        fixture.workspace_directories(),
        vec![CREATED_WORKSPACE],
        "one change gets one Workspace, not one per run"
    );
}

/// `deliver` names the change by its bookmark too, and a weld that conflicts
/// stops before the push naming the same Workspace. Without that, an operator
/// driving the forge is left with a conflict and no Workspace name to resolve it
/// in.
#[test]
fn deliver_by_bookmark_names_the_workspace_a_conflicting_weld_must_be_resolved_in() {
    let fixture = ReweldFixture::delivered();
    fixture.merge_a_sibling_over_the_same_line();
    let pushed = fixture.origin_branch_commit(BRANCH);

    let output = fixture.jjfx(&["deliver", BRANCH]);

    assert!(
        !output.status.success(),
        "a conflicting delivery must fail: {}",
        stderr(&output)
    );
    let stderr = stderr(&output);
    assert!(
        stderr.contains("Delivering adam/eng-1"),
        "the delivery must name the change it is delivering: {stderr}"
    );
    assert!(
        stderr.contains(&format!("resolve it in Workspace {CREATED_WORKSPACE}")),
        "the conflict must name the Workspace to resolve in: {stderr}"
    );
    assert_eq!(
        fixture.origin_branch_commit(BRANCH),
        pushed,
        "a conflicted weld must not push: {}",
        fixture.origin_refs()
    );
}

/// The names a landing verb does *not* accept are refused by name, and the two
/// hazardous ones - a name that is nothing, and the mainline - are refused rather
/// than acted on.
#[test]
fn a_landing_verb_refuses_an_unknown_name_the_mainline_and_a_missing_workspace() {
    let fixture = ReweldFixture::delivered();

    let unknown = fixture.jjfx(&["rebase", "no-such-change"]);
    assert!(!unknown.status.success(), "an unknown name must fail");
    assert!(
        stderr(&unknown).contains("`no-such-change` names no Worker and no bookmark"),
        "{}",
        stderr(&unknown)
    );

    // Re-welding the mainline onto itself is a no-op, and a landing verb must
    // never push it.
    let trunk = fixture.jjfx(&["rebase", "main"]);
    assert!(!trunk.status.success(), "the mainline must be refused");
    assert!(
        stderr(&trunk).contains("bookmark main is the mainline"),
        "{}",
        stderr(&trunk)
    );

    // A name in jjfx's own `worker-<id>` namespace that is not there reads as a
    // missing Workspace, not as a name that means nothing.
    let missing = fixture.jjfx(&["rebase", "worker-99"]);
    assert!(!missing.status.success(), "a missing Worker must fail");
    assert!(
        stderr(&missing).contains("Workspace directory is missing for Worker worker-99"),
        "{}",
        stderr(&missing)
    );
    assert!(
        fixture.workspace_directories().is_empty(),
        "a refusal must leave no Workspace behind: {:?}",
        fixture.workspace_directories()
    );
}

/// A change that *is* in a Workspace is re-welded there rather than in a new one:
/// the scan finds it, so the conflict is resolvable in the Workspace the change
/// is actually checked out in.
#[test]
fn a_change_a_workspace_holds_is_re_welded_in_that_workspace() {
    let fixture = ReweldFixture::delivered();
    fixture.merge_a_sibling_over_the_same_line();
    // Check the delivered change out at the repository root: a Workspace now
    // holds it, and it is the one the re-weld must use.
    fixture.jj(&["new", BRANCH]);

    let output = fixture.jjfx(&["rebase", BRANCH]);

    assert!(
        !output.status.success(),
        "the re-weld still conflicts: {}",
        stderr(&output)
    );
    let stderr = stderr(&output);
    assert!(
        stderr.contains("resolve them in Workspace default"),
        "the change's own Workspace must be named: {stderr}"
    );
    assert!(
        !stderr.contains("Created Workspace"),
        "a held change must not get a new Workspace: {stderr}"
    );
    assert!(
        fixture.workspace_directories().is_empty(),
        "no Workspace must be created: {:?}",
        fixture.workspace_directories()
    );
}
