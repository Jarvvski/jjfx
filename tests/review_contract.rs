//! The review contract agrees across `jjfx help review`, `docs/review-loop.md`
//! and `docs/configuration.md`'s `[review]` section - and `jjfx review` runs the
//! bounded loop they describe.
//!
//! `dogfood-2`'s finding 04 was that the three surfaces disagreed: the help page
//! denied a bounded loop while the tool advertised one, and no command drove
//! `ReviewLoop`. jjfx owns the loop again, so the surfaces must name the same
//! findings source (the Pull Request's unresolved review threads), the same bound
//! (`[review] max_passes`) and the same escalation - and the command must do what
//! they say: converge without sending when the Pull Request has no unresolved
//! threads, escalate a stalled round, refuse a missing forge or Pull Request by
//! name, and read the threads as they are *now* on every invocation rather than
//! replaying a decision recorded when they may have been different.
#![cfg(unix)]

use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use jjfx::engine::{
    CommitOutcome, Expected, Loaded, PoolCapacity, Repository, StateChange, WireAgent, WireStatus,
    WireTimestamp, WorkerId,
};
use tempfile::TempDir;

/// The rendered `jjfx help <page>`.
fn help(page: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_jjfx"))
        .args(["help", page])
        .output()
        .expect("jjfx help should run");
    assert!(
        output.status.success(),
        "help {page} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The `[review]` section of `docs/configuration.md`, from its heading to the
/// next one.
fn review_configuration_section() -> String {
    let document = include_str!("../docs/configuration.md");
    let start = document
        .find("## `[review]`")
        .expect("configuration.md documents `[review]`");
    let section = &document[start..];
    match section[3..].find("\n## ") {
        Some(end) => section[..end + 3].to_owned(),
        None => section.to_owned(),
    }
}

/// The criterion: help, the review-loop page and the config reference describe
/// the same command - the findings source, the bound and the escalation - and
/// none of them denies the loop the command runs.
#[test]
fn the_help_page_the_docs_and_the_config_agree_on_the_loop() {
    let help = help("review");
    let loop_doc = include_str!("../docs/review-loop.md");
    let config = review_configuration_section();

    let surfaces = [
        ("`jjfx help review`", help.as_str()),
        ("docs/review-loop.md", loop_doc),
        ("docs/configuration.md `[review]`", config.as_str()),
    ];
    for (name, text) in surfaces {
        assert!(
            text.contains("unresolved review threads"),
            "{name} must name the findings source: {text}"
        );
        assert!(
            text.contains("max_passes"),
            "{name} must name the bound: {text}"
        );
        assert!(
            text.contains("escalat"),
            "{name} must state that a stop escalates: {text}"
        );
        assert!(
            !text.contains("no bounded loop"),
            "{name} must not deny the loop the command runs: {text}"
        );
    }

    // The command the surfaces describe is the one the parser accepts.
    assert!(help.contains("Usage: jjfx review <worker>"), "{help}");
}

/// A jj Repository with a Pool and one Worker, plus the fakes `jjfx review`
/// needs on `PATH`.
///
/// The Repository carries its own identity in a repo-local jj config, so a
/// fixture never depends on the developer's `~/.config/jj/config.toml` (absent on
/// CI), and `jjfx` runs with `JJ_CONFIG` dropped and an empty `XDG_CONFIG_HOME`
/// for the same reason.
struct Fixture {
    root: TempDir,
    config_home: TempDir,
    bin: TempDir,
    repository: Repository,
}

impl Fixture {
    /// A Repository with (or without) a GitHub `origin`, and one terminal Worker
    /// reserved for `ENG-301`.
    fn new(with_origin: bool) -> Self {
        let root = TempDir::new().expect("temporary repository");
        let config_home = TempDir::new().expect("temporary config home");
        let bin = TempDir::new().expect("temporary bin");
        jj(root.path(), &["git", "init", "--no-colocate"]);
        if with_origin {
            jj(
                root.path(),
                &[
                    "git",
                    "remote",
                    "add",
                    "origin",
                    "git@github.com:owner/repo.git",
                ],
            );
        }
        fs::create_dir_all(root.path().join(".jj/repo")).expect("repo config directory");
        fs::write(
            root.path().join(".jj/repo/config.toml"),
            "user.name = \"jjfx test\"\nuser.email = \"jjfx@example.com\"\n",
        )
        .expect("repo-local jj identity");
        jj(root.path(), &["bookmark", "create", "main", "-r", "@"]);

        let repository = Repository::open(root.path()).expect("repository");
        let fixture = Self {
            root,
            config_home,
            bin,
            repository,
        };
        let worker = fixture
            .repository
            .worker_pool()
            .resize_to(PoolCapacity::new(1).expect("capacity"))
            .expect("Worker Workspace should be provisioned")
            .added_workers()[0]
            .clone();
        fixture.set_terminal_worker(&worker);
        fixture
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn worker(&self) -> WorkerId {
        self.repository
            .worker_pool()
            .snapshot()
            .workers()
            .first()
            .expect("one Worker")
            .worker_id()
            .clone()
    }

    /// A terminal Worker state with a resumable Session, so an address step can
    /// launch without a real Agent Runtime.
    fn set_terminal_worker(&self, worker: &WorkerId) {
        let prior_log = self.path().join("review-prior.log");
        fs::write(
            &prior_log,
            "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"review-session\"}\n",
        )
        .expect("prior Session log");
        let state_repository = self.repository.state_store().worker(worker.clone());
        let loaded = match state_repository.load().expect("Worker state") {
            Loaded::Present(versioned) => versioned,
            Loaded::Missing => panic!("Worker state should exist"),
        };
        let (mut state, revision) = loaded.into_parts();
        state.status = WireStatus::new("done");
        state.agent = Some(WireAgent::new("claude"));
        state.ticket = Some("ENG-301".to_owned());
        state.branch_name = Some("eng-301".to_owned());
        state.log_file = Some(prior_log.to_string_lossy().into_owned());
        state.started_at = Some(WireTimestamp::new("2026-07-31T10:00:00Z"));
        state.completed_at = Some(WireTimestamp::new("2026-07-31T10:05:00Z"));
        state.exit_code = Some(0);
        let outcome = state_repository
            .commit(Expected::Match(revision), StateChange::Replace(state))
            .expect("terminal Worker state");
        assert!(matches!(outcome, CommitOutcome::Applied(_)));
    }

    fn write_jj(&self) {
        write_executable(
            &self.bin.path().join("jj"),
            concat!(
                "#!/bin/sh\n",
                "case \" $* \" in\n",
                "  *' --version '*) printf 'jj 0.46.0\\n' ;;\n",
                "  *' bookmark list '*) printf 'owner/eng-301-action\\n' ;;\n",
                "esac\n"
            ),
        );
    }

    fn write_gh(&self, script: &str) {
        write_executable(&self.bin.path().join("gh"), script);
    }

    /// A fake `claude` that records the prompt it was handed and exits
    /// successfully, so an address step runs without a real Agent Runtime.
    fn write_claude(&self, capture: &Path) {
        write_executable(
            &self.bin.path().join("claude"),
            &format!(
                "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf '%s\\n' \"$@\" > {}\nprintf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}}'\n",
                capture.display()
            ),
        );
    }

    fn env_path(&self) -> OsString {
        let mut paths = vec![self.bin.path().to_path_buf()];
        paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
        env::join_paths(paths).expect("fixture PATH")
    }

    fn jjfx(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_jjfx"))
            .args(args)
            .current_dir(self.path())
            .env_remove("JJ_CONFIG")
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .env("PATH", self.env_path())
            .output()
            .expect("jjfx should run")
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A Pull Request whose review is a bot's plain comment, not a thread: `gh`
/// reports no unresolved review threads, so the loop converges on the first pass
/// and sends nothing. A pass with no findings never reaches the address step, so
/// no Agent Runtime is even on `PATH` here - a launch would fail the test.
#[test]
fn a_pull_request_with_no_unresolved_threads_converges_without_sending() {
    let fixture = Fixture::new(true);
    fixture.write_jj();
    fixture.write_gh(
        concat!(
            "#!/bin/sh\n",
            "case \"$*\" in\n",
            "  *\"pr list\"*) printf '%s\\n' '[{\"number\":42,\"url\":\"https://example/pr/42\",\"headRefName\":\"owner/eng-301-action\",\"baseRefName\":\"main\",\"mergeable\":\"MERGEABLE\",\"reviewDecision\":\"REVIEW_REQUIRED\"}]' ;;\n",
            "  *\"pr checks\"*) printf '[]\\n' ;;\n",
            "  *\"api graphql\"*) printf '%s\\n' '{\"data\":{\"repository\":{\"pullRequest\":{\"reviewThreads\":{\"nodes\":[]}}}}}' ;;\n",
            "esac\n"
        ),
    );
    let worker = fixture.worker();

    let output = fixture.jjfx(&["review", worker.as_str(), "--fg"]);

    assert!(
        output.status.success(),
        "a converged review is success: {}",
        stderr(&output)
    );
    let reported = stderr(&output);
    assert!(
        reported.contains("stopped on convergence"),
        "the stop must be reported: {reported}"
    );
    assert!(
        reported.contains("0 finding(s)"),
        "a converged pass found nothing: {reported}"
    );
}

/// A round that sends findings but sees the same unresolved threads next pass
/// has made no progress: the loop stops on `NoProgress`, escalates the findings
/// it could not resolve, and exits non-zero - it never reports success. The
/// address step sends the pass's findings to the Worker, and the captured prompt
/// proves it.
#[test]
fn a_stalled_round_escalates_and_exits_non_zero() {
    let fixture = Fixture::new(true);
    fixture.write_jj();
    fixture.write_gh(
        concat!(
            "#!/bin/sh\n",
            "case \"$*\" in\n",
            "  *\"pr list\"*) printf '%s\\n' '[{\"number\":42,\"url\":\"https://example/pr/42\",\"headRefName\":\"owner/eng-301-action\",\"baseRefName\":\"main\",\"mergeable\":\"MERGEABLE\",\"reviewDecision\":\"CHANGES_REQUESTED\"}]' ;;\n",
            "  *\"pr checks\"*) printf '[]\\n' ;;\n",
            "  *\"api graphql\"*) printf '%s\\n' '{\"data\":{\"repository\":{\"pullRequest\":{\"reviewThreads\":{\"nodes\":[{\"isResolved\":false,\"path\":\"src/x.rs\",\"startLine\":12,\"line\":14,\"comments\":{\"nodes\":[{\"author\":{\"login\":\"adam\"},\"body\":\"Cap the loop.\"}]}}]}}}}}' ;;\n",
            "esac\n"
        ),
    );
    let capture = fixture.path().join("review-prompt");
    fixture.write_claude(&capture);
    let worker = fixture.worker();

    let output = fixture.jjfx(&["review", worker.as_str(), "--fg"]);

    assert!(
        !output.status.success(),
        "a stalled round must not report success: {}",
        stderr(&output)
    );
    let reported = stderr(&output);
    assert!(
        reported.contains("did not converge"),
        "the escalation must be named: {reported}"
    );
    assert!(
        reported.contains("no progress"),
        "the stop must be named: {reported}"
    );
    assert!(
        reported.contains("unresolved [none] src/x.rs:12-14 - Cap the loop."),
        "the unresolved finding must be escalated: {reported}"
    );
    let prompt = fs::read_to_string(&capture).expect("the address step ran");
    assert!(
        prompt.contains("unresolved review threads"),
        "the address step sends the findings: {prompt}"
    );
    assert!(
        prompt.contains("Cap the loop."),
        "the address step sends the finding's thread: {prompt}"
    );
}

/// The rule this bug breaks: a recorded pass must not outlive the review state it
/// decided on. A review thread opened *after* a converged pass is read by the next
/// `jjfx review`, so the loop does not report convergence on the replay alone.
#[test]
fn a_thread_opened_after_a_converged_pass_is_read_by_the_next_review() {
    let fixture = Fixture::new(true);
    fixture.write_jj();
    let reads = fixture.path().join("graphql-reads");
    let threads = fixture.path().join("review-threads.json");
    fixture.write_gh(&format!(
        "#!/bin/sh\ncase \"$*\" in\n  *\"pr list\"*) printf '%s\\n' '[{{\"number\":42,\"url\":\"https://example/pr/42\",\"headRefName\":\"owner/eng-301-action\",\"baseRefName\":\"main\",\"mergeable\":\"MERGEABLE\",\"reviewDecision\":\"REVIEW_REQUIRED\"}}]' ;;\n  *\"pr checks\"*) printf '[]\\n' ;;\n  *\"api graphql\"*) printf 'read\\n' >> {}; cat {} ;;\nesac\n",
        reads.display(),
        threads.display()
    ));
    let capture = fixture.path().join("review-prompt");
    fixture.write_claude(&capture);
    let worker = fixture.worker();

    // The Pull Request has no review threads yet, so the first drive converges.
    fs::write(
        &threads,
        "{\"data\":{\"repository\":{\"pullRequest\":{\"reviewThreads\":{\"nodes\":[]}}}}}\n",
    )
    .expect("no review threads yet");
    let first = fixture.jjfx(&["review", worker.as_str(), "--fg"]);
    assert!(first.status.success(), "{}", stderr(&first));
    assert!(
        stderr(&first).contains("stopped on convergence"),
        "the first drive converges on a Pull Request with no threads: {}",
        stderr(&first)
    );
    assert_eq!(
        fs::read_to_string(&reads).expect("the first drive reads the threads"),
        "read\n"
    );

    // A reviewer then opens a thread on the same Pull Request, with no push.
    fs::write(
        &threads,
        "{\"data\":{\"repository\":{\"pullRequest\":{\"reviewThreads\":{\"nodes\":[{\"isResolved\":false,\"path\":\"src/x.rs\",\"startLine\":12,\"line\":14,\"comments\":{\"nodes\":[{\"author\":{\"login\":\"adam\"},\"body\":\"Cap the loop.\"}]}}]}}}}}\n",
    )
    .expect("a new review thread");

    let second = fixture.jjfx(&["review", worker.as_str(), "--fg"]);
    let reported = stderr(&second);
    assert!(
        !second.status.success(),
        "the loop must not report success while a thread is open: {reported}"
    );
    assert!(
        !reported.contains("stopped on convergence"),
        "a later review must not report convergence on a replayed pass: {reported}"
    );
    assert!(
        reported.contains("did not converge"),
        "the thread opened after the first drive must escalate: {reported}"
    );
    assert!(
        !reported.contains("replayed from the decision log"),
        "a later review must not be served from an earlier decision: {reported}"
    );
    assert!(
        fs::read_to_string(&reads)
            .expect("the capture survives")
            .lines()
            .count()
            > 1,
        "the second review must read the current threads rather than replay"
    );
}

/// A Worker whose bookmark has no Pull Request is refused by name, before any
/// pass runs.
#[test]
fn a_worker_with_no_pull_request_is_refused_by_name() {
    let fixture = Fixture::new(true);
    fixture.write_jj();
    fixture.write_gh("#!/bin/sh\ncase \"$*\" in\n  *\"pr list\"*) printf '[]\\n' ;;\nesac\n");
    let worker = fixture.worker();

    let output = fixture.jjfx(&["review", worker.as_str(), "--fg"]);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("no pull request found for branch owner/eng-301-action"),
        "the refusal must name the missing Pull Request: {}",
        stderr(&output)
    );
}

/// A Repository with no forge has no Pull Request to read, so `review` refuses
/// by name rather than converging on an empty review.
#[test]
fn a_repository_with_no_forge_is_refused_by_name() {
    let fixture = Fixture::new(false);
    fixture.write_jj();
    fixture.write_gh("#!/bin/sh\nexit 0\n");
    let worker = fixture.worker();

    let output = fixture.jjfx(&["review", worker.as_str(), "--fg"]);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("cannot detect GitHub repository"),
        "the refusal must name the missing forge: {}",
        stderr(&output)
    );
}

/// Run one jj command against a fixture repository, asserting success.
fn jj(dir: &Path, args: &[&str]) {
    let output = Command::new("jj")
        .args([
            "--config",
            "signing.behavior=drop",
            "--config",
            "user.name=jjfx test",
            "--config",
            "user.email=jjfx@example.com",
        ])
        .args(args)
        .current_dir(dir)
        .env_remove("JJ_CONFIG")
        .output()
        .expect("jj should be installed");
    assert!(
        output.status.success(),
        "jj {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("fake executable");
    let mut permissions = fs::metadata(path)
        .expect("fake executable metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("fake executable permissions");
}
