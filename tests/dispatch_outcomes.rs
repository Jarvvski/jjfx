//! Dispatch must not report success when nothing happened, and must say what it
//! did with a Ticket.
//!
//! Each case drives the real binary, because the point is the exit code and the
//! output a scripting caller sees: a batch where every Ticket failed must exit
//! non-zero, a batch that dispatched (or held back) a Ticket must exit zero, a
//! background orchestration that fails must be reported as a failure rather than
//! as `orchestrating`, and a Ticket named by hand that is not ready is dispatched
//! anyway with the reason it was not ready reported first. One case is about the
//! path an agent drives: a Dispatch issued from inside an opencode session must
//! launch a Worker, not fail every launch on the anchor opencode inherits.
#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use jjfx::engine::{Repository, TicketId};
use tempfile::TempDir;

mod support;

/// A jj repository with one local ready Ticket and a `[tickets] source = "local"`
/// config home. The Pool and remote are added per case, because they are what
/// decide which phase a Dispatch fails in.
struct DispatchFixture {
    root: TempDir,
    config_home: TempDir,
    bin: TempDir,
    /// `PATH` with the fixture's `bin` ahead of the inherited one, so the stub
    /// `kitten` (and any fake Agent) is what the binary under test finds.
    env_path: OsString,
}

impl DispatchFixture {
    fn new() -> Self {
        let root = TempDir::new().expect("temporary repository");
        let config_home = TempDir::new().expect("temporary config home");
        let bin = TempDir::new().expect("temporary bin");
        // A stub terminal on `PATH` keeps Dispatch deterministic: without it the
        // Dispatch Eligibility terminal check fails closed when no kitty can be
        // queried (ADR 0011), so every test here would need `--force` to pass on
        // CI. The stub answers `kitten @ ls` with an empty workspace list.
        let env_path = support::stub_terminal(bin.path());
        let fixture = Self {
            root,
            config_home,
            bin,
            env_path,
        };
        fixture.jj(&["git", "init", "--no-colocate"]);
        fixture.write_ticket(
            "widgets",
            "01-ship-the-widget.md",
            "# Ship the widget\n\nStatus: ready-for-agent\n\n## Problem\n\nDo it.\n",
        );
        fixture.write_config("[tickets]\nsource = \"local\"\n");
        fixture
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    /// The jj config file the binary under test reads its delivery identity from.
    ///
    /// It is passed as `JJ_CONFIG` rather than set with `jj config set --repo`:
    /// jj stores repo config under a directory derived from `XDG_CONFIG_HOME`,
    /// which this fixture points at its own config home, so a `--repo` write
    /// would land where the binary never looks (and a developer's own user
    /// config would hide that).
    fn jj_config_path(&self) -> std::path::PathBuf {
        self.config_home.path().join("jj.toml")
    }

    fn jj(&self, args: &[&str]) -> Output {
        Command::new("jj")
            .args(["--config", "signing.behavior=drop"])
            .args(args)
            .current_dir(self.path())
            .output()
            .expect("jj should be installed")
    }

    fn write_ticket(&self, feature: &str, file: &str, content: &str) {
        let directory = self.path().join(".scratch").join(feature).join("issues");
        fs::create_dir_all(&directory).expect("issues directory");
        fs::write(directory.join(file), content).expect("Ticket file");
    }

    fn write_config(&self, content: &str) {
        let directory = self.config_home.path().join("jjfx");
        fs::create_dir_all(&directory).expect("config directory");
        fs::write(directory.join("config.toml"), content).expect("config file");
        fs::write(
            self.jj_config_path(),
            "user.name = \"Test User\"\nuser.email = \"test@example.com\"\n",
        )
        .expect("jj config file");
    }

    /// Writes the Repository's own `.jjfx/config.toml` - the file that may
    /// select a Ticket source for this Repository alone.
    fn write_repo_config(&self, content: &str) {
        let directory = self.path().join(".jjfx");
        fs::create_dir_all(&directory).expect("repository config directory");
        fs::write(directory.join("config.toml"), content).expect("repository config file");
    }

    /// Sizes the Worker Pool, so Dispatch reaches a real per-Ticket outcome
    /// rather than failing on a missing Pool.
    fn with_pool(&self, size: usize) {
        let output = self
            .jjfx(&["pool", &size.to_string()])
            .output()
            .expect("pool");
        assert!(output.status.success(), "{}", stderr(&output));
    }

    /// Gives the repository an `origin` remote, so Dispatch can resolve the
    /// owner/name identity a Pull Request targets.
    fn with_origin(&self) {
        assert!(
            self.jj(&[
                "git",
                "remote",
                "add",
                "origin",
                "git@github.com:owner/name.git"
            ])
            .status
            .success()
        );
    }

    /// Puts a fake `claude` on the fixture's `PATH` that exits successfully, so a
    /// Dispatch can launch a Run without a real Agent Runtime.
    fn with_fake_agent(&self) {
        let agent = self.bin.path().join("claude");
        fs::write(
            &agent,
            "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n",
        )
        .expect("fake agent");
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).expect("executable agent");
    }

    /// Puts a fake `opencode` on the fixture's `PATH` that serves the session API
    /// the way a real opencode does: a session created with a `parentID` is
    /// anchored to its **parent's** directory, while a root session is anchored to
    /// the directory the request asked for, and a read-back reports the anchor
    /// opencode stored.
    ///
    /// The dispatching session `ses_dispatcher` is anchored to the Repository,
    /// which is where an agent issuing a Dispatch from its own shell runs - not
    /// the Worker Workspace it is dispatching into.
    fn with_fake_opencode(&self) {
        let script = OPENCODE_SESSION_SCRIPT
            .replace(
                "__CREATE_BODY__",
                &self.opencode_create_body().display().to_string(),
            )
            .replace(
                "__PROMPT_BODY__",
                &self.opencode_prompt_body().display().to_string(),
            )
            .replace(
                "__SESSION_DIRECTORY__",
                &self.opencode_session_directory().display().to_string(),
            )
            .replace("__DISPATCHER__", &self.path().display().to_string());
        let agent = self.bin.path().join("opencode");
        fs::write(&agent, script).expect("fake opencode");
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o755))
            .expect("executable opencode");
    }

    /// Where the fake `opencode` records the `session.create` request body.
    fn opencode_create_body(&self) -> std::path::PathBuf {
        self.bin.path().join("opencode-create-body")
    }

    /// Where the fake `opencode` records the `session.prompt` request body.
    fn opencode_prompt_body(&self) -> std::path::PathBuf {
        self.bin.path().join("opencode-prompt-body")
    }

    /// Where the fake `opencode` remembers the directory it anchored a session
    /// to, so a read-back reports the anchor opencode stored rather than the one
    /// the request asked for.
    fn opencode_session_directory(&self) -> std::path::PathBuf {
        self.bin.path().join("opencode-session-directory")
    }

    /// The single Worker Workspace a pool of one provisioned.
    fn worker_workspace(&self) -> std::path::PathBuf {
        let name = self
            .path()
            .file_name()
            .expect("repository name")
            .to_string_lossy()
            .into_owned();
        let base = self
            .path()
            .parent()
            .expect("workspace base parent")
            .join(format!("{name}-workspaces"));
        let mut workspaces: Vec<std::path::PathBuf> = fs::read_dir(&base)
            .expect("workspace base")
            .map(|entry| entry.expect("Workspace entry").path())
            .collect();
        workspaces.sort();
        assert_eq!(
            workspaces.len(),
            1,
            "a pool of one provisions one Worker Workspace"
        );
        workspaces.pop().expect("one Worker Workspace")
    }

    /// Puts a fake `claude` on the fixture's `PATH` that answers a Ticket query
    /// with an empty ready set.
    ///
    /// A Linear source is queried by shelling out to the configured Agent
    /// Runtime, so this stubs the Runtime rather than Linear itself: the query
    /// returns `{"tickets":[]}`, the shape a real query returns when the label
    /// matched nothing. That reaches the empty-ready-set branch without Linear
    /// credentials, a network call, or a real Agent.
    fn with_empty_ready_query(&self) {
        let agent = self.bin.path().join("claude");
        fs::write(
            &agent,
            concat!(
                "#!/bin/sh\n",
                "if [ \"$1\" = \"--help\" ]; then exit 0; fi\n",
                "printf '%s\\n' '{\"result\":\"{\\\"tickets\\\":[]}\"}'\n",
            ),
        )
        .expect("fake agent");
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).expect("executable agent");
    }

    fn jjfx(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_jjfx"));
        command
            .args(args)
            .current_dir(self.path())
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .env("JJ_CONFIG", self.jj_config_path())
            .env("PATH", &self.env_path);
        command
    }

    /// Runs `dispatch`, retrying a Dispatch that failed closed because the stub
    /// terminal was starved past its deadline under load - see
    /// [`support::run_jjfx_tolerating_a_starved_terminal`].
    fn dispatch(&self, args: &[&str]) -> Output {
        support::run_jjfx_tolerating_a_starved_terminal(|| self.jjfx(args))
    }

    /// Runs `dispatch` as if issued from the opencode session `session_id`, which
    /// is how opencode hands a session's id to a command it runs.
    fn dispatch_from_session(&self, session_id: &str, args: &[&str]) -> Output {
        support::run_jjfx_tolerating_a_starved_terminal(|| {
            let mut command = self.jjfx(args);
            command.env("OPENCODE_SESSION_ID", session_id);
            command
        })
    }
}

/// A fake `opencode` that serves the session API the way a real opencode does:
/// a child session inherits its parent's directory, a root session is anchored to
/// the directory the request asked for, and a read-back reports the anchor
/// opencode stored. `session.get` reports the dispatching session anchored to
/// `__DISPATCHER__`.
const OPENCODE_SESSION_SCRIPT: &str = r#"#!/bin/sh
case "$1" in
  --version) printf '%s\n' '2.0.29'; exit 0 ;;
  api)
    case "$2" in
      session.create)
        printf '%s' "$4" > "__CREATE_BODY__"
        case "$4" in
          *parentID*) directory="__DISPATCHER__" ;;
          *) directory=$(printf '%s' "$4" | sed -n 's/.*"directory":"\([^"]*\)".*/\1/p') ;;
        esac
        printf '%s' "$directory" > "__SESSION_DIRECTORY__"
        printf '{"data":{"id":"ses_new","location":{"directory":"%s"}}}' "$directory"
        exit 0 ;;
      session.get)
        case "$4" in
          sessionID=ses_dispatcher)
            printf '{"data":{"id":"ses_dispatcher","location":{"directory":"__DISPATCHER__"}}}'
            exit 0 ;;
        esac
        printf '{"data":{"id":"ses_new","location":{"directory":"%s"}}}' "$(cat "__SESSION_DIRECTORY__")"
        exit 0 ;;
      session.prompt)
        printf '%s' "$6" > "__PROMPT_BODY__"
        printf '{"data":{"id":"msg_1"}}'
        exit 0 ;;
      session.remove)
        exit 0 ;;
    esac
    exit 1 ;;
  run)
    exit 0 ;;
esac
exit 1
"#;

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_batch_where_every_ticket_failed_exits_non_zero() {
    let fixture = DispatchFixture::new();
    fixture.with_pool(1);
    // No origin remote, so the only Ticket fails at the identity phase - the
    // exact shape that used to exit 0.
    let output = fixture.dispatch(&["dispatch", "--all", "--json"]);

    assert!(
        !output.status.success(),
        "an all-failed batch must exit non-zero:\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        stdout(&output).contains("\"outcome\": \"failed\""),
        "{}",
        stdout(&output)
    );
    assert!(
        stderr(&output).contains("every Ticket failed to dispatch"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_batch_with_a_dispatched_ticket_exits_zero() {
    let fixture = DispatchFixture::new();
    fixture.with_pool(1);
    fixture.with_origin();
    fixture.with_fake_agent();
    let output = fixture.dispatch(&["dispatch", "--all", "--json"]);

    assert!(
        output.status.success(),
        "a dispatched Ticket is a success:\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        stdout(&output).contains("\"outcome\": \"dispatched\""),
        "{}",
        stdout(&output)
    );
}

/// The wall this removes: a Dispatch issued from inside an opencode session -
/// the documented way an agent drives jjfx - launched no Worker at all.
///
/// jjfx created each Worker's session as a **child** of the dispatching session,
/// and opencode anchors a child to its parent's directory. The dispatching agent
/// runs in the Repository, so the Worker Workspace anchor assertion failed every
/// launch and the only workaround was to unset `OPENCODE_SESSION_ID`, giving up
/// the child-session topology. The Run's session is now a root session anchored to
/// the Worker Workspace whenever a child could not be, so the agent-driven path
/// dispatches.
#[test]
fn a_dispatch_from_an_opencode_session_launches_a_worker() {
    let fixture = DispatchFixture::new();
    fixture.with_pool(1);
    fixture.with_origin();
    let workspace = fixture.worker_workspace();
    fixture.with_fake_opencode();
    let profile = fixture
        .jjfx(&["pool", "profile", "opencode"])
        .output()
        .expect("pool profile");
    assert!(profile.status.success(), "{}", stderr(&profile));

    let output = fixture.dispatch_from_session("ses_dispatcher", &["dispatch", "--all", "--json"]);

    assert!(
        output.status.success(),
        "a Dispatch from an opencode session must launch a Worker:\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        stdout(&output).contains("\"outcome\": \"dispatched\""),
        "{}",
        stdout(&output)
    );

    // The Run's session is a root session anchored to the Worker Workspace: the
    // dispatching session runs in the Repository, and a child session would have
    // inherited that directory instead.
    let body: serde_json::Value = serde_json::from_slice(
        &fs::read(fixture.opencode_create_body()).expect("recorded session.create body"),
    )
    .expect("session.create body JSON");
    assert!(
        body.get("parentID").is_none(),
        "a dispatching session outside the Workspace must not own the Run's session: {body}"
    );
    let anchored = body["location"]["directory"]
        .as_str()
        .expect("the session names its directory");
    assert_eq!(
        Path::new(anchored)
            .canonicalize()
            .expect("session directory canonical"),
        workspace.canonicalize().expect("workspace canonical"),
        "the Run's session must be anchored to the Worker Workspace"
    );

    // The prompt reached the session, so the Run started without a client process.
    assert!(
        fixture.opencode_prompt_body().exists(),
        "the Run should deliver its prompt to the session it created"
    );
}

/// The wall this removes: a Repository selects `local` for itself, in its own
/// `.jjfx/config.toml`, with the user config left at the `linear` default - no
/// faked `XDG_CONFIG_HOME` needed.
#[test]
fn a_repository_selects_its_ticket_source_without_touching_the_user_config() {
    let fixture = DispatchFixture::new();
    // The user config carries jjfx's defaults (`source = "linear"`); the
    // selection lives in the Repository alone.
    fixture.write_config("");
    fixture.write_repo_config("[tickets]\nsource = \"local\"\n");
    fixture.with_pool(1);
    fixture.with_origin();
    fixture.with_fake_agent();
    let output = fixture.dispatch(&["dispatch", "--all", "--json"]);

    assert!(
        output.status.success(),
        "a Repository-local source selection must dispatch:\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        stdout(&output).contains("\"outcome\": \"dispatched\""),
        "{}",
        stdout(&output)
    );
}

/// A Repository may select a source and may not define one: a repository-local
/// attempt to set a source's details is a loud error naming the file.
#[test]
fn a_repository_defining_a_ticket_source_fails_loudly_naming_the_file() {
    let fixture = DispatchFixture::new();
    fixture.write_config("");
    fixture.write_repo_config("[tickets.local]\nroot = \".tickets\"\n");
    let output = fixture
        .jjfx(&["ticket", "reconcile"])
        .output()
        .expect("jjfx should run");

    assert!(
        !output.status.success(),
        "defining a source must fail:\nstdout:\n{}",
        stdout(&output)
    );
    let error = stderr(&output);
    assert!(error.contains("config.toml"), "{error}");
    assert!(error.contains("local"), "{error}");
}

#[test]
fn a_background_orchestration_without_a_pool_is_a_failure_not_orchestrating() {
    let fixture = DispatchFixture::new();
    // No Pool at all: the child used to die at once behind a printed
    // `orchestrating`. The parent must surface the error and exit non-zero.
    let output = fixture.dispatch(&["dispatch", "widgets--01-ship-the-widget", "--json"]);

    assert!(!output.status.success(), "{}", stdout(&output));
    assert!(
        !stdout(&output).contains("orchestrating"),
        "a dead orchestration must not print orchestrating:\n{}",
        stdout(&output)
    );
    assert!(
        stderr(&output).contains("Worker Pool is missing"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_background_orchestration_for_an_unresolvable_ticket_is_a_failure() {
    let fixture = DispatchFixture::new();
    fixture.with_pool(1);
    let output = fixture.dispatch(&["dispatch", "nope-typo", "--json"]);

    assert!(!output.status.success(), "{}", stdout(&output));
    assert!(
        !stdout(&output).contains("orchestrating"),
        "a Ticket that cannot be resolved must not print orchestrating:\n{}",
        stdout(&output)
    );
    let error = stderr(&output);
    assert!(error.contains("cannot fetch Ticket nope-typo"), "{error}");
    // The hint is keyed off the configured source: a local repository never
    // consults Linear, so the message must name the root it searched instead.
    assert!(!error.contains("Linear"), "{error}");
    assert!(error.contains(".scratch"), "{error}");
}

#[test]
fn a_background_orchestration_for_a_local_feature_parent_names_the_child_log() {
    let fixture = DispatchFixture::new();
    // The origin must precede the Worker Pool, so it records the owner/name
    // identity orchestration resolves before it discovers children.
    fixture.with_origin();
    fixture.with_pool(1);
    // The feature is a Parent directory, not a Ticket file. Its one child is
    // already resolved, so the group is terminal and the child exits without
    // launching an Agent Runtime.
    fixture.write_ticket("gadgets", "01-done.md", "# Done\n\nStatus: resolved\n");

    // No `--force`: the fixture's stub `kitten` answers the Dispatch Eligibility
    // terminal check (ADR 0011) with an empty workspace list, so this exercises
    // the real seam without depending on the ambient terminal. The runner
    // retries the one transient that seam can hit under load.
    let output = fixture.dispatch(&["dispatch", "gadgets", "--json"]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let json = stdout(&output);
    assert!(json.contains("\"outcome\": \"orchestrating\""), "{json}");
    let log = fixture
        .path()
        .join(".jj/pools/default/orchestrate-gadgets.log");
    assert!(
        json.contains(&log.display().to_string()),
        "the JSON must name the child log:\n{json}"
    );
    assert!(
        log.is_file(),
        "the child log must exist at {}",
        log.display()
    );
    // The handoff came from real local discovery: the persisted group names the
    // feature's child rather than a pre-persisted placeholder.
    let group = match Repository::open(fixture.path())
        .expect("open repository")
        .state_store()
        .dispatch_group(TicketId::parse("gadgets").expect("Parent Ticket"))
        .load()
        .expect("load group")
    {
        jjfx::engine::Loaded::Present(value) => value.value,
        jjfx::engine::Loaded::Missing => panic!("a local feature Parent must persist a group"),
    };
    assert!(
        group
            .sub_issues
            .contains_key(&TicketId::parse("gadgets--01-done").expect("child Ticket")),
        "{:?}",
        group.sub_issues
    );
}

#[test]
fn a_named_ticket_blocked_by_an_unresolved_sibling_says_why_and_dispatches_anyway() {
    let fixture = DispatchFixture::new();
    fixture.with_origin();
    fixture.with_pool(1);
    // 01 is ready but not resolved, so 02 is blocked by an unresolved sibling.
    fixture.write_ticket(
        "widgets",
        "02-dependent.md",
        "# Dependent\n\nStatus: ready-for-agent\nBlocked by: 01\n",
    );
    fixture.with_fake_agent();

    // Default Dispatch orchestrates a single named Ticket, which falls back to a
    // direct Dispatch because the Ticket is not a feature directory - so this
    // covers the path `jjfx dispatch <TICKET>` actually takes.
    let output = fixture.dispatch(&["dispatch", "widgets--02-dependent"]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let error = stderr(&output);
    // The reason comes from the same seam `--all` uses, and naming the Ticket is
    // the override, so the Dispatch proceeds rather than refusing.
    assert!(
        error.contains(
            "Ticket widgets--02-dependent is not ready: blocked by widgets--01-ship-the-widget (ready-for-agent); dispatching anyway"
        ),
        "{error}"
    );
    assert!(
        error.contains("-> widgets--02-dependent"),
        "the named Ticket must still be dispatched:\n{error}"
    );
}

#[test]
fn a_named_ticket_with_the_wrong_status_reports_it_and_dispatches_anyway() {
    let fixture = DispatchFixture::new();
    fixture.with_origin();
    fixture.with_pool(1);
    // The Ticket is in flight, so its `Status:` is not the ready status.
    fixture.write_ticket(
        "widgets",
        "01-ship-the-widget.md",
        "# Ship the widget\n\nStatus: claimed\n\n## Problem\n\nDo it.\n",
    );
    fixture.with_fake_agent();

    let output = fixture.dispatch(&[
        "dispatch",
        "widgets--01-ship-the-widget",
        "--no-orchestrate",
        "--json",
    ]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let error = stderr(&output);
    assert!(
        error.contains(
            "Ticket widgets--01-ship-the-widget is not ready: expected status \"ready-for-agent\", found \"claimed\"; dispatching anyway"
        ),
        "{error}"
    );
    // The reason is a diagnostic on stderr; the dispatch result on stdout is one
    // object for the Ticket that was launched anyway.
    let json = stdout(&output);
    assert!(
        json.contains("\"ticket\": \"widgets--01-ship-the-widget\""),
        "{json}"
    );
    assert!(json.contains("\"outcome\": \"dispatched\""), "{json}");
}

#[test]
fn a_ready_named_ticket_is_not_reported_as_not_ready() {
    let fixture = DispatchFixture::new();
    fixture.with_origin();
    fixture.with_pool(1);
    fixture.with_fake_agent();

    let output = fixture.dispatch(&["dispatch", "widgets--01-ship-the-widget"]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let error = stderr(&output);
    assert!(
        !error.contains("is not ready"),
        "a ready Ticket must not be reported as not ready:\n{error}"
    );
}

/// One pool implies it: a Repository that declares no pools behaves exactly as
/// it did before pools could be named, so an unqualified Dispatch fills the
/// implicit `default` lane.
#[test]
fn an_unqualified_dispatch_fills_the_only_pool() {
    let fixture = DispatchFixture::new();
    fixture.with_origin();
    fixture.with_pool(1);
    fixture.with_fake_agent();

    let output = fixture.dispatch(&[
        "dispatch",
        "widgets--01-ship-the-widget",
        "--no-orchestrate",
    ]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        fixture.path().join(".jj/pools/default").is_dir(),
        "the single pool is the implicit `default`"
    );
}

/// Several pools require `--pool`: an unqualified Dispatch is refused loudly,
/// naming the pools that exist, never silently filling one. The refusal comes
/// before any Pool or Worker mutation, so nothing is reserved or launched.
#[test]
fn a_dispatch_into_a_repository_with_several_pools_is_refused_naming_them() {
    let fixture = DispatchFixture::new();
    fixture.write_repo_config("[pools.bugs]\n");

    let output = fixture.dispatch(&["dispatch", "widgets--01-ship-the-widget"]);

    assert!(
        !output.status.success(),
        "an unqualified Dispatch into several pools must refuse:\nstdout:\n{}",
        stdout(&output)
    );
    let error = stderr(&output);
    assert!(error.contains("default"), "{error}");
    assert!(error.contains("bugs"), "{error}");
    assert!(error.contains("--pool"), "{error}");

    // `--text` and `--from-file` follow the same rule, because they resolve a
    // pool through the same seam.
    let ad_hoc = fixture.dispatch(&["dispatch", "--text", "fix the bug"]);
    assert!(
        !ad_hoc.status.success(),
        "an unqualified --text Dispatch must refuse too:\nstdout:\n{}",
        stdout(&ad_hoc)
    );
    let error = stderr(&ad_hoc);
    assert!(error.contains("default"), "{error}");
    assert!(error.contains("bugs"), "{error}");
}

/// `--pool` selects the lane: reservation, eligibility and size are all read
/// from the named pool, so a Dispatch into a named pool fails on that pool
/// rather than quietly using `default`'s Worker.
#[test]
fn an_explicit_pool_fills_that_lane_not_default() {
    let fixture = DispatchFixture::new();
    fixture.with_origin();
    // `default` is sized and ready; `bugs` is declared with no size and no state.
    fixture.with_pool(1);
    fixture.write_repo_config("[pools.bugs]\n");
    fixture.with_fake_agent();

    // Naming `bugs` materialises that lane's own state - empty, because the
    // declaration names no size - and fails for want of an idle Worker there
    // rather than falling back to `default`'s idle one.
    let bugs = fixture.dispatch(&[
        "dispatch",
        "widgets--01-ship-the-widget",
        "--pool",
        "bugs",
        "--no-orchestrate",
    ]);
    assert!(
        !bugs.status.success(),
        "a Dispatch into a lane with no idle Worker must fail:\nstdout:\n{}",
        stdout(&bugs)
    );
    let bugs_state =
        fs::read_to_string(fixture.path().join(".jj/pools/bugs/pool.json")).expect("bugs state");
    assert!(bugs_state.contains("\"size\": 0"), "{bugs_state}");
    assert!(
        bugs_state.contains("\"workers\": []") || bugs_state.contains("\"workers\":[]"),
        "the declared lane materialises with no Workers of its own: {bugs_state}"
    );

    // Naming `default` fills the lane that is provisioned.
    let default = fixture.dispatch(&[
        "dispatch",
        "widgets--01-ship-the-widget",
        "--pool",
        "default",
        "--no-orchestrate",
    ]);
    assert!(
        default.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&default),
        stderr(&default)
    );
}

/// When nothing is ready, the `--json` batch explains why instead of printing a
/// bare `[]`: the label it searched and the root it searched under, so a caller
/// can tell a wrong root or a label that matched nothing from a Repository that
/// simply has no work.
#[test]
fn an_empty_ready_set_carries_the_search_in_json() {
    let fixture = DispatchFixture::new();
    fixture.with_pool(1);
    // The only Ticket is finished, so the ready set is genuinely empty and no
    // per-Ticket diagnostic explains it - exactly the case that used to be `[]`.
    fixture.write_ticket(
        "widgets",
        "01-ship-the-widget.md",
        "# Ship the widget\n\nStatus: resolved\n",
    );

    let output = fixture.dispatch(&["dispatch", "--all", "--json"]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let json: serde_json::Value =
        serde_json::from_str(stdout(&output).trim()).expect("a JSON batch");
    let entries = json.as_array().expect("a batch is an array");
    let search = entries
        .iter()
        .find(|entry| entry["ticket"].is_null())
        .expect("the batch names the search that found nothing");
    assert_eq!(search["outcome"], "skipped");
    assert_eq!(search["label"], "ready-for-agent");
    let root = search["root"]
        .as_str()
        .expect("the search names the root it looked under");
    assert!(root.ends_with(".scratch"), "{root}");

    // The prose path is unchanged: it still names the label on stderr.
    let prose = fixture.dispatch(&["dispatch", "--all"]);
    assert!(prose.status.success(), "{}", stderr(&prose));
    assert!(
        stderr(&prose).contains("No tickets found with label 'ready-for-agent'"),
        "{}",
        stderr(&prose)
    );
}

/// A Linear source has no filesystem root to name, so its search entry carries
/// `root: null` rather than a path - the shape a caller reads without checking.
///
/// This drives the config seam that selects the source - `[tickets] source =
/// "linear"` - and asserts the emitted JSON. The query itself is not exercised:
/// a real Linear query needs Linear credentials and a reachable Agent Runtime,
/// neither of which a test suite has, so the Runtime is stubbed with an empty
/// ready set. What this proves is that the source selection reaches
/// `search_entry` and that a Linear source emits a null root end to end; it is
/// not evidence that jjfx can discover real Linear Tickets.
#[test]
fn a_linear_source_reports_a_null_search_root_in_json() {
    let fixture = DispatchFixture::new();
    fixture.write_config("[tickets]\nsource = \"linear\"\n");
    fixture.with_pool(1);
    fixture.with_empty_ready_query();

    let output = fixture.dispatch(&["dispatch", "--all", "--json"]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let json: serde_json::Value =
        serde_json::from_str(stdout(&output).trim()).expect("a JSON batch");
    let entries = json.as_array().expect("a batch is an array");
    let search = entries
        .iter()
        .find(|entry| entry["ticket"].is_null())
        .expect("the batch names the search that found nothing");
    assert_eq!(search["outcome"], "skipped");
    assert_eq!(search["label"], "ready-for-agent");
    assert!(
        search["root"].is_null(),
        "a Linear source names no filesystem root: {json}"
    );
}

/// A Ticket that was discovered but did not qualify is named alongside the
/// search, so the batch says both where it looked and what it found there.
#[test]
fn a_not_ready_ticket_is_named_alongside_the_search_in_json() {
    let fixture = DispatchFixture::new();
    fixture.with_pool(1);
    // A status that is not the ready status, so the Ticket is found but does not
    // qualify.
    fixture.write_ticket(
        "widgets",
        "01-ship-the-widget.md",
        "# Ship the widget\n\nStatus: claimed\n",
    );

    let output = fixture.dispatch(&["dispatch", "--all", "--json"]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let json: serde_json::Value =
        serde_json::from_str(stdout(&output).trim()).expect("a JSON batch");
    let entries = json.as_array().expect("a batch is an array");
    assert!(
        entries
            .iter()
            .any(|entry| entry["ticket"].is_null() && entry["label"] == "ready-for-agent"),
        "{json}"
    );
    let skipped = entries
        .iter()
        .find(|entry| entry["ticket"] == "widgets--01-ship-the-widget")
        .expect("the discovered Ticket is named");
    assert_eq!(skipped["outcome"], "skipped");
    assert!(
        skipped["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("claimed")),
        "{json}"
    );
}
