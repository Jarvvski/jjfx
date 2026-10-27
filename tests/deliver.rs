//! `jjfx deliver` - the one forge path as a shell verb.
//!
//! The Dispatch prompt tells every Worker to deliver the change itself with
//! `jjfx deliver` (weld, check, push, open the Pull Request), rather than pushing
//! or opening it by hand. The verb exists because nothing else did: the forge was
//! TUI-only, `rebase` pushed without a PR, and `open-pr` only opened one that
//! already existed. These cases drive the real
//! binary against a repository with a local `origin` (no network), so the verb
//! must weld, run the repository's check, push, and - when a forge is
//! configured - reach the Pull Request step, from the shell.
//!
//! A repository with no forge stays a supported, non-degraded path: the change
//! is pushed and the delivery is clean.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// A repository with a local bare `origin`, a `main` trunk, and one `worker-01`
/// Workspace holding a bookmarked change - everything `deliver` needs, with no
/// network. `forge` is the `[forge] pull_requests` setting and `check` the
/// repository-owned `[checks] command`.
struct DeliverFixture {
    parent: TempDir,
    root: PathBuf,
    origin: PathBuf,
    workspace: PathBuf,
    config_home: TempDir,
}

impl DeliverFixture {
    fn new(pull_requests: bool, check: &str) -> Self {
        let parent = TempDir::new().expect("temporary directory");
        let root = parent.path().join("repo");
        fs::create_dir_all(&root).expect("repository directory");
        let config_home = TempDir::new().expect("temporary config home");
        // A jj user identity, because a delivery rewrites commits and jj needs
        // one - blank on a clean-config runner, and present here because this
        // fixture points XDG_CONFIG_HOME at its own config home.
        let jj_config = config_home.path().join("jj");
        fs::create_dir_all(&jj_config).expect("jj config directory");
        fs::write(
            jj_config.join("config.toml"),
            "user.name = \"jjfx test\"\nuser.email = \"jjfx@example.com\"\n",
        )
        .expect("jj config");
        // The jjfx user config: how the forge opens Pull Requests, and the
        // repository-owned build check.
        let jjfx_config = config_home.path().join("jjfx");
        fs::create_dir_all(&jjfx_config).expect("jjfx config directory");
        fs::write(
            jjfx_config.join("config.toml"),
            format!(
                "[forge]\npull_requests = {pull_requests}\n\n[checks]\ncommand = \"{check}\"\n"
            ),
        )
        .expect("jjfx config");

        let origin = parent.path().join("origin.git");
        git_init_bare(&origin);

        let mut fixture = Self {
            parent,
            root,
            origin,
            workspace: PathBuf::new(),
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
        fs::write(fixture.root.join("seed.txt"), "seed\n").expect("seed file");
        fixture.jj(&["commit", "-m", "seed"]);
        fixture.jj(&["bookmark", "create", "main", "-r", "@-"]);
        fixture.jj(&["git", "push", "--bookmark", "main"]);
        fixture.jj(&["git", "fetch"]);

        // A Worker Workspace at the path `jjfx` resolves for `worker-01`, with
        // one bookmarked change to deliver.
        let workspace = fixture
            .parent
            .path()
            .join("repo-workspaces")
            .join("worker-01");
        fs::create_dir_all(workspace.parent().expect("workspaces directory"))
            .expect("workspaces directory");
        fixture.jj(&[
            "workspace",
            "add",
            "--name",
            "worker-01",
            "--no-colocate",
            &workspace.to_string_lossy(),
        ]);
        fs::write(workspace.join("change.txt"), "the change\n").expect("change file");
        fixture.jj_in(&workspace, &["commit", "-m", "the change"]);
        fixture.jj_in(
            &workspace,
            &["bookmark", "create", "adam/eng-1", "-r", "@-"],
        );
        fixture.workspace = workspace;
        fixture
    }

    fn jj(&self, args: &[&str]) {
        self.jj_in(&self.root, args);
    }

    fn jj_in(&self, dir: &Path, args: &[&str]) {
        let output = Command::new("jj")
            .args(["--config", "signing.behavior=drop"])
            .current_dir(dir)
            // The fixture owns its jj identity through XDG_CONFIG_HOME; a
            // developer's JJ_CONFIG would take precedence and could leave the
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

    /// Runs `jjfx deliver` for `worker` from the repository root.
    fn deliver(&self, worker: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_jjfx"))
            .args(["deliver", worker])
            .current_dir(&self.root)
            .env_remove("JJ_CONFIG")
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .output()
            .expect("jjfx should run")
    }

    /// Runs `jjfx rebase <name>` from the repository root.
    fn rebase(&self, name: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_jjfx"))
            .args(["rebase", name])
            .current_dir(&self.root)
            .env_remove("JJ_CONFIG")
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .output()
            .expect("jjfx should run")
    }

    /// The trimmed stdout of one `jj` read, for asserting on repository state.
    fn jj_out(&self, args: &[&str]) -> String {
        let output = Command::new("jj")
            .args(["--config", "signing.behavior=drop"])
            .current_dir(&self.root)
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
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// Writes the repository-local settings at `<repo>/.jjfx/config.toml`.
    fn write_repo_config(&self, text: &str) {
        let dir = self.root.join(".jjfx");
        fs::create_dir_all(&dir).expect(".jjfx directory");
        fs::write(dir.join("config.toml"), text).expect("repository config");
    }

    /// Grow the Worker's chain with an empty, undescribed commit below a new
    /// `the top` commit, and move the Worker's bookmark onto it - the shape a
    /// Worker leaves behind when it runs `jj new` without describing the result.
    fn add_empty_undescribed_below_the_top(&self) {
        self.jj_in(&self.workspace, &["new"]);
        fs::write(self.workspace.join("top.txt"), "top\n").expect("top file");
        self.jj_in(&self.workspace, &["commit", "-m", "the top"]);
        self.jj_in(
            &self.workspace,
            &["bookmark", "set", "adam/eng-1", "-r", "@-"],
        );
    }

    /// Grow the Worker's chain with a *non-empty* commit that has no
    /// description below a new `the top` commit. The weld must not drop it - it
    /// carries a change - so jj refuses the push and the delivery must name it.
    fn add_undescribed_change_below_the_top(&self) {
        fs::write(self.workspace.join("hidden.txt"), "hidden\n").expect("hidden file");
        self.jj_in(&self.workspace, &["commit", "-m", ""]);
        fs::write(self.workspace.join("top.txt"), "top\n").expect("top file");
        self.jj_in(&self.workspace, &["commit", "-m", "the top"]);
        self.jj_in(
            &self.workspace,
            &["bookmark", "set", "adam/eng-1", "-r", "@-"],
        );
    }

    /// The Worker's chain above trunk, newest first, as `empty=<bool>
    /// desc=[<description>]` lines - what the weld left behind. Scoped to
    /// mutable commits, so the immutable root is not counted.
    fn chain(&self) -> String {
        let output = Command::new("jj")
            .args(["--config", "signing.behavior=drop"])
            .current_dir(&self.workspace)
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .args([
                "--ignore-working-copy",
                "log",
                "-r",
                "::@- ~ trunk() & mutable()",
                "--no-graph",
                "-T",
                "change_id.short() ++ \" empty=\" ++ empty ++ \" desc=[\" ++ description ++ \"]\\n\"",
            ])
            .output()
            .expect("jj should be installed");
        assert!(
            output.status.success(),
            "jj log: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
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

/// The criterion the ticket exists for: a CLI verb performs the delivery the
/// prompt describes, so an agent driving from the shell lands without running
/// `gh pr create` itself.
///
/// With no forge (`pull_requests = false`) the Pull Request step does not
/// apply, and the delivery is still clean: the change is welded, checked and
/// pushed.
#[test]
fn deliver_pushes_a_change_when_the_repository_has_no_forge() {
    let fixture = DeliverFixture::new(false, "true");

    let output = fixture.deliver("worker-01");

    assert!(
        output.status.success(),
        "a no-forge delivery must succeed: {}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("Delivered worker-01"),
        "{}",
        stderr(&output)
    );
    assert!(
        fixture.origin_refs().contains("refs/heads/adam/eng-1"),
        "the change must reach origin: {}",
        fixture.origin_refs()
    );
}

/// A delivery whose check fails stops before the push and names the check, so
/// the verb reports why rather than a bare failure - and nothing reaches the
/// remote.
#[test]
fn deliver_stops_before_the_push_when_the_check_fails() {
    let fixture = DeliverFixture::new(false, "echo boom-check >&2; exit 1");

    let output = fixture.deliver("worker-01");

    assert!(
        !output.status.success(),
        "a failed check must fail the verb"
    );
    let stderr = stderr(&output);
    assert!(stderr.contains("check:"), "{stderr}");
    assert!(stderr.contains("boom-check"), "{stderr}");
    assert!(
        !fixture.origin_refs().contains("adam/eng-1"),
        "a failed check must stop the push: {}",
        fixture.origin_refs()
    );
}

/// With a forge configured, the verb reaches the Pull Request step: an `origin`
/// that is not a GitHub remote is refused by name, and the run is reported as
/// stopped rather than silently skipping the Pull Request.
///
/// The push precedes the Pull Request step, so the change is already on the
/// remote when the forge refuses - the same order the TUI's `f` key uses.
#[test]
fn deliver_reaches_the_forge_and_reports_a_non_github_origin() {
    let fixture = DeliverFixture::new(true, "true");

    let output = fixture.deliver("worker-01");

    assert!(
        !output.status.success(),
        "a refused forge must fail the verb"
    );
    assert!(
        stderr(&output).contains("origin is not a GitHub remote"),
        "{}",
        stderr(&output)
    );
    assert!(
        fixture.origin_refs().contains("refs/heads/adam/eng-1"),
        "the push precedes the Pull Request step: {}",
        fixture.origin_refs()
    );
}

/// A repository owns its own Pull Request policy: a `[forge]` section in
/// `<repo>/.jjfx/config.toml` is accepted at load (it used to be refused as an
/// unknown section), and the delivery reaches the forge rather than failing on
/// the config. The fixture's `origin` is a local bare repo, so the forge is
/// refused by name - but only after the repository config was accepted.
#[test]
fn deliver_accepts_a_repository_owned_forge_policy() {
    let fixture = DeliverFixture::new(true, "true");
    fixture.write_repo_config("[forge]\ndraft = false\n");

    let output = fixture.deliver("worker-01");

    let stderr = stderr(&output);
    assert!(
        stderr.contains("origin is not a GitHub remote"),
        "a repository-owned [forge] must be accepted and reach the forge: {stderr}"
    );
    assert!(
        !stderr.contains("may set only"),
        "the repository config must not be refused: {stderr}"
    );
}

/// A Worker with no Workspace is refused by name before anything runs.
#[test]
fn deliver_refuses_a_worker_with_no_workspace() {
    let fixture = DeliverFixture::new(false, "true");

    let output = fixture.deliver("worker-99");

    assert!(!output.status.success(), "a missing Workspace must fail");
    assert!(
        fixture.workspace.is_dir(),
        "the fixture's worker-01 Workspace must exist"
    );
    assert!(
        stderr(&output).contains("Workspace directory is missing for Worker worker-99"),
        "{}",
        stderr(&output)
    );
    assert!(
        !fixture.origin_refs().contains("adam/eng-1"),
        "nothing may reach origin when the Worker is unknown"
    );
}

/// A chain carrying an empty, undescribed commit still delivers: the weld drops
/// the commit - it is not a change - rather than carrying it into a push jj
/// refuses.
#[test]
fn deliver_drops_an_empty_undescribed_commit_before_pushing() {
    let fixture = DeliverFixture::new(false, "true");
    fixture.add_empty_undescribed_below_the_top();
    assert!(
        fixture.chain().contains("empty=true"),
        "the fixture must actually carry an empty commit: {}",
        fixture.chain()
    );

    let output = fixture.deliver("worker-01");

    assert!(
        output.status.success(),
        "an empty commit must not stop the delivery: {}",
        stderr(&output)
    );
    assert!(
        fixture.origin_refs().contains("refs/heads/adam/eng-1"),
        "the cleaned chain must reach origin: {}",
        fixture.origin_refs()
    );
    assert!(
        !fixture.chain().contains("empty=true"),
        "the weld must drop the empty commit: {}",
        fixture.chain()
    );
}

/// A commit jj refuses to push stops the delivery and names jj's reason. A
/// *non-empty* commit with no description is the case the weld must not drop -
/// it carries a change - so the refusal is what has to stop the pipeline, before
/// it reaches the Pull Request step with a branch that never reached the remote.
#[test]
fn deliver_stops_and_names_a_commit_jj_refuses_to_push() {
    let fixture = DeliverFixture::new(true, "true");
    fixture.add_undescribed_change_below_the_top();

    let output = fixture.deliver("worker-01");

    assert!(
        !output.status.success(),
        "a refused push must fail the verb: {}",
        stderr(&output)
    );
    let stderr = stderr(&output);
    assert!(
        stderr.contains("push:") && stderr.contains("no description"),
        "the refusal must name jj's reason: {stderr}"
    );
    assert!(
        !fixture.origin_refs().contains("adam/eng-1"),
        "the branch must not reach origin when the push was refused: {}",
        fixture.origin_refs()
    );
    assert!(
        !stderr.contains("origin is not a GitHub remote"),
        "the delivery must stop before the Pull Request step: {stderr}"
    );
}

/// Drive 4's finding 10: a rebase must be onto the trunk the forge has, not the
/// repository's stale view of it.
///
/// A sibling's Pull Request merged on the forge moves `main` there and nowhere
/// else. The repository has not fetched, so its `main` is behind - and a rebase
/// against that `main` rewrites nothing, pushes nothing, and reports success on
/// a Pull Request that is still conflicting.
#[test]
fn rebase_fetches_the_trunk_a_sibling_moved_on_the_forge() {
    let fixture = DeliverFixture::new(false, "true");

    // The sibling's merge: a commit on `main` that exists only on the forge.
    let sibling = fixture.parent.path().join("sibling");
    let origin = fixture.origin.to_string_lossy().to_string();
    let sibling_path = sibling.to_string_lossy().to_string();
    git_in(
        fixture.parent.path(),
        &["clone", "--quiet", &origin, &sibling_path],
    );
    fs::write(sibling.join("sibling.txt"), "merged\n").expect("sibling file");
    git_in(&sibling, &["add", "."]);
    git_in(
        &sibling,
        &[
            "-c",
            "user.name=jjfx test",
            "-c",
            "user.email=jjfx@example.com",
            "commit",
            "-m",
            "a sibling merged",
        ],
    );
    git_in(&sibling, &["push", "--quiet", "origin", "main"]);
    let merged = git_out(&sibling, &["rev-parse", "HEAD"]);

    // The repository's view of trunk is stale: it never fetched the merge.
    assert_ne!(
        fixture.jj_out(&["log", "-r", "main", "--no-graph", "-T", "commit_id"]),
        merged,
        "the fixture must start with a stale trunk, or the test proves nothing"
    );

    let output = fixture.rebase("adam/eng-1");
    assert!(
        output.status.success(),
        "rebase failed: {}",
        stderr(&output)
    );
    assert_eq!(
        fixture.jj_out(&["log", "-r", "main", "--no-graph", "-T", "commit_id"]),
        merged,
        "the rebase must have fetched the trunk the forge has"
    );
    assert!(
        stderr(&output).contains("onto the fetched trunk"),
        "the operator must be told what it rebased onto: {}",
        stderr(&output)
    );
}

/// Runs one `git` command in `dir`, asserting it succeeded.
fn git_in(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git should be installed");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The trimmed stdout of one `git` command in `dir`.
fn git_out(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git should be installed");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}
