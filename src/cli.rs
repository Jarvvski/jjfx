use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
const PROGRAM: &str = env!("CARGO_PKG_NAME");

/// Whether this build is a development build rather than a release.
///
/// `build.rs` sets the marker for a checkout build the release pipeline did not
/// produce. See `docs/releasing.md`.
const DEVELOPMENT_BUILD: bool = option_env!("JJFX_DEV_BUILD").is_some();

/// The version `jjfx version` reports.
///
/// A development build carries a `-dev` suffix, so a binary built from `main`
/// after a release cannot be mistaken for that release in a bug report.
pub(crate) const VERSION: &str = version_string(DEVELOPMENT_BUILD);

/// The version string for a build: `-dev` when it is a development build.
const fn version_string(development: bool) -> &'static str {
    if development {
        concat!(env!("CARGO_PKG_VERSION"), "-dev")
    } else {
        env!("CARGO_PKG_VERSION")
    }
}

use crate::adhoc_ticket::{AdHocInput, AdHocSource, AdHocTicket};
use crate::cli_help::{
    ADD_FLAGS, CHILDREN_FLAGS, CLEAN_FLAGS, DISPATCH_FLAGS, Flag, POOL_CREATE_FLAGS,
    POOL_DESTROY_FLAGS, POOL_LIST_FLAGS, POOL_PROFILE_FLAGS, POOL_RESIZE_FLAGS, RESOLVE_FLAGS,
    RESUME_FLAGS, REVIEW_FLAGS, RM_FLAGS, SEND_FLAGS, STATUS_FLAGS, TICKET_LIST_FLAGS, UNDO_FLAGS,
    WAIT_FLAGS,
};
use crate::config::{
    Config, MigrationsConfig, PoolConfig, PoolRoute, RepoConfig, TicketSource, TicketsConfig,
    global_ready_label, ready_label_for, resolve_pool,
};
use crate::engine::{
    AgentModel, AgentRuntime, AgentRuntimeProfile, CapacityShortage, CleanDecision, CleanScope,
    DEFAULT_POOL_NAME, DEFAULT_READY_LABEL, DirectDispatchError, DirectDispatchExecution,
    DirectDispatchFailurePhase, DirectDispatchOutcome, DirectDispatchRequest,
    DirectDispatchSuccess, DispatchBudget, DispatchDependencyContext, DispatchExclusions,
    FollowUpExecution, FollowUpOutcome, Loaded, LocalTicketReadiness, LocalTickets, OpenCodeApi,
    OpenCodeError, OrchestrationEvent, PoolCapacity, PoolCostSnapshot, PoolName, QueueItemState,
    REPOSITORY_WIDE_MUTATION, REVIEW_THREADS_DIMENSION, ReadyTicketFilter, ReadyTickets,
    RecordedReviewLoop, Repository, ReviewLoop, ReviewLoopSummary, ReviewRequest, ReviewSubject,
    RunActivity, RunActivityKind, RunLogError, RunMode, SessionMessage, SpendCap, SpendCapMode,
    SpendDecision, SpendLimit, SpendScope, SpendWindow, StuckWindow, SyncAction, TerminalSettings,
    ThreadReviewer, TicketDetails, TicketDiscovery, TicketId, TicketQuery, TicketStatus,
    TrunkDrift, UndoOutcome, WireAgent, WorkerActionError, WorkerActions, WorkerId, WorkerLogs,
    WorkerPoolError, WorkerPoolSnapshot, WorkerSnapshot, WorkerStatus, WorkspaceAddOutcome,
    WorkspaceHookStream, dependency_basis, resolve_ticket_branch,
};
use crate::migrations::FindingKind;
use crate::ticket_source::{apply_local_writeback, configured_ticket_query};
use crate::work::WorkState;

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Default,
    /// `help [<command>...]`: the top-level list, or one command's page.
    Help {
        command: Option<String>,
    },
    Version,
    Add {
        name: String,
        revision: Option<String>,
    },
    Remove {
        force: bool,
        names: Vec<String>,
    },
    List,
    Clean {
        /// The Worker Pool to scope the sweep to; `None` sweeps the whole
        /// Repository, which is the default.
        pool: Option<String>,
    },
    Root,
    Where,
    Path {
        name: String,
    },
    Refresh,
    Sync,
    Colocation {
        action: ColocationAction,
    },
    Status {
        json: bool,
    },
    /// `children [<session>] [--json]`: a session's child sessions and what
    /// each is doing.
    Children {
        session: Option<String>,
        json: bool,
    },
    Dispatch(DispatchArgs),
    /// `resume [<TICKET>...]`: re-dispatch the work the durable queue records as
    /// unfinished, without redoing the steps that completed.
    Resume(ResumeArgs),
    Send {
        worker: String,
        prompt: String,
        mode: RunMode,
        force: bool,
    },
    Review {
        worker: String,
        mode: RunMode,
    },
    Logs {
        worker: String,
    },
    Wait(WaitArgs),
    Mount {
        worker: String,
    },
    Rebase {
        /// A Worker's name, or the bookmark of the change to re-weld.
        worker: String,
    },
    Absorb {
        worker: String,
    },
    OpenPullRequest {
        /// A Worker's name, or the bookmark of the change whose Pull Request to
        /// open.
        worker: String,
    },
    /// `deliver <worker|branch>`: the one forge path - weld, check, push, open
    /// one Pull Request - as a shell verb, so an agent need not run
    /// `gh pr create`.
    Deliver {
        /// A Worker's name, or the bookmark of the change to deliver.
        worker: String,
    },
    Resolve {
        workspace: String,
        tool: Option<String>,
    },
    Undo {
        workspace: Option<String>,
        force: bool,
    },
    Completion {
        shell: String,
    },
    Check(CheckCommand),
    InternalComplete {
        mode: String,
    },
    InternalForeground {
        arguments: Vec<String>,
    },
    InternalOrchestrate {
        parent: String,
        provider: Option<String>,
        model: Option<String>,
        force: bool,
        /// The Worker Pool the detached runner fills, as the parent resolved it.
        /// `None` means `default`, so a hand-run `__orchestrate` is unchanged.
        pool: Option<String>,
    },
    Pool(PoolCommand),
    Ticket(TicketCommand),
}

/// The `ticket` subcommands: local Ticket state, reconciled on demand, and the
/// read-only backlog view.
#[derive(Debug, PartialEq, Eq)]
pub enum TicketCommand {
    /// `ticket reconcile`: resolve finished local Tickets without a Dispatch.
    Reconcile,
    /// `ticket list`: every Ticket the configured source discovers, with its
    /// Status, Blockers and current readiness. Read-only: it reconciles nothing.
    List(TicketListArgs),
}

/// The `ticket list` command's arguments: the lane whose readiness to report and
/// the output format.
#[derive(Debug, PartialEq, Eq)]
pub struct TicketListArgs {
    /// The Worker Pool whose lane to evaluate. `None` fans out across the pools
    /// that declare a routing label, exactly as `dispatch --all` does.
    pub pool: Option<String>,
    /// The label discovery looks for, overriding the pool's routing label.
    pub label: Option<String>,
    /// Print the Tickets as JSON rather than prose.
    pub json: bool,
}

/// Which colocation transition the `colocation` command performs.
#[derive(Debug, PartialEq, Eq)]
pub enum ColocationAction {
    Enable,
    Disable,
    Status,
}

impl ColocationAction {
    /// The verb `jj git colocation` takes for this action.
    fn verb(&self) -> &'static str {
        match self {
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Status => "status",
        }
    }

    /// Whether this action only reads the repository's state.
    ///
    /// Only `status` does, and a read must never snapshot the working copy. A
    /// snapshot takes the repository's working-copy lock, and jj prints nothing
    /// while it waits on another jj process holding it - so a plain
    /// `jj git colocation status` can block silently for as long as that process
    /// runs, which is the hang dogfood 08 recorded. Enabling and disabling are
    /// mutations and must snapshot.
    fn is_read(&self) -> bool {
        matches!(self, Self::Status)
    }
}

/// The read-only cross-workspace checks `jjfx check` can run.
#[derive(Debug, PartialEq, Eq)]
pub enum CheckCommand {
    /// Flag migration-order inversions across the open workspaces.
    Migrations,
}

#[derive(Debug, PartialEq, Eq)]
pub struct DispatchArgs {
    pub tickets: Vec<String>,
    pub all: bool,
    pub mode: RunMode,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub budget: Option<u32>,
    /// The label `--all` discovers by, overriding the selected pool's own
    /// routing label. `None` leaves the pool's label in force, and a pool that
    /// declares none falls back to the global ready label.
    pub label: Option<String>,
    /// The Worker Pool this Dispatch fills. `None` means "the Repository's only
    /// pool", which is `default` for a Repository that declares none; several
    /// pools with no `--pool` is a loud refusal, never a silent choice.
    pub pool: Option<String>,
    pub no_orchestrate: bool,
    pub force: bool,
    pub json: bool,
    /// A Ticket handed in directly (`--text`, `--from-file`, or stdin) rather
    /// than resolved through a configured source. `None` for the ordinary path.
    pub input: Option<AdHocInput>,
}

/// The `resume` command's arguments: what to resume and how to run it.
///
/// Resumption is explicit - an operator asks for it - because re-running a step
/// may have side effects an automatic retry must not take on. With no Ticket
/// named, every step the queue records as unfinished is resumed; naming Tickets
/// narrows it to those.
#[derive(Debug, PartialEq, Eq)]
pub struct ResumeArgs {
    /// Tickets to resume; empty means every unfinished step the queue holds.
    pub tickets: Vec<String>,
    /// Whether a resumed Run runs in the foreground or detaches.
    pub mode: RunMode,
    /// The Worker Pool whose queue to resume. `None` means "the Repository's only
    /// pool", resolved exactly as `dispatch` resolves it.
    pub pool: Option<String>,
    /// Resume even into a Worker whose Workspace is open in a terminal.
    pub force: bool,
    /// Print one machine-readable object per Ticket instead of prose.
    pub json: bool,
}

/// The `wait` command's arguments: the Workers to wait for and what to wait for.
#[derive(Debug, PartialEq, Eq)]
pub struct WaitArgs {
    /// Explicit Worker names; empty when `all` is set.
    pub workers: Vec<String>,
    /// Wait for every Worker in the pool rather than the named ones.
    pub all: bool,
    /// The Worker Pool this wait acts on. `None` means "the Repository's only
    /// pool", which is `default` for a Repository that declares none; several
    /// pools with no `--pool` is a loud refusal, never a silent choice.
    pub pool: Option<String>,
    /// The condition to wait for. `terminal` unless `--until` says otherwise.
    pub until: WaitCondition,
    /// Give up after this many seconds, exiting with the timeout code. `None`
    /// waits indefinitely, so "not done" is only ever reported on request.
    pub timeout: Option<u64>,
    /// Print the target Workers as JSON rather than prose.
    pub json: bool,
}

/// What `jjfx wait` blocks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitCondition {
    /// The Worker succeeded.
    Done,
    /// The Worker failed.
    Failed,
    /// The Worker reached any terminal state - done or failed. The default,
    /// because it is the honest answer until a Ticket's resolution is certain.
    Terminal,
    /// The Worker's local Ticket is resolved: the end-to-end confirmation.
    TicketResolved,
}

impl WaitCondition {
    /// The condition named by an `--until` value, or `None` when unrecognized.
    fn parse(value: &str) -> Option<Self> {
        match value {
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "terminal" => Some(Self::Terminal),
            "ticket-resolved" => Some(Self::TicketResolved),
            _ => None,
        }
    }

    /// The spelling shown in help and diagnostics.
    fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Terminal => "terminal",
            Self::TicketResolved => "ticket-resolved",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PoolCommand {
    /// `pool <N>` / `pool resize <N>`: size the `default` pool, or the named one
    /// `--pool` selects.
    Resize {
        size: String,
        pool: Option<String>,
    },
    List {
        json: bool,
    },
    /// `pool create <name>`: write a `[pools.<name>]` declaration.
    Create(PoolCreateArgs),
    /// `pool destroy [<name>] [--force]`: remove a `[pools.<name>]`
    /// declaration together with its Workers' Workspaces. No name means
    /// `default`, which is refused.
    Destroy {
        name: Option<String>,
        force: bool,
    },
    Remove {
        worker: String,
    },
    Reset {
        worker: String,
    },
    Profile {
        runtime: String,
        provider: Option<String>,
        model: Option<String>,
    },
    Help,
}

/// The declaration `pool create` writes, as parsed from the command line.
///
/// The fields mirror the `[pools.<name>]` keys one for one; each is optional, so
/// a bare `pool create bugs` writes a bare `[pools.bugs]` and the Repository
/// default applies. Values that name a profile, a source or a cap are kept as
/// the raw strings the parser read and validated into the declaration.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PoolCreateArgs {
    pub name: String,
    pub size: Option<String>,
    pub profile: Option<String>,
    pub source: Option<String>,
    pub label: Option<String>,
    pub ready_status: Option<String>,
    pub cap_usd: Option<String>,
    pub window: Option<String>,
    pub on_exceed: Option<String>,
}

impl PoolCreateArgs {
    /// The declaration this parses to, rejecting a value the config could not
    /// hold so the failure happens here rather than in the written file.
    fn declaration(&self) -> Result<PoolConfig> {
        Ok(PoolConfig {
            size: self.size.as_deref().map(parse_pool_size).transpose()?,
            profile: self.profile.clone(),
            source: self
                .source
                .as_deref()
                .map(|value| {
                    TicketSource::parse(value).ok_or_else(|| {
                        anyhow::anyhow!("unknown Ticket source: {value} (expected linear or local)")
                    })
                })
                .transpose()?,
            label: self.label.clone(),
            ready_status: self.ready_status.clone(),
            cap_usd: self.cap_usd.as_deref().map(parse_cap_usd).transpose()?,
            window: self
                .window
                .as_deref()
                .map(|value| {
                    SpendWindow::parse(value).ok_or_else(|| {
                        anyhow::anyhow!(
                            "unknown spend window: {value} (expected today, week or total)"
                        )
                    })
                })
                .transpose()?,
            on_exceed: self
                .on_exceed
                .as_deref()
                .map(|value| {
                    SpendCapMode::parse(value).ok_or_else(|| {
                        anyhow::anyhow!("unknown spend mode: {value} (expected block or warn)")
                    })
                })
                .transpose()?,
        })
    }
}

/// Parse a pool size, rejecting one the persisted size cannot hold.
fn parse_pool_size(value: &str) -> Result<usize> {
    let size = value
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("Invalid pool size: {value}"))?;
    PoolCapacity::new(size)?;
    Ok(size)
}

/// Parse a `--cap-usd` value. The positivity and finiteness rules are the
/// config's, so a bad value is rejected by the write's own validation.
fn parse_cap_usd(value: &str) -> Result<f64> {
    value
        .parse::<f64>()
        .map_err(|_| anyhow::anyhow!("Invalid spend cap: {value}"))
}

pub fn parse(args: &[String]) -> Result<Command> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(Command::Default);
    };
    match command {
        "help" | "-h" | "--help" => Ok(Command::Help {
            command: help_argument(&args[1..]),
        }),
        "version" | "--version" => Ok(Command::Version),
        "add" | "a" => parse_add(&args[1..]),
        "rm" | "remove" => parse_remove(&args[1..]),
        "list" | "ls" => Ok(Command::List),
        "clean" => parse_clean(&args[1..]),
        "root" => Ok(Command::Root),
        "where" | "info" => Ok(Command::Where),
        "path" => parse_single(
            concat!("Usage: ", env!("CARGO_PKG_NAME"), " path <name>"),
            &args[1..],
        )
        .map(|name| Command::Path { name }),
        "refresh" => Ok(Command::Refresh),
        "sync" => Ok(Command::Sync),
        "colocation" | "colocate" => parse_colocation(&args[1..]),
        "status" => parse_status(&args[1..]),
        "children" => parse_children(&args[1..]),
        "reset" => parse_single_exact(
            concat!("Usage: ", env!("CARGO_PKG_NAME"), " reset <worker>"),
            &args[1..],
        )
        .map(|worker| Command::Pool(PoolCommand::Reset { worker })),
        "dispatch" | "d" => parse_dispatch(&args[1..]),
        "resume" => parse_resume(&args[1..]),
        "send" | "s" => parse_send(&args[1..]),
        "review" | "rev" => parse_review(&args[1..]),
        "logs" | "log" => parse_worker_command(
            concat!("Usage: ", env!("CARGO_PKG_NAME"), " logs <worker>"),
            &args[1..],
        )
        .map(|worker| Command::Logs { worker }),
        "wait" => parse_wait(&args[1..]),
        "mount" | "m" => parse_worker_command(
            concat!("Usage: ", env!("CARGO_PKG_NAME"), " mount <worker>"),
            &args[1..],
        )
        .map(|worker| Command::Mount { worker }),
        "rebase" | "rb" => parse_worker_command(
            concat!("Usage: ", env!("CARGO_PKG_NAME"), " rebase <worker|branch>"),
            &args[1..],
        )
        .map(|worker| Command::Rebase { worker }),
        "absorb" | "ab" => parse_worker_command(
            concat!("Usage: ", env!("CARGO_PKG_NAME"), " absorb <worker>"),
            &args[1..],
        )
        .map(|worker| Command::Absorb { worker }),
        "open-pr" | "pr" => parse_worker_command(
            concat!(
                "Usage: ",
                env!("CARGO_PKG_NAME"),
                " open-pr <worker|branch>"
            ),
            &args[1..],
        )
        .map(|worker| Command::OpenPullRequest { worker }),
        "deliver" => parse_worker_command(
            concat!(
                "Usage: ",
                env!("CARGO_PKG_NAME"),
                " deliver <worker|branch>"
            ),
            &args[1..],
        )
        .map(|worker| Command::Deliver { worker }),
        "undo" => parse_undo(&args[1..]),
        "resolve" => parse_resolve(&args[1..]),
        "completion" => parse_completion(&args[1..]),
        "check" => parse_check(&args[1..]),
        "__complete" => parse_worker_command(
            concat!("Usage: ", env!("CARGO_PKG_NAME"), " __complete <mode>"),
            &args[1..],
        )
        .map(|mode| Command::InternalComplete { mode }),
        // Hidden: the wrapper the Run supervisor re-invokes inside a foreground
        // Run's process group to hand the terminal back. See `engine::foreground`.
        crate::engine::WRAPPER_SUBCOMMAND => Ok(Command::InternalForeground {
            arguments: args[1..].to_vec(),
        }),
        "__orchestrate" => parse_orchestrate(&args[1..]),
        "pool" => parse_pool(&args[1..]),
        "ticket" => parse_ticket(&args[1..]),
        unknown => bail!("Unknown command: {unknown}. Run '{PROGRAM} help' for usage."),
    }
}

/// Join the words after `help` into the command key a page is looked up by, or
/// `None` for a bare `help`.
fn help_argument(args: &[String]) -> Option<String> {
    if args.is_empty() {
        return None;
    }
    Some(args.join(" "))
}

/// Reject any option the command's help page does not document.
///
/// The page's flags are the *same* slice the parser validates against, so the
/// parser only ever accepts a flag some page documents: a flag handled here but
/// missing from the page (or the reverse) cannot be accepted. Each `parse_*`
/// match arm below gives a documented flag its meaning; it cannot widen the set.
fn reject_unknown_flags(command: &str, flags: &[Flag], args: &[String]) -> Result<()> {
    let mut index = 0;
    while index < args.len() {
        let argument = args[index].as_str();
        match flags.iter().find(|flag| flag.matches(argument)) {
            // A value-taking flag consumes the next token, whatever it looks
            // like: `--provider -x` is the parser's "missing value", not an
            // unknown option.
            Some(flag) if flag.value.is_some() => index += 1,
            Some(_) => {}
            // A bare `-` is the conventional stdin marker, not an option, so it
            // is never an unknown flag. The command that accepts it gives it
            // meaning (see `parse_dispatch`).
            None if argument == "-" => {}
            None if argument.starts_with('-') => {
                bail!("unknown option for {command}: {argument}");
            }
            None => {}
        }
        index += 1;
    }
    Ok(())
}

/// Parse `check <what>`, the read-only cross-workspace checks.
fn parse_check(args: &[String]) -> Result<Command> {
    match args.first().map(String::as_str) {
        Some("migrations") => Ok(Command::Check(CheckCommand::Migrations)),
        _ => bail!(concat!(
            "Usage: ",
            env!("CARGO_PKG_NAME"),
            " check migrations"
        )),
    }
}

fn parse_ticket(args: &[String]) -> Result<Command> {
    let usage = concat!(
        "Usage: ",
        env!("CARGO_PKG_NAME"),
        " ticket <reconcile|list> [--json] [--label <LABEL>] [--pool <name>]"
    );
    match args.first().map(String::as_str) {
        Some("reconcile") if args.len() == 1 => Ok(Command::Ticket(TicketCommand::Reconcile)),
        Some("list") => parse_ticket_list(&args[1..]),
        _ => bail!("{usage}"),
    }
}

/// Parse `ticket list [--json] [--label <LABEL>] [--pool <name>]`.
///
/// The parser only shapes the request; which lane a `--pool` names, and whether
/// an unqualified request is ambiguous, is settled against the Repository's
/// declarations when the command runs, so a parse never reads state.
fn parse_ticket_list(args: &[String]) -> Result<Command> {
    let usage = concat!(
        "Usage: ",
        env!("CARGO_PKG_NAME"),
        " ticket list [--json] [--label <LABEL>] [--pool <name>]"
    );
    reject_unknown_flags("ticket list", TICKET_LIST_FLAGS, args)?;
    let mut pool = None;
    let mut label = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => json = true,
            "--pool" | "--label" => {
                let flag = args[index].clone();
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| anyhow::anyhow!("missing value for {flag}"))?
                    .clone();
                if flag == "--pool" {
                    pool = Some(value);
                } else {
                    label = Some(value);
                }
                index += 2;
                continue;
            }
            value => bail!("unexpected argument for ticket list: {value}\n{usage}"),
        }
        index += 1;
    }
    Ok(Command::Ticket(TicketCommand::List(TicketListArgs {
        pool,
        label,
        json,
    })))
}

fn parse_colocation(args: &[String]) -> Result<Command> {
    let usage = concat!(
        "Usage: ",
        env!("CARGO_PKG_NAME"),
        " colocation <enable|disable|status>"
    );
    let action = match args.first().map(String::as_str) {
        Some("enable") => ColocationAction::Enable,
        Some("disable") => ColocationAction::Disable,
        Some("status") => ColocationAction::Status,
        _ => bail!("{usage}"),
    };
    if args.len() > 1 {
        bail!("{usage}");
    }
    Ok(Command::Colocation { action })
}

fn parse_add(args: &[String]) -> Result<Command> {
    reject_unknown_flags("add", ADD_FLAGS, args)?;
    let mut name = None;
    let mut revision = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-r" | "--revision" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    anyhow::anyhow!(concat!(
                        "Usage: ",
                        env!("CARGO_PKG_NAME"),
                        " add <name> [-r <rev>]"
                    ))
                })?;
                revision = Some(value.clone());
                index += 2;
            }
            value if value.starts_with('-') => {
                bail!("unknown option for add: {value}");
            }
            value if name.is_none() => {
                name = Some(value.to_owned());
                index += 1;
            }
            _ => {
                index += 1;
            }
        }
    }
    let name = name.ok_or_else(|| {
        anyhow::anyhow!(concat!(
            "Usage: ",
            env!("CARGO_PKG_NAME"),
            " add <name> [-r <rev>]"
        ))
    })?;
    Ok(Command::Add { name, revision })
}

fn parse_remove(args: &[String]) -> Result<Command> {
    reject_unknown_flags("rm", RM_FLAGS, args)?;
    let mut force = false;
    let mut names = Vec::new();
    for argument in args {
        match argument.as_str() {
            "--force" | "-f" => force = true,
            value if value.starts_with('-') => bail!("unknown option for rm: {value}"),
            value => names.push(value.to_owned()),
        }
    }
    if names.is_empty() {
        bail!(concat!(
            "Usage: ",
            env!("CARGO_PKG_NAME"),
            " rm [--force] <name> [name...]"
        ));
    }
    Ok(Command::Remove { force, names })
}

/// Parse `clean [--pool <name>]`.
///
/// The parser only shapes the request; whether the named pool exists is settled
/// against the Repository's declarations when the command runs, so a parse never
/// reads state. A bare `clean` sweeps the whole Repository.
fn parse_clean(args: &[String]) -> Result<Command> {
    let usage = concat!("Usage: ", env!("CARGO_PKG_NAME"), " clean [--pool <name>]");
    reject_unknown_flags("clean", CLEAN_FLAGS, args)?;
    let mut pool = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--pool" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| anyhow::anyhow!("{usage}"))?;
                pool = Some(value.clone());
                index += 2;
            }
            value if value.starts_with('-') => bail!("unknown option for clean: {value}"),
            value => bail!("unexpected argument for clean: {value}\n{usage}"),
        }
    }
    Ok(Command::Clean { pool })
}

/// Parse the top-level `status` alias, which accepts `--json`.
fn parse_status(args: &[String]) -> Result<Command> {
    reject_unknown_flags("status", STATUS_FLAGS, args)?;
    let mut json = false;
    for argument in args {
        match argument.as_str() {
            "--json" => json = true,
            value => bail!("Unknown option for status: {value}"),
        }
    }
    Ok(Command::Status { json })
}

/// Parse `children [<session>] [--json]`.
///
/// The parent session is optional: omitted, the command asks about the session
/// that ran it (the `OPENCODE_SESSION_ID` opencode puts in its shell), which is
/// how a parent asks from its own shell with no argument at all.
fn parse_children(args: &[String]) -> Result<Command> {
    reject_unknown_flags("children", CHILDREN_FLAGS, args)?;
    let mut session = None;
    let mut json = false;
    for argument in args {
        match argument.as_str() {
            "--json" => json = true,
            value if value.starts_with('-') => {
                bail!("Unknown option for children: {value}")
            }
            value if session.is_none() => session = Some(value.to_owned()),
            _ => bail!("children takes at most one session"),
        }
    }
    Ok(Command::Children { session, json })
}

/// Parse `wait <worker>... [--until CONDITION] [--timeout SECS] [--json] [--all]
/// [--pool <name>]`.
///
/// The parser only shapes the request; the answer is the exit code. The
/// condition defaults to `terminal`, and a missing `--timeout` means wait
/// forever, so "not done" is only ever reported when the caller asked it to.
fn parse_wait(args: &[String]) -> Result<Command> {
    let usage = concat!(
        "Usage: ",
        env!("CARGO_PKG_NAME"),
        " wait <worker>... [--until done|failed|terminal|ticket-resolved] [--timeout SECS] [--json] [--all] [--pool <name>]"
    );
    reject_unknown_flags("wait", WAIT_FLAGS, args)?;
    let mut workers = Vec::new();
    let mut all = false;
    let mut pool = None;
    let mut until = None;
    let mut timeout = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--all" => all = true,
            "--json" => json = true,
            "--pool" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| anyhow::anyhow!("missing value for --pool"))?;
                pool = Some(value.clone());
                index += 1;
            }
            "--until" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| anyhow::anyhow!("missing value for --until"))?;
                until = Some(WaitCondition::parse(value).ok_or_else(|| {
                    anyhow::anyhow!(
                        "invalid --until: {value} (expected done, failed, terminal or ticket-resolved)"
                    )
                })?);
                index += 1;
            }
            "--timeout" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| anyhow::anyhow!("missing value for --timeout"))?;
                timeout = Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| anyhow::anyhow!("invalid --timeout: {value}"))?,
                );
                index += 1;
            }
            value if value.starts_with('-') => bail!("unknown option for wait: {value}"),
            value => workers.push(value.to_owned()),
        }
        index += 1;
    }
    if all && !workers.is_empty() {
        bail!("--all cannot be combined with explicit Workers");
    }
    if workers.is_empty() && !all {
        bail!("{usage}");
    }
    Ok(Command::Wait(WaitArgs {
        workers,
        all,
        pool,
        until: until.unwrap_or(WaitCondition::Terminal),
        timeout,
        json,
    }))
}

/// Parse `undo [workspace] [--force]`.
///
/// With no Workspace, `undo` lists the jjfx-initiated mutations that can be
/// undone; naming one undoes it. The Workspace may be a Worker in either
/// spelling (`3` or `worker-3`), any Workspace name, or `repo`/`*` for a
/// repository-wide mutation.
fn parse_undo(args: &[String]) -> Result<Command> {
    reject_unknown_flags("undo", UNDO_FLAGS, args)?;
    let mut workspace = None;
    let mut force = false;
    for argument in args {
        match argument.as_str() {
            "--force" | "-f" => force = true,
            value if value.starts_with('-') => bail!("unknown option for undo: {value}"),
            value if workspace.is_none() => workspace = Some(value.to_owned()),
            value => bail!("unexpected argument for undo: {value}"),
        }
    }
    Ok(Command::Undo { workspace, force })
}

/// Parse `resolve <workspace> [--tool <name>]`.
///
/// The Workspace may be a Worker in either spelling (`3` or `worker-3`) or any
/// Workspace name. `--tool` is passed to `jj resolve`, including jj's built-in
/// `:ours`/`:theirs` side pickers; without it the user's own merge tool runs.
fn parse_resolve(args: &[String]) -> Result<Command> {
    reject_unknown_flags("resolve", RESOLVE_FLAGS, args)?;
    let mut workspace = None;
    let mut tool = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--tool" | "-t" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    anyhow::anyhow!(concat!(
                        "Usage: ",
                        env!("CARGO_PKG_NAME"),
                        " resolve <workspace> [--tool <name>]"
                    ))
                })?;
                tool = Some(value.clone());
                index += 2;
            }
            value if value.starts_with('-') => bail!("unknown option for resolve: {value}"),
            value if workspace.is_none() => {
                workspace = Some(value.to_owned());
                index += 1;
            }
            value => bail!("unexpected argument for resolve: {value}"),
        }
    }
    let workspace = workspace.ok_or_else(|| {
        anyhow::anyhow!(concat!(
            "Usage: ",
            env!("CARGO_PKG_NAME"),
            " resolve <workspace> [--tool <name>]"
        ))
    })?;
    Ok(Command::Resolve { workspace, tool })
}

/// Parse `pool list`/`ls`/`status`, which accepts `--json`.
fn parse_pool_list(args: &[String]) -> Result<Command> {
    reject_unknown_flags("pool list", POOL_LIST_FLAGS, args)?;
    let mut json = false;
    for argument in args {
        match argument.as_str() {
            "--json" => json = true,
            value => bail!("Unknown option for pool list: {value}"),
        }
    }
    Ok(PoolCommand::List { json }.into())
}

fn parse_pool(args: &[String]) -> Result<Command> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        return Ok(PoolCommand::List { json: false }.into());
    };
    match subcommand {
        "list" | "ls" | "status" => parse_pool_list(&args[1..]),
        "destroy" => parse_pool_destroy(&args[1..]),
        "help" | "-h" | "--help" => Ok(PoolCommand::Help.into()),
        "rm" | "remove" => parse_single(
            concat!("Usage: ", env!("CARGO_PKG_NAME"), " pool rm <worker>"),
            &args[1..],
        )
        .map(|worker| PoolCommand::Remove { worker }.into()),
        "reset" => parse_single(
            concat!("Usage: ", env!("CARGO_PKG_NAME"), " pool reset <worker>"),
            &args[1..],
        )
        .map(|worker| PoolCommand::Reset { worker }.into()),
        "profile" => parse_pool_profile(&args[1..]),
        "create" | "c" => parse_pool_create(&args[1..]),
        "resize" | "r" => parse_pool_resize(&args[1..]),
        value if value.parse::<i64>().is_ok() => parse_pool_resize(args),
        unknown => bail!("Unknown pool command: {unknown}"),
    }
}

/// Parse `pool destroy [<name>] [--force]`. No name targets `default`, which the
/// verb refuses with its reason.
fn parse_pool_destroy(args: &[String]) -> Result<Command> {
    reject_unknown_flags("pool destroy", POOL_DESTROY_FLAGS, args)?;
    let mut force = false;
    let mut name = None;
    for argument in args {
        match argument.as_str() {
            "--force" | "-f" => force = true,
            value if !value.starts_with('-') && name.is_none() => name = Some(value.to_owned()),
            value if value.starts_with('-') => {
                bail!("unknown option for pool destroy: {value}");
            }
            value => bail!(format!(
                "Usage: {} pool destroy [<name>] [--force] (unexpected argument: {value})",
                env!("CARGO_PKG_NAME")
            )),
        }
    }
    Ok(PoolCommand::Destroy { name, force }.into())
}

/// Parse `pool create <name> [--size N] [declaration flags]`.
fn parse_pool_create(args: &[String]) -> Result<Command> {
    reject_unknown_flags("pool create", POOL_CREATE_FLAGS, args)?;
    let usage = concat!(
        "Usage: ",
        env!("CARGO_PKG_NAME"),
        " pool create <name> [--size N] [--profile NAME] [--source linear|local] \
         [--label LABEL] [--ready-status STATUS] [--cap-usd USD] [--window WINDOW] \
         [--on-exceed block|warn]"
    );
    let mut create = PoolCreateArgs::default();
    let mut index = 0;
    while index < args.len() {
        let argument = args[index].as_str();
        let destination = match argument {
            "--size" | "-s" => &mut create.size,
            "--profile" => &mut create.profile,
            "--source" => &mut create.source,
            "--label" => &mut create.label,
            "--ready-status" => &mut create.ready_status,
            "--cap-usd" => &mut create.cap_usd,
            "--window" => &mut create.window,
            "--on-exceed" => &mut create.on_exceed,
            value if !value.starts_with('-') && create.name.is_empty() => {
                create.name = value.to_owned();
                index += 1;
                continue;
            }
            value if value.starts_with('-') => {
                bail!("unknown option for pool create: {value}");
            }
            value => bail!("unexpected argument for pool create: {value}"),
        };
        let value = args
            .get(index + 1)
            .filter(|value| !value.starts_with('-'))
            .ok_or_else(|| anyhow::anyhow!("missing value for {argument}"))?;
        if destination.replace(value.clone()).is_some() {
            bail!("duplicate option for pool create: {argument}");
        }
        index += 2;
    }
    if create.name.is_empty() {
        bail!(usage);
    }
    Ok(PoolCommand::Create(create).into())
}

fn parse_pool_profile(args: &[String]) -> Result<Command> {
    reject_unknown_flags("pool profile", POOL_PROFILE_FLAGS, args)?;
    let usage = concat!(
        "Usage: ",
        env!("CARGO_PKG_NAME"),
        " pool profile <claude|codex|opencode> [--provider PROVIDER] [--model MODEL]"
    );
    let Some(runtime) = args.first().filter(|value| !value.starts_with('-')) else {
        bail!(usage);
    };
    let mut provider = None;
    let mut model = None;
    let mut index = 1;
    while index < args.len() {
        let option = args[index].as_str();
        let destination = match option {
            "--provider" => &mut provider,
            "--model" => &mut model,
            _ => bail!("unknown option for pool profile: {option}"),
        };
        let value = args
            .get(index + 1)
            .filter(|value| !value.starts_with('-'))
            .ok_or_else(|| anyhow::anyhow!("missing value for {option}"))?;
        if destination.replace(value.clone()).is_some() {
            bail!("duplicate option for pool profile: {option}");
        }
        index += 2;
    }
    Ok(PoolCommand::Profile {
        runtime: runtime.clone(),
        provider,
        model,
    }
    .into())
}

fn parse_pool_resize(args: &[String]) -> Result<Command> {
    reject_unknown_flags("pool resize", POOL_RESIZE_FLAGS, args)?;
    let mut size = None;
    let mut pool = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--size" | "-s" => {
                size = args.get(index + 1).cloned();
                index += 2;
            }
            "--pool" => {
                pool = args.get(index + 1).cloned();
                index += 2;
            }
            value if !value.starts_with('-') => {
                size = Some(value.to_owned());
                index += 1;
            }
            _ => index += 1,
        }
    }
    let size = size.ok_or_else(|| {
        anyhow::anyhow!(concat!(
            "Usage: ",
            env!("CARGO_PKG_NAME"),
            " pool resize <N> [--pool <name>]"
        ))
    })?;
    Ok(PoolCommand::Resize { size, pool }.into())
}

fn parse_single(usage: &str, args: &[String]) -> Result<String> {
    args.first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("{usage}"))
}

fn parse_single_exact(usage: &str, args: &[String]) -> Result<String> {
    if args.len() != 1 {
        bail!("{usage}");
    }
    Ok(args[0].clone())
}

fn parse_worker_command(usage: &str, args: &[String]) -> Result<String> {
    parse_single_exact(usage, args)
}

fn parse_mode(flag: &str, mode: &mut Option<RunMode>) -> Result<()> {
    let next = match flag {
        "--fg" => RunMode::Foreground,
        "--bg" => RunMode::Background,
        _ => unreachable!(),
    };
    if mode.replace(next).is_some() {
        bail!("cannot combine --fg and --bg");
    }
    Ok(())
}

fn parse_dispatch(args: &[String]) -> Result<Command> {
    reject_unknown_flags("dispatch", DISPATCH_FLAGS, args)?;
    let mut tickets = Vec::new();
    let mut all = false;
    let mut mode = None;
    let mut provider = None;
    let mut model = None;
    let mut budget = None;
    let mut label = None;
    let mut pool = None;
    let mut no_orchestrate = false;
    let mut force = false;
    let mut json = false;
    let mut input = None;
    let mut id = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--fg" | "--bg" => parse_mode(&args[index], &mut mode)?,
            "--all" => all = true,
            "--no-orchestrate" => no_orchestrate = true,
            "--force" => force = true,
            "--json" => json = true,
            "--text" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| anyhow::anyhow!("missing value for --text"))?;
                set_adhoc_source(&mut input, AdHocSource::Text(value.clone()))?;
                index += 1;
            }
            "--from-file" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| anyhow::anyhow!("missing value for --from-file"))?;
                let source = if value == "-" {
                    AdHocSource::Stdin
                } else {
                    AdHocSource::File(PathBuf::from(value))
                };
                set_adhoc_source(&mut input, source)?;
                index += 1;
            }
            "--id" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| anyhow::anyhow!("missing value for --id"))?;
                id = Some(value.clone());
                index += 1;
            }
            // A bare `-` reads the Ticket from stdin, so a paragraph can be
            // piped straight in without dressing it up as a tracker item.
            "-" => set_adhoc_source(&mut input, AdHocSource::Stdin)?,
            "--provider" | "--model" | "--label" | "--budget" | "--pool" => {
                let option = args[index].as_str();
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| anyhow::anyhow!("missing value for {option}"))?;
                if value.starts_with('-') {
                    bail!("missing value for {option}");
                }
                match option {
                    "--provider" => provider = Some(value.clone()),
                    "--model" => model = Some(value.clone()),
                    "--label" => label = Some(value.clone()),
                    "--pool" => pool = Some(value.clone()),
                    "--budget" => {
                        let dollars = value
                            .parse::<u32>()
                            .map_err(|_| anyhow::anyhow!("invalid budget: {value}"))?;
                        if dollars == 0 {
                            bail!("invalid budget: {value}");
                        }
                        budget = Some(dollars);
                    }
                    _ => unreachable!(),
                }
                index += 1;
            }
            value if value.starts_with('-') => bail!("unknown option for dispatch: {value}"),
            value => tickets.push(value.to_owned()),
        }
        index += 1;
    }
    if all && !tickets.is_empty() {
        bail!("--all cannot be combined with explicit Tickets");
    }
    if input.is_some() && all {
        bail!("--text/--from-file cannot be combined with --all");
    }
    if input.is_some() && !tickets.is_empty() {
        bail!("--text/--from-file cannot be combined with explicit Tickets");
    }
    if id.is_some() && input.is_none() {
        bail!("--id requires --text, --from-file or -");
    }
    if tickets.is_empty() && !all && input.is_none() {
        bail!(concat!(
            "Usage: ",
            env!("CARGO_PKG_NAME"),
            " dispatch <TICKET>... [--fg|--bg] [--provider PROVIDER] [--model MODEL] [--json]"
        ));
    }
    if budget.is_some() && tickets.len() == 1 && !no_orchestrate {
        bail!("--budget for one Ticket requires --no-orchestrate");
    }
    Ok(Command::Dispatch(DispatchArgs {
        tickets,
        all,
        mode: mode.unwrap_or(RunMode::Background),
        provider,
        model,
        budget,
        label,
        pool,
        no_orchestrate,
        force,
        json,
        input: input.map(|source| AdHocInput { source, id }),
    }))
}

/// Parse `resume [<TICKET>...] [--pool NAME] [--fg|--bg] [--force] [--json]`.
///
/// No Ticket means "every unfinished step the queue records", so the bare
/// command is the recovery an operator runs after an interruption; naming
/// Tickets narrows the resumption to those steps.
fn parse_resume(args: &[String]) -> Result<Command> {
    reject_unknown_flags("resume", RESUME_FLAGS, args)?;
    let mut tickets = Vec::new();
    let mut mode = None;
    let mut pool = None;
    let mut force = false;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--fg" | "--bg" => parse_mode(&args[index], &mut mode)?,
            "--force" => force = true,
            "--json" => json = true,
            "--pool" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| anyhow::anyhow!("missing value for --pool"))?;
                pool = Some(value.clone());
                index += 1;
            }
            value if value.starts_with('-') => bail!("unknown option for resume: {value}"),
            value => tickets.push(value.to_owned()),
        }
        index += 1;
    }
    Ok(Command::Resume(ResumeArgs {
        tickets,
        mode: mode.unwrap_or(RunMode::Background),
        pool,
        force,
        json,
    }))
}

/// Records an ad-hoc Ticket input, rejecting a second one: a Dispatch carries
/// exactly one Ticket when it is handed in directly.
fn set_adhoc_source(input: &mut Option<AdHocSource>, source: AdHocSource) -> Result<()> {
    if input.is_some() {
        bail!("only one of --text, --from-file or - may supply the Ticket");
    }
    *input = Some(source);
    Ok(())
}

fn parse_send(args: &[String]) -> Result<Command> {
    reject_unknown_flags("send", SEND_FLAGS, args)?;
    let mut values = Vec::new();
    let mut mode = None;
    let mut force = false;
    for argument in args {
        match argument.as_str() {
            "--fg" | "--bg" => parse_mode(argument, &mut mode)?,
            "--force" => force = true,
            value if value.starts_with('-') => bail!("unknown option for send: {value}"),
            value => values.push(value.to_owned()),
        }
    }
    if values.len() != 2 {
        bail!(concat!(
            "Usage: ",
            env!("CARGO_PKG_NAME"),
            " send <worker> <prompt> [--fg|--bg] [--force]"
        ));
    }
    Ok(Command::Send {
        worker: values.remove(0),
        prompt: values.remove(0),
        mode: mode.unwrap_or(RunMode::Background),
        force,
    })
}

fn parse_review(args: &[String]) -> Result<Command> {
    reject_unknown_flags("review", REVIEW_FLAGS, args)?;
    let mut worker = None;
    let mut mode = None;
    for argument in args {
        match argument.as_str() {
            "--fg" | "--bg" => parse_mode(argument, &mut mode)?,
            value if value.starts_with('-') => bail!("unknown option for review: {value}"),
            value if worker.is_none() => worker = Some(value.to_owned()),
            _ => bail!(concat!(
                "Usage: ",
                env!("CARGO_PKG_NAME"),
                " review <worker> [--fg|--bg]"
            )),
        }
    }
    Ok(Command::Review {
        worker: worker.ok_or_else(|| {
            anyhow::anyhow!(concat!(
                "Usage: ",
                env!("CARGO_PKG_NAME"),
                " review <worker> [--fg|--bg]"
            ))
        })?,
        mode: mode.unwrap_or(RunMode::Background),
    })
}

fn parse_completion(args: &[String]) -> Result<Command> {
    if args.len() > 1 {
        bail!(concat!(
            "Usage: ",
            env!("CARGO_PKG_NAME"),
            " completion [zsh]"
        ));
    }
    Ok(Command::Completion {
        shell: args.first().cloned().unwrap_or_else(|| "zsh".to_owned()),
    })
}

fn parse_orchestrate(args: &[String]) -> Result<Command> {
    let Some(parent) = args.first() else {
        bail!(concat!(
            "Usage: ",
            env!("CARGO_PKG_NAME"),
            " __orchestrate <PARENT-TICKET> [--provider PROVIDER] [--model MODEL] [--pool NAME]"
        ));
    };
    let mut provider = None;
    let mut model = None;
    let mut pool = None;
    let mut force = false;
    let mut index = 1;
    while index < args.len() {
        let option = args[index].as_str();
        if option == "--force" {
            force = true;
            index += 1;
            continue;
        }
        let destination = match option {
            "--provider" => &mut provider,
            "--model" => &mut model,
            "--pool" => &mut pool,
            _ => bail!("unknown option for __orchestrate: {option}"),
        };
        let value = args
            .get(index + 1)
            .filter(|value| !value.starts_with('-'))
            .ok_or_else(|| anyhow::anyhow!("missing value for {option}"))?;
        if destination.replace(value.clone()).is_some() {
            bail!("duplicate option for __orchestrate: {option}");
        }
        index += 2;
    }
    Ok(Command::InternalOrchestrate {
        parent: parent.clone(),
        provider,
        model,
        force,
        pool,
    })
}

impl From<PoolCommand> for Command {
    fn from(command: PoolCommand) -> Self {
        Self::Pool(command)
    }
}

/// The settings a CLI invocation resolves lazily, so a command only reads the
/// config file it actually uses. Bundled rather than threaded positionally: the
/// list outgrew a readable argument list.
pub struct Settings {
    /// The terminal settings `mount` reads.
    pub terminal: fn() -> Result<TerminalSettings>,
    /// Whether new workspaces are colocated with Git.
    pub colocate: fn() -> Result<bool>,
    /// The workspaces currently open in the terminal jjfx drives.
    pub open_workspaces: fn() -> Result<std::collections::BTreeSet<String>>,
    /// Where Dispatch discovers Ready Tickets, resolved for the Repository the
    /// command acts on so its own `[tickets]` selection is honoured.
    pub tickets: fn(&Repository) -> Result<TicketsConfig>,
    /// How the migration-order check finds migrations.
    pub migrations: fn() -> Result<MigrationsConfig>,
}

/// Route a CLI invocation. `launch` enters the interactive TUI; the rest of the
/// [`Settings`] resolve lazily, so a command reads only the config it uses - no
/// command pays for a file it never opens.
pub fn run(args: Vec<String>, launch: fn(PathBuf) -> Result<()>, settings: Settings) -> Result<()> {
    let Settings {
        terminal: terminal_settings,
        colocate,
        open_workspaces,
        tickets,
        migrations,
    } = settings;
    match parse(&args)? {
        Command::Help { command } => print!("{}", crate::cli_help::text(command.as_deref())?),
        Command::Version => println!("{PROGRAM} {VERSION}"),
        Command::Default => default_command(launch)?,
        Command::Root => root_command(&repository(colocate)?)?,
        Command::Where => where_command(&repository(colocate)?)?,
        Command::Path { name } => path_command(&repository(colocate)?, &name)?,
        Command::Refresh => refresh_command(&repository(colocate)?)?,
        Command::Sync => sync_command(&repository(colocate)?)?,
        Command::Colocation { action } => colocation_command(&repository(colocate)?, &action)?,
        Command::Add { name, revision } => {
            add_command(&repository(colocate)?, &name, revision.as_deref())?
        }
        Command::List => list_command(&repository(colocate)?)?,
        Command::Remove { force, names } => remove_command(&repository(colocate)?, force, &names)?,
        Command::Clean { pool } => clean_command(&repository(colocate)?, pool.as_deref())?,
        Command::Status { json } => pool_list_command(&repository(colocate)?, json)?,
        Command::Children { session, json } => crate::children::run(session.as_deref(), json)?,
        Command::Dispatch(args) => {
            dispatch_command(&repository(colocate)?, &args, open_workspaces, tickets)?
        }
        Command::Resume(args) => {
            resume_command(&repository(colocate)?, &args, open_workspaces, tickets)?
        }
        Command::Send {
            worker,
            prompt,
            mode,
            force,
        } => send_command(
            &repository(colocate)?,
            &worker,
            &prompt,
            mode,
            force,
            open_workspaces,
        )?,
        Command::Review { worker, mode } => review_command(&repository(colocate)?, &worker, mode)?,
        Command::Logs { worker } => logs_command(&repository(colocate)?, &worker)?,
        Command::Wait(args) => wait_command(&repository(colocate)?, &args)?,
        Command::Mount { worker } => {
            mount_command(&repository(colocate)?, &worker, terminal_settings)?
        }
        Command::Rebase { worker } => rebase_command(&repository(colocate)?, &worker)?,
        Command::Absorb { worker } => absorb_command(&repository(colocate)?, &worker)?,
        Command::OpenPullRequest { worker } => open_pr_command(&repository(colocate)?, &worker)?,
        Command::Deliver { worker } => deliver_command(&repository(colocate)?, &worker)?,
        Command::Resolve { workspace, tool } => {
            resolve_command(&repository(colocate)?, &workspace, tool.as_deref())?
        }
        Command::Undo { workspace, force } => {
            undo_command(&repository(colocate)?, workspace.as_deref(), force)?
        }
        Command::Completion { shell } => completion_command(&shell)?,
        Command::Check(CheckCommand::Migrations) => {
            check_migrations_command(&repository(colocate)?, &migrations()?)?
        }
        Command::InternalComplete { mode } => internal_complete_command(&mode)?,
        Command::InternalForeground { arguments } => {
            crate::engine::run_wrapper(&arguments)?;
        }
        Command::InternalOrchestrate {
            parent,
            provider,
            model,
            force,
            pool,
        } => {
            let repository = repository(colocate)?;
            let repository = match pool {
                Some(name) => repository.with_pool(PoolName::parse(name)?),
                None => repository,
            };
            orchestrate_command(
                &repository,
                &parent,
                selected_model(provider.as_deref(), model.as_deref())?,
                force,
                false,
                open_workspaces,
                tickets,
            )?
        }
        Command::Pool(command) => pool_command(&repository(colocate)?, command)?,
        Command::Ticket(TicketCommand::Reconcile) => {
            ticket_reconcile_command(&repository(colocate)?, tickets)?
        }
        Command::Ticket(TicketCommand::List(args)) => {
            ticket_list_command(&repository(colocate)?, &args, tickets)?
        }
    }
    Ok(())
}

fn repository(colocate: fn() -> Result<bool>) -> Result<Repository> {
    // A `jj`/`jj-lib` mismatch warns on every repo command: jjfx is
    // version-coupled to jj, so a differing binary can make its reads wrong.
    if let Some(notice) = crate::engine::jj_lib_compat_notice() {
        eprintln!("warning: {notice}");
    }
    // New workspaces follow jjfx's `colocate` setting, read lazily so only
    // commands that touch a repository pay for it; a broken config is a loud
    // error, exactly as it is in the TUI.
    let colocate = colocate()?;
    let repository = Repository::open(".").map_err(|_| anyhow::anyhow!("Not in a jj repo"))?;
    // The repo-owned build check a finished Run is verified against (ticket 02):
    // the repository's own `[checks]` when it sets one, else the user-global one.
    let checks = crate::config::load()?.checks_for(repository.root())?;
    Ok(repository.with_colocate(colocate).with_checks(checks))
}

fn print_hook_output(_stream: WorkspaceHookStream, line: &str) {
    if !line.is_empty() {
        eprintln!("{line}");
    }
}

/// Warn on stderr when the working copy in the current directory has fallen
/// behind trunk, so a stale on-disk tree is never silent.
///
/// The warning names how far behind it is and the command that re-syncs it; it
/// is only a warning, because the stale state is legitimate mid-edit. A read
/// failure is surfaced too - "in sync" and "could not tell" must not look alike.
fn warn_when_behind_trunk(repository: &Repository) {
    match current_workspace_drift(repository) {
        Ok(Some(drift)) => eprintln!("warning: {}", drift.warning()),
        Ok(None) => {}
        Err(error) => {
            eprintln!(
                "warning: could not check whether this working copy is behind trunk: {error:#}"
            )
        }
    }
}

/// How far the working copy in the current directory has fallen behind trunk.
fn current_workspace_drift(repository: &Repository) -> Result<Option<TrunkDrift>> {
    let dir = std::env::current_dir().context("reading the current directory")?;
    Ok(repository.workspaces().trunk_drift(&dir)?)
}

fn root_command(repository: &Repository) -> Result<()> {
    warn_when_behind_trunk(repository);
    println!("{}", repository.root().display());
    Ok(())
}

fn where_command(repository: &Repository) -> Result<()> {
    warn_when_behind_trunk(repository);
    println!("repo:       {}", repository.root().display());
    println!(
        "workspaces: {}",
        repository.workspaces().base_dir().display()
    );
    Ok(())
}

/// Fetch and move the current workspace's working copy onto trunk.
///
/// Distinct from `refresh` (rebuild the workspace cache; nothing moves) and
/// `rebase <worker>` (rebase and push a named Worker's bookmark): `sync` pushes
/// nothing and touches no Worker, it only re-syncs the working copy in the
/// current directory. A rebase that conflicts is reported, never resolved.
fn sync_command(repository: &Repository) -> Result<()> {
    let dir = std::env::current_dir().context("reading the current directory")?;
    let outcome = repository.workspaces().sync_to_trunk(&dir)?;
    if let Some(warning) = outcome.warning() {
        eprintln!("warning: {warning}");
    }
    match outcome.action() {
        SyncAction::AlreadyOnTrunk => eprintln!("Already on trunk"),
        SyncAction::MovedEmpty => eprintln!(
            "Moved the empty working copy onto trunk (was {} behind)",
            commits_behind(outcome.behind())
        ),
        SyncAction::Rebased => eprintln!(
            "Rebased the working copy onto trunk (was {} behind)",
            commits_behind(outcome.behind())
        ),
    }
    if !outcome.conflicts().is_empty() {
        anyhow::bail!(
            "re-syncing left {} conflicted commit(s): {}; resolve them with \
             `{PROGRAM} resolve <workspace>` or `jj resolve`, then continue",
            outcome.conflicts().len(),
            outcome.conflicts().join(", ")
        );
    }
    Ok(())
}

/// `1 commit` / `3 commits`, so a count reads as prose.
fn commits_behind(count: u32) -> String {
    match count {
        1 => "1 commit".to_owned(),
        count => format!("{count} commits"),
    }
}

/// Run the read-only cross-workspace migration-order check and print its report.
/// It never reorders or rewrites anything. Exits non-zero when the set has a
/// hazard, so an agent can gate a merge on it.
fn check_migrations_command(repository: &Repository, migrations: &MigrationsConfig) -> Result<()> {
    let report = crate::migrations::check(repository.root(), &migrations.pattern)?;
    if report.is_clean() {
        println!(
            "no migration-order inversions across the workspaces (`{}`)",
            migrations.pattern
        );
        return Ok(());
    }
    println!(
        "migration-order hazards across the workspaces (`{}`):",
        migrations.pattern
    );
    for finding in report.findings() {
        let heading = match finding.kind {
            FindingKind::Inversion => "merge in this order",
            FindingKind::Ambiguous => "ambiguous version - resolve before merging",
        };
        println!("\n{} - {heading}:", finding.space);
        for migration in &finding.migrations {
            println!(
                "  V{}  {}  {}",
                migration.version, migration.workspace, migration.path
            );
        }
    }
    anyhow::bail!("migration-order check failed: {}", report.summary())
}

fn path_command(repository: &Repository, name: &str) -> Result<()> {
    // Dispatch and the `pool list` output use the short Worker id, while the
    // Workspace is named `worker-<id>`. Resolve either form against the real
    // projection so `path <short-id>` returns the directory that actually
    // exists.
    println!("{}", workspace_path(repository, name).display());
    Ok(())
}

/// The directory a Workspace name or Worker id resolves to, against the real
/// projection so a short Worker id (`3`) reaches the `worker-3` that exists.
fn workspace_path(repository: &Repository, name: &str) -> PathBuf {
    let workspaces = repository.workspaces();
    let prefixed = format!("worker-{name}");
    workspaces
        .projection()
        .ok()
        .and_then(|snapshot| {
            let entries = snapshot.entries();
            entries
                .iter()
                .find(|entry| entry.name() == name)
                .or_else(|| entries.iter().find(|entry| entry.name() == prefixed))
                .and_then(|entry| entry.path().map(PathBuf::from))
        })
        .unwrap_or_else(|| {
            let worker_path = workspaces.path(&prefixed);
            if worker_path.is_dir() {
                worker_path
            } else {
                workspaces.path(name)
            }
        })
}

/// Transition this repository's Git colocation through jj. Enabling lets jjfx
/// create colocated workspaces here; disabling restores plain jj workspaces.
///
/// `status` is answered with `--ignore-working-copy`, so the read is lock-free:
/// colocation is a property of the workspace layout rather than of the
/// working-copy contents, so not snapshotting cannot change the answer, while
/// snapshotting let another jj process block the query silently (dogfood 08).
fn colocation_command(repository: &Repository, action: &ColocationAction) -> Result<()> {
    let verb = action.verb();
    let mut command = std::process::Command::new("jj");
    command.args(["git", "colocation", verb]);
    if action.is_read() {
        command.arg("--ignore-working-copy");
    }
    let status = command
        .current_dir(repository.root())
        .status()
        .with_context(|| format!("run jj git colocation {verb}"))?;
    if !status.success() {
        bail!("jj git colocation {verb} failed");
    }
    Ok(())
}

fn refresh_command(repository: &Repository) -> Result<()> {
    repository.workspaces().refresh()?;
    eprintln!("Cache refreshed");
    Ok(())
}

fn add_command(repository: &Repository, name: &str, revision: Option<&str>) -> Result<()> {
    match repository
        .workspaces()
        .add_with_progress(name, revision, print_hook_output)?
    {
        WorkspaceAddOutcome::Default(path) => println!("{}", path.display()),
        WorkspaceAddOutcome::Existing(workspace) | WorkspaceAddOutcome::Created(workspace) => {
            println!("{}", workspace.path().display())
        }
    }
    Ok(())
}

fn list_command(repository: &Repository) -> Result<()> {
    let entries = repository.workspaces().snapshot()?.entries().to_owned();
    if entries.is_empty() {
        println!("  No workspaces");
        return Ok(());
    }
    for entry in entries {
        // Name the path when jjfx knows it, and say plainly when it is gone. An
        // unknown path is never dressed up as a location.
        let location = match entry.path() {
            Some(path) if path.is_dir() => path.display().to_string(),
            Some(path) => format!("{} (missing)", path.display()),
            None => "(path unknown)".to_owned(),
        };
        println!("  {} ➜ {}", entry.name(), location);
    }
    Ok(())
}

fn remove_command(repository: &Repository, force: bool, names: &[String]) -> Result<()> {
    let workspaces = repository.workspaces();
    let mut failure = None;
    for name in names {
        if name == "default" {
            eprintln!("Cannot remove default workspace");
            continue;
        }
        let path = workspaces.path(name);
        match workspaces.remove_with_progress(name, force, print_hook_output) {
            Ok(true) => eprintln!("Deleted {}", path.display()),
            Ok(false) => {}
            Err(error) => {
                eprintln!("{error}");
                failure.get_or_insert(error);
            }
        }
    }
    if let Some(error) = failure {
        return Err(error.into());
    }
    Ok(())
}

/// Removes non-default Workspaces, Repository-wide or scoped to one Worker Pool.
///
/// `--pool <name>` scopes the sweep to that pool's Workers' Workspaces, so one
/// lane's tidy-up leaves another lane's work in progress alone. The
/// Repository-wide form stays the explicit "everything" and stays the default,
/// so every existing invocation is unchanged. An unknown pool name is refused
/// loudly, naming the pools the Repository holds, the way a Dispatch refuses an
/// ambiguous target.
fn clean_command(repository: &Repository, pool: Option<&str>) -> Result<()> {
    let scope = match pool {
        Some(name) => {
            let config = crate::config::load()?;
            let available = config.pool_names_for(repository.root())?;
            CleanScope::Pool(resolve_pool(&available, Some(name))?)
        }
        None => CleanScope::Repository,
    };
    let workspaces = repository.workspaces();
    let plan = workspaces.plan_clean(scope)?;
    if plan.entries().is_empty() {
        println!("No workspaces to remove");
        return Ok(());
    }
    println!("Remove {} workspace(s)?", plan.entries().len());
    for entry in plan.entries() {
        println!("  {}", entry.name());
    }
    let decision = confirm_clean()?;
    workspaces.clean_with_progress(&plan, decision, print_hook_output)?;
    Ok(())
}

/// Whether a typed answer confirms a clean sweep.
///
/// Only an explicit `y` does, matching the contract `clean` has always had; an
/// `n`, an empty answer, or anything else declines and exits 0.
fn answer_confirms_clean(answer: &str) -> bool {
    answer.trim().eq_ignore_ascii_case("y")
}

/// Asks whether to apply a clean plan, reading the answer from stdin.
///
/// The prompt goes to stderr so stdout carries only the plan's listing, and a
/// caller that never answers - stdin at EOF, as a non-interactive script gets -
/// is refused loudly rather than treated as a decline. The distinction matters:
/// a user typing `n` chose not to act, but a process that never answered did
/// not choose anything, and a silent success would make "nothing happened"
/// indistinguishable from "nothing needed doing". An explicit non-`y` answer
/// declines and exits 0, the contract `clean` already had.
fn confirm_clean() -> Result<CleanDecision> {
    eprint!("Confirm? (y/n) ");
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer)? == 0 {
        bail!("no confirmation received; nothing was removed");
    }
    Ok(if answer_confirms_clean(&answer) {
        CleanDecision::Confirmed
    } else {
        CleanDecision::Declined
    })
}

fn pool_command(repository: &Repository, command: PoolCommand) -> Result<()> {
    match command {
        PoolCommand::Resize { size, pool } => {
            resize_pool_command(repository, &size, pool.as_deref())
        }
        PoolCommand::List { json } => pool_list_command(repository, json),
        PoolCommand::Create(args) => create_pool_command(repository, &args),
        PoolCommand::Destroy { name, force } => {
            destroy_pool_command(repository, name.as_deref(), force)
        }
        PoolCommand::Remove { worker } => remove_worker_command(repository, &worker),
        PoolCommand::Reset { worker } => reset_worker_command(repository, &worker),
        PoolCommand::Profile {
            runtime,
            provider,
            model,
        } => pool_profile_command(repository, &runtime, provider.as_deref(), model.as_deref()),
        PoolCommand::Help => {
            print!("{}", crate::cli_help::text(Some("pool"))?);
            Ok(())
        }
    }
}

/// The repository-local config file a lifecycle verb edits, for its message.
fn repo_config_display(repository: &Repository) -> String {
    crate::config::repo_config_path(repository.root())
        .display()
        .to_string()
}

/// `pool create <name>`: write a `[pools.<name>]` declaration.
fn create_pool_command(repository: &Repository, args: &PoolCreateArgs) -> Result<()> {
    create_pool(repository, args, &crate::config::load()?)
}

/// The declaration write, with the user-global config handed in so a test can
/// drive it without reading the developer's own config.
///
/// The declaration is the source of truth (ADR 0014), so creating a pool is
/// writing the section - no parallel state, and nothing to reconcile. A pool
/// that is already declared is refused rather than silently replaced, so a typo
/// cannot clobber a lane's settings.
fn create_pool(repository: &Repository, args: &PoolCreateArgs, user: &Config) -> Result<()> {
    // `pool create <N>` used to be a synonym for `pool <N>`. It now names a pool,
    // so a numeric argument would silently mean something else; refuse it and
    // name the sizing verb instead. A pool genuinely called `3` is hand-declared.
    if args.name.trim().parse::<i64>().is_ok() {
        bail!(
            "`{}` is a size, not a pool name; `{PROGRAM} pool {}` sizes `default`, and \
             `{PROGRAM} pool resize {} --pool <name>` sizes a named pool",
            args.name.trim(),
            args.name.trim(),
            args.name.trim()
        );
    }
    let name = PoolName::parse(args.name.trim()).map_err(|error| anyhow::anyhow!("{error}"))?;
    let existing = user.repo_config(repository.root())?;
    if existing.pools.contains_key(name.as_str()) {
        bail!(
            "pool `{name}` is already declared in {}",
            repo_config_display(repository)
        );
    }
    let declaration = args.declaration()?;
    crate::config::declare_pool(repository.root(), name.as_str(), &declaration, user)?;
    eprintln!(
        "Declared pool `{name}` in {}",
        repo_config_display(repository)
    );
    Ok(())
}

fn pool_profile_command(
    repository: &Repository,
    runtime: &str,
    provider: Option<&str>,
    model: Option<&str>,
) -> Result<()> {
    let profile = runtime_profile(runtime, provider, model)?;
    repository.worker_pool().set_profile(profile.clone())?;
    eprintln!("Pool profile set to {}", render_profile(&profile));
    Ok(())
}

fn runtime_profile(
    runtime: &str,
    provider: Option<&str>,
    model: Option<&str>,
) -> Result<AgentRuntimeProfile> {
    let configured = WireAgent::new(runtime);
    let runtime = AgentRuntime::from_configured(Some(&configured))
        .map_err(|message| anyhow::anyhow!("{message}"))?;
    let mut profile = AgentRuntimeProfile::new(runtime);
    if let Some(selection) = selected_model(provider, model)? {
        profile = profile.with_model(selection);
    }
    Ok(profile)
}

fn selected_model(provider: Option<&str>, model: Option<&str>) -> Result<Option<AgentModel>> {
    let Some(model) = model else {
        if provider.is_some() {
            bail!("--provider requires --model");
        }
        return Ok(None);
    };
    let mut selection = AgentModel::new(model);
    if let Some(provider) = provider {
        selection = selection.with_provider(provider);
    }
    Ok(Some(selection))
}

fn render_profile(profile: &AgentRuntimeProfile) -> String {
    match profile.model() {
        Some(model) => match model.provider() {
            Some(provider) => format!("{} ({provider}/{})", profile.runtime(), model.model()),
            None => format!("{} ({})", profile.runtime(), model.model()),
        },
        None => profile.runtime().to_string(),
    }
}

/// `pool <N>` / `pool resize <N>`: size a pool.
///
/// `default` (the bare form, or an explicit `--pool default`) sizes its state
/// exactly as before - its Workers are provisioned and detached against its
/// state - and moves its declaration too when the Repository declares one, so a
/// declared `default` cannot go stale (ticket 16). A named pool is sized in its
/// *declaration*, because that is the source of truth (ADR 0014); when the lane
/// has already materialised, the resize reaches its Workers first and the
/// declaration is updated to agree, so the two never disagree about the size.
fn resize_pool_command(repository: &Repository, value: &str, pool: Option<&str>) -> Result<()> {
    let size = value
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("Invalid pool size: {value}"))?;
    let user = crate::config::load()?;
    match pool {
        None | Some(DEFAULT_POOL_NAME) => resize_default_pool(repository, size, &user),
        Some(name) => resize_declared_pool(repository, name, size, &user),
    }
}

/// Resize a Pool's materialised state, provisioning new Workers or detaching its
/// stable tail, exactly as `pool <N>` always has.
///
/// This is `default`'s whole path - an implicit pool with no declaration to move
/// - and the state half of resizing a declared pool that has materialised.
fn apply_pool_resize(repository: &Repository, size: usize) -> Result<()> {
    let capacity = PoolCapacity::new(size)?;
    let before = repository
        .worker_pool()
        .snapshot()
        .pool()
        .map_or(0, |pool| usize::try_from(pool.size()).unwrap_or_default());
    let resize = repository
        .worker_pool()
        .resize_to_with_progress(capacity, print_hook_output)?;
    for worker in resize.added_workers() {
        eprintln!("  Created {worker}");
    }
    for worker in resize.removed_workers() {
        eprintln!("  Removed {worker}");
    }
    if before == size {
        eprintln!("Pool is already size {size}");
    } else if before < size {
        eprintln!("Pool expanded from {before} to {size}");
    } else {
        eprintln!("Pool shrunk from {before} to {size}");
    }
    Ok(())
}

/// Size `default`, moving its declaration too when the Repository declares one.
///
/// `default` is implicit unless a Repository writes `[pools.default]`, so the
/// state resize is its whole path in the common case - and must stay that way:
/// sizing an implicit `default` must never grow a `[pools.default]` section the
/// first time someone resizes their pool. When a declaration does exist the two
/// are moved together, exactly as a named pool's are (ticket 15): the state half
/// runs first, so a resize refused over a busy Worker returns before the write
/// and leaves the declaration alone, and the lane and the declaration cannot
/// disagree about the size (ticket 16).
fn resize_default_pool(repository: &Repository, size: usize, user: &Config) -> Result<()> {
    let declared = user
        .repo_config(repository.root())?
        .pools
        .contains_key(DEFAULT_POOL_NAME);
    apply_pool_resize(repository, size)?;
    if declared {
        crate::config::set_pool_size(repository.root(), DEFAULT_POOL_NAME, size, user)?;
        eprintln!(
            "Pool `{DEFAULT_POOL_NAME}` size set to {size} in {}",
            repo_config_display(repository)
        );
    }
    Ok(())
}

/// Size a declared pool: resize its materialised lane, then move its declaration
/// to agree.
///
/// The declaration is the source of truth, but a pool that has materialised has
/// Workers of its own, and the declaration alone cannot change them (ticket 15).
/// So a lane that exists is resized in place first - growing and shrinking under
/// the same busy-Worker safety `default` has always had, so a shrink over a busy
/// Worker is refused and nothing is written - and only then is the declaration
/// updated. A pool that has not materialised has no state to resize, so only the
/// declaration moves and the first Dispatch provisions the lane at that size
/// (ticket 13). Either way the two agree, so `status` reports one true size.
fn resize_declared_pool(
    repository: &Repository,
    name: &str,
    size: usize,
    user: &Config,
) -> Result<()> {
    let existing = user.repo_config(repository.root())?;
    if !existing.pools.contains_key(name) {
        bail!(
            "no pool `{name}` is declared in {}; create it with `{PROGRAM} pool create {name}`",
            repo_config_display(repository)
        );
    }
    let pool = PoolName::parse(name).map_err(|error| anyhow::anyhow!("{error}"))?;
    let scoped = repository.clone().with_pool(pool);
    if scoped.state_store().directory_exists() {
        apply_pool_resize(&scoped, size)?;
    }
    crate::config::set_pool_size(repository.root(), name, size, user)?;
    eprintln!(
        "Pool `{name}` size set to {size} in {}",
        repo_config_display(repository)
    );
    Ok(())
}

/// Materialises a declared Worker Pool's state on first use.
///
/// A Repository may commit a `[pools.<name>]` declaration while the lane's state
/// document does not exist yet; that is normal, not a disagreement (ADR 0013).
/// The first Dispatch into such a lane provisions its Workers at the declared
/// size - exactly as `jjfx pool <N>` does for `default` - so a declaration a
/// teammate committed works on a fresh clone with nobody provisioning it by
/// hand. A lane whose state already exists is left alone, so this is idempotent.
///
/// The mark of a materialised lane is its **document**, not its directory: the
/// directory also holds Run Records, the waiting set, the work queue and its
/// locks, and a lock creates its parents, so a directory can be left behind with
/// no document in it (ticket 48) - including by the TUI's in-place destroy,
/// which deliberately leaves the directory for the lane to be recreated there.
///
/// An implicit `default` has no declaration to materialise from, so sizing it
/// stays `jjfx pool <N>`, unchanged. This is deliberately called from the
/// Dispatch path rather than from `Repository::with_pool`: `status` and
/// `pool list` build the same handles to *read* every Pool, and must keep
/// reporting a declared-but-unmaterialised lane without creating its state.
fn materialise_declared_pool(repository: &Repository, user: &Config) -> Result<()> {
    if repository.state_store().pool_document_exists() {
        return Ok(());
    }
    let Some(declaration) = user
        .repo_config(repository.root())?
        .pools
        .get(repository.selected_pool().as_str())
        .cloned()
    else {
        return Ok(());
    };
    let capacity = PoolCapacity::new(declaration.size.unwrap_or(0))?;
    for worker in repository.worker_pool().materialise(capacity)? {
        eprintln!("  Created {worker}");
    }
    Ok(())
}

/// One Worker Pool as `status` and `pool list` report it: its declaration, its
/// materialised state, its recorded spend, and the Workers and held Tickets
/// that belong to it.
struct PoolStatus {
    /// The pool's name. `default` is always present.
    name: String,
    /// Whether the pool exists only implicitly - `default` when no declaration
    /// names it. Every named pool is declared, so this is `false` for them.
    implicit: bool,
    /// The pool's size: its materialised state's, else its declared size.
    size: Option<u64>,
    /// The pool's materialised Worker count, when its state exists.
    workers: Option<usize>,
    /// The pool's own cap, or `None` when the declaration sets none and the
    /// Repository-wide ceiling is the only limit.
    cap: Option<SpendCap>,
    /// The window the pool's `spend` and `cap` are stated over: the pool's own,
    /// else the global `spend.window`.
    window: SpendWindow,
    /// The pool's recorded cost, or `None` when its record store could not be
    /// read (so a frontend says "unavailable" rather than "$0").
    cost: Option<PoolCostSnapshot>,
    /// The pool's materialised Workers.
    members: Vec<WorkerSnapshot>,
    /// The Tickets this pool's last capped Dispatch held back, in input order.
    waiting: Vec<String>,
    /// The pool's durable work queue, in queue order.
    queue: Vec<crate::engine::QueueItem>,
    /// The pool's configured Agent Runtime Profile, when its state names one.
    profile: Option<AgentRuntimeProfile>,
}

impl PoolStatus {
    /// The pool's recorded spend in its cap window as `$d.dddd`, or `-` when the
    /// record store could not be read.
    fn spend_display(&self) -> String {
        self.cost.as_ref().map_or_else(
            || "-".to_owned(),
            |cost| cost.window(self.window).cost_display(),
        )
    }

    /// The pool's own cap as `$d.dddd`, or `-` when it sets none.
    fn cap_display(&self) -> String {
        self.cap
            .map_or_else(|| "-".to_owned(), SpendCap::limit_display)
    }
}

/// Everything `status` and `pool list` report: every pool, the Repository-wide
/// spend their subtotals sit under, the concurrency governor's view, and the
/// orphan pool directories no pool claims.
struct PoolReport {
    pools: Vec<PoolStatus>,
    /// The Repository-wide recorded spend, or `None` when a record store could
    /// not be read.
    total_cost: Option<PoolCostSnapshot>,
    /// The concurrency cap in force for this Repository, or `None` when no
    /// `[dispatch] max_concurrent` is set. One number, not per pool: the cap is
    /// a property of the Repository's dispatch settings, while a pool's size is
    /// its own capacity declaration.
    limit: Option<usize>,
    /// The Runs in flight across every pool: each Worker the Pool records as
    /// busy, whatever Dispatch started it.
    running: usize,
    /// The Tickets the last capped Dispatch held back, across every pool.
    held: usize,
    /// Pool directories under `.jj/pools/` that no pool the Repository holds
    /// claims. Reported by name, never adopted.
    orphans: Vec<String>,
    /// Per-pool snapshot diagnostics, printed before the tables.
    diagnostics: Vec<String>,
    /// Local Ticket writeback failures, reported after the pass.
    failures: Vec<String>,
}

fn pool_list_command(repository: &Repository, json: bool) -> Result<()> {
    warn_when_behind_trunk(repository);
    let config = crate::config::load()?;
    // `status`/`pool list` read Pool state, so they also reconcile local Tickets:
    // a finished Run resolves its Ticket here, with no Dispatch needed. The
    // source is resolved for this Repository, so its own `[tickets]` selection
    // is honoured.
    let tickets = config.tickets_for(repository.root())?;
    let repo_config = config.repo_config(repository.root())?;
    let report = pool_report(&config, &repo_config, repository, &tickets)?;
    report_writeback_failures(&report.failures);
    // A Repository that neither declares a pool nor has any state is the
    // pre-pools shape: keep the old "no pool yet" guidance unchanged. Once a
    // pool is declared, listing the pools is the useful answer even when
    // `default` has no Workers yet.
    if repo_config.pools.is_empty() && report.pools.iter().all(|pool| pool.workers.is_none()) {
        bail!("No pool. Run: {PROGRAM} pool <N>");
    }
    // The watchdog's window is jjfx's own setting, read here rather than passed
    // down: `pool list` is the only CLI command that surfaces Run health, and
    // `cli::run` is already at the argument limit.
    let watchdog = config.watchdog.stuck_window();
    let now = Timestamp::now();
    if json {
        println!("{}", pool_list_json_string(&report, now, watchdog));
        return Ok(());
    }
    for diagnostic in &report.diagnostics {
        eprintln!("{diagnostic}");
    }
    render_pool_summaries(&report.pools);
    render_orphan_pools(&report.orphans);
    render_pool_workers(&report, now, watchdog);
    Ok(())
}

/// Reads every Worker Pool the Repository holds, reconciling each.
///
/// Reading all of them - not just `default` - is what lets one `status` describe
/// several lanes: each Worker and each held Ticket is tagged with the pool it
/// belongs to, and the per-pool subtotals sit under the Repository-wide total.
/// `default` is always first, the order [`Config::pool_names_for`] gives, so the
/// pre-pools single-pool output is unchanged. The orphan directories are the
/// pool names the Repository does *not* hold, which is why `default` counts as
/// held even when nothing declares it.
fn pool_report(
    config: &Config,
    repo_config: &RepoConfig,
    repository: &Repository,
    tickets: &TicketsConfig,
) -> Result<PoolReport> {
    let names = config.pool_names_for(repository.root())?;
    let held: std::collections::BTreeSet<PoolName> = names.iter().cloned().collect();
    let orphans = repository
        .orphan_pool_directories(&held)
        .map_err(|error| anyhow::anyhow!("{error}"))?
        .iter()
        .map(|name| name.as_str().to_owned())
        .collect();
    let mut pools = Vec::new();
    let mut diagnostics = Vec::new();
    let mut failures = Vec::new();
    for name in &names {
        let scoped = repository.clone().with_pool(name.clone());
        let (snapshot, pool_failures) = reconcile_pass(&scoped, tickets);
        failures.extend(pool_failures);
        diagnostics.extend(
            snapshot
                .diagnostics()
                .iter()
                .map(|diagnostic| diagnostic.message().to_owned()),
        );
        let declaration = repo_config.pools.get(name.as_str());
        let cap = declaration
            .map(|declaration| declaration.spend_cap(&config.spend))
            .transpose()?
            .flatten();
        pools.push(PoolStatus {
            name: name.as_str().to_owned(),
            implicit: declaration.is_none(),
            size: snapshot
                .pool()
                .and_then(|pool| u64::try_from(pool.size()).ok())
                .or_else(|| {
                    declaration
                        .and_then(|pool| pool.size)
                        .map(|size| size as u64)
                }),
            workers: snapshot.pool().map(|_| snapshot.workers().len()),
            cap,
            window: cap.map_or(config.spend.window, SpendCap::window),
            cost: scoped.pool_cost().ok(),
            members: snapshot.workers().to_vec(),
            waiting: snapshot.waiting().to_vec(),
            queue: snapshot.queue().to_vec(),
            profile: snapshot.pool().and_then(|pool| pool.profile()).cloned(),
        });
    }
    let total_cost = repository.repository_cost(&names).ok();
    let limit = config
        .dispatch_cap_for(repository.root())?
        .map(crate::engine::ConcurrencyCap::limit);
    let running = pools
        .iter()
        .flat_map(|pool| &pool.members)
        .filter(|worker| worker.status() == WorkerStatus::Busy)
        .count();
    let held = pools.iter().map(|pool| pool.waiting.len()).sum();
    Ok(PoolReport {
        pools,
        total_cost,
        limit,
        running,
        held,
        orphans,
        diagnostics,
        failures,
    })
}

/// The `Pools:` table: every pool, its size, its recorded spend against its own
/// cap, and whether a declaration names it.
fn render_pool_summaries(pools: &[PoolStatus]) {
    println!("Pools:");
    println!(
        "{:<12} {:<6} {:<8} {:<11} {:<11} {:<8} DECLARED",
        "POOL", "SIZE", "WORKERS", "SPEND", "CAP", "WINDOW"
    );
    for pool in pools {
        let size = pool
            .size
            .map_or_else(|| "-".to_owned(), |size| size.to_string());
        let workers = pool
            .workers
            .map_or_else(|| "-".to_owned(), |workers| workers.to_string());
        let declared = if pool.implicit { "no" } else { "yes" };
        println!(
            "{:<12} {:<6} {:<8} {:<11} {:<11} {:<8} {declared}",
            pool.name,
            size,
            workers,
            pool.spend_display(),
            pool.cap_display(),
            pool.window.as_str()
        );
    }
    println!();
}

/// Names the pool directories no pool claims, so a stray directory is told
/// rather than left to guess about. Nothing is adopted or removed.
fn render_orphan_pools(orphans: &[String]) {
    if orphans.is_empty() {
        return;
    }
    println!(
        "Orphaned pool directories (no pool claims them; their state is left in place): {}",
        orphans.join(", ")
    );
    println!();
}

/// The Worker table across every pool, each row naming its pool, then the
/// Repository-wide summary and the held-back Tickets.
fn render_pool_workers(report: &PoolReport, now: Timestamp, watchdog: StuckWindow) {
    println!(
        "{:<10} {:<12} {:<12} {:<10} {:<14} {:<11} ELAPSED",
        "WORKER", "POOL", "NAME", "STATUS", "TICKET", "SPEND"
    );
    println!(
        "{:<10} {:<12} {:<12} {:<10} {:<14} {:<11} -------",
        "------", "----", "----", "------", "------", "-----"
    );
    let mut counts = [0usize; 4];
    let mut stuck = 0usize;
    for pool in &report.pools {
        for worker in &pool.members {
            let index = match worker.status() {
                WorkerStatus::Idle => 0,
                WorkerStatus::Busy => 1,
                WorkerStatus::Done => 2,
                WorkerStatus::Failed => 3,
            };
            counts[index] += 1;
            let short = short_worker_id(worker.worker_id().as_str());
            let name = if worker.alias().is_empty() {
                "-"
            } else {
                worker.alias()
            };
            let ticket = worker.ticket().unwrap_or("-");
            let spend = worker_spend(pool.cost.as_ref(), worker.worker_id().as_str());
            let elapsed = elapsed_display(worker.started_at(), worker.completed_at());
            println!(
                "{short:<10} {:<12} {name:<12} {:<10} {ticket:<14} {spend:<11} {elapsed}",
                pool.name,
                worker.status().as_str()
            );
            // A stuck Run reads as `busy`, which is exactly the guessing the
            // watchdog removes, so say how long it has been quiet.
            if worker.health(now, watchdog).is_stuck() {
                stuck += 1;
                let quiet = worker.run_progress().last_progress_at().map_or_else(
                    || "-".to_owned(),
                    |since| seconds_display(now.duration_since(since)),
                );
                println!("           stuck: no progress for {quiet}");
            }
            // A terminal Worker's reason is only useful when surfaced here: the
            // raw diagnosis lives in its Run log, and the persisted error may be
            // a generic fallback.
            if worker.status() == WorkerStatus::Failed {
                if let Some(error) = worker.error().filter(|error| !error.is_empty()) {
                    println!("           reason: {error}");
                }
                if let Some(code) = worker.exit_code() {
                    println!("           exit: {code}");
                }
                if let Some(log) = worker.log_file().filter(|log| !log.is_empty()) {
                    println!("           log: {log}");
                }
            }
        }
    }
    println!();
    // A pool's Profile line names its pool once there is more than one, so a
    // multi-lane Repository cannot read the first profile as the only one.
    let named = report.pools.len() > 1;
    for pool in &report.pools {
        if let Some(profile) = &pool.profile {
            if named {
                println!("Profile ({}): {}", pool.name, render_profile(profile));
            } else {
                println!("Profile: {}", render_profile(profile));
            }
        }
    }
    let stuck = if stuck > 0 {
        format!(", {stuck} stuck")
    } else {
        String::new()
    };
    let size: u64 = report.pools.iter().filter_map(|pool| pool.size).sum();
    println!(
        "Pool: {} idle, {} busy{}, {} done, {} failed ({} total)",
        counts[0], counts[1], stuck, counts[2], counts[3], size
    );
    println!("{}", spend_summary(report.total_cost.as_ref()));
    render_waiting(&report.pools);
    render_governor(report);
}

/// The concurrency governor's view: the cap in force, how many Runs are in
/// flight, and how many Tickets the last capped Dispatch held back.
///
/// Printed whatever the state, so the limit is visible on a quiet Repository
/// too - the point of a governor is that the ceiling is known before it binds.
fn render_governor(report: &PoolReport) {
    let limit = report.limit.map_or_else(
        || "no cap".to_owned(),
        |limit| format!("dispatch.max_concurrent = {limit}"),
    );
    let held = if report.held > 0 {
        format!(", {} held back", report.held)
    } else {
        String::new()
    };
    println!(
        "Concurrency: {} Run(s) in flight ({limit}){held}",
        report.running
    );
}

/// The held-back Tickets, each named with the pool whose Dispatch held it.
fn render_waiting(pools: &[PoolStatus]) {
    let held = pools
        .iter()
        .flat_map(|pool| {
            pool.waiting
                .iter()
                .map(|ticket| format!("{ticket} ({})", pool.name))
        })
        .collect::<Vec<_>>();
    if !held.is_empty() {
        println!("Waiting: {}", held.join(", "));
    }
}

/// One Worker's recorded spend in its pool, or `-` when it recorded no Run.
fn worker_spend(cost: Option<&PoolCostSnapshot>, worker: &str) -> String {
    cost.and_then(|cost| cost.worker(worker))
        .map_or_else(|| "-".to_owned(), crate::engine::PoolCost::cost_display)
}

/// The Repository-wide cost/usage summary line, the total the per-pool subtotals
/// sit under.
fn spend_summary(cost: Option<&PoolCostSnapshot>) -> String {
    let Some(cost) = cost else {
        return "Spend: record store unavailable".to_owned();
    };
    let total = cost.total();
    let unpriced = if total.is_unmeasured() {
        // Every Run reported no cost, so the total is an absence of data, not a
        // measurement. Say so rather than let `$0.0000` read as "these Runs were
        // free".
        " (no Run reported a cost; the total is not a measurement)".to_owned()
    } else if total.unpriced_runs() > 0 {
        format!(" ({} without a reported cost)", total.unpriced_runs())
    } else {
        String::new()
    };
    format!(
        "Spend: {} today, {} last 7 days, {} total across {} Run(s){}; tokens {}",
        cost.today().cost_display(),
        cost.week().cost_display(),
        total.cost_display(),
        total.runs(),
        unpriced,
        total.tokens_display()
    )
}

/// The short Worker id (without the `worker-` prefix) used in the list view.
fn short_worker_id(worker_id: &str) -> &str {
    worker_id.strip_prefix("worker-").unwrap_or(worker_id)
}

/// The Pool status JSON document:
/// `{"workers": [ ... ], "waiting": [ ... ], "governor": { ... },
/// "queue": [ ... ], "pools": [ ... ], "orphans": [ ... ]}`.
///
/// A top-level object rather than the bare Worker array: the human table prints
/// state that is not per-Worker (the `Waiting:` line), and a bare array has
/// nowhere to put it. The wrapper is the seam for that state and for the next
/// line the table grows, so the JSON can always carry what the table prints.
///
/// `workers` and `waiting` stay flat and each entry names its pool, so
/// `jq '.workers[] | select(.pool == "bugs")'` answers the per-pool question
/// without a second shape. `queue` is the durable work queue, also flat and
/// pool-tagged, so a step that was queued, claimed or abandoned is visible as
/// structure rather than only as live Worker state. `pools` names every pool the
/// Repository holds,
/// including an implicit `default`, with its size and its recorded spend against
/// its own cap - a subtotal under the Repository-wide total. `orphans` names
/// pool directories no pool claims. Encoded by hand so the emitted field order
/// is fixed.
fn pool_list_json_string(report: &PoolReport, now: Timestamp, watchdog: StuckWindow) -> String {
    let mut out = String::from("{\n  \"workers\": [");
    let mut first = true;
    for pool in &report.pools {
        for worker in &pool.members {
            if !first {
                out.push(',');
            }
            first = false;
            out.push_str("\n    ");
            out.push_str(&worker_json(
                worker,
                &pool.name,
                pool.cost.as_ref(),
                now,
                watchdog,
            ));
        }
    }
    if first {
        out.push_str("],\n");
    } else {
        out.push_str("\n  ],\n");
    }
    out.push_str("  \"waiting\": [");
    let mut first = true;
    for pool in &report.pools {
        for ticket in &pool.waiting {
            if !first {
                out.push_str(", ");
            }
            first = false;
            out.push_str(&waiting_json(ticket, &pool.name));
        }
    }
    out.push_str("],\n");
    out.push_str(&governor_json(report));
    out.push_str("  \"queue\": [");
    let mut first = true;
    for pool in &report.pools {
        for item in &pool.queue {
            if !first {
                out.push_str(", ");
            }
            first = false;
            out.push_str(&queue_json(item, &pool.name));
        }
    }
    out.push_str("],\n");
    out.push_str("  \"pools\": [");
    for (index, pool) in report.pools.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("\n    ");
        out.push_str(&pool_summary_json(pool));
    }
    if report.pools.is_empty() {
        out.push_str("],\n");
    } else {
        out.push_str("\n  ],\n");
    }
    out.push_str("  \"orphans\": [");
    for (index, orphan) in report.orphans.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&json_string(orphan));
    }
    out.push_str("]\n}");
    out
}

/// One held-back Ticket, the shape `status --json` emits per waiting entry: the
/// Ticket and the pool whose capped Dispatch held it.
fn waiting_json(ticket: &str, pool: &str) -> String {
    format!(
        "{{\"ticket\": {}, \"pool\": {}}}",
        json_string(ticket),
        json_string(pool)
    )
}

/// The concurrency governor, the shape `status --json` emits as `governor`:
/// what the limit is (`limit`, null when uncapped), how many Runs are in flight
/// (`running`), and how many Tickets the last capped Dispatch held back
/// (`held`).
///
/// One object for the whole Repository rather than one per pool: the limit is a
/// single `[dispatch] max_concurrent` setting, while a pool's own capacity is
/// the `size` on its `pools` entry. A frontend reads this to answer "am I at the
/// ceiling, and if so why is nothing starting?" without counting Workers.
fn governor_json(report: &PoolReport) -> String {
    let limit = report
        .limit
        .map_or_else(|| "null".to_owned(), |limit| limit.to_string());
    format!(
        "  \"governor\": {{\"limit\": {limit}, \"running\": {}, \"held\": {}}},\n",
        report.running, report.held
    )
}

/// One queued step, the shape `status --json` emits per queue entry.
///
/// The durable record, not a view of it: what the step is (`step`, `kind`,
/// `subject`), its lifecycle (`state`, `sequence`), when it was queued and
/// claimed and by what (`queued_at`, `claimed_at`, `claimed_by`, `started_at`),
/// how it ended (`finished_at`, `detail`), and what a resumption needs
/// (`in_flight`) - tagged with the pool whose queue holds it, as `waiting` is.
fn queue_json(item: &crate::engine::QueueItem, pool: &str) -> String {
    let in_flight = item.in_flight.as_ref().map_or_else(
        || "null".to_owned(),
        |record| {
            format!(
                "{{\"worker\": {}, \"workspace\": {}, \"branch\": {}, \"runtime\": {}, \"provider\": {}, \"model\": {}, \"session\": {}, \"pid\": {}}}",
                json_option(record.worker.as_ref().map(WorkerId::as_str)),
                json_option(record.workspace.as_deref()),
                json_option(record.branch.as_deref()),
                json_option(record.runtime.as_deref()),
                json_option(record.provider.as_deref()),
                json_option(record.model.as_deref()),
                json_option(record.session.as_deref()),
                record
                    .pid
                    .map_or_else(|| "null".to_owned(), |pid| pid.to_string()),
            )
        },
    );
    format!(
        "{{\"step\": {}, \"kind\": {}, \"subject\": {}, \"state\": {}, \"pool\": {}, \"sequence\": {}, \"queued_at\": {}, \"claimed_at\": {}, \"claimed_by\": {}, \"started_at\": {}, \"finished_at\": {}, \"detail\": {}, \"in_flight\": {}}}",
        json_string(item.step.as_str()),
        json_string(item.kind.as_str()),
        json_string(&item.subject),
        json_string(item.state.as_str()),
        json_string(pool),
        item.sequence,
        json_string(item.queued_at.as_str()),
        json_option(item.claimed_at.as_ref().map(|at| at.as_str())),
        json_option(item.claimed_by.as_ref().map(WorkerId::as_str)),
        json_option(item.started_at.as_ref().map(|at| at.as_str())),
        json_option(item.finished_at.as_ref().map(|at| at.as_str())),
        json_option(item.detail.as_deref()),
        in_flight,
    )
}

/// One pool's JSON object, the shape `status --json` emits per pool.
fn pool_summary_json(pool: &PoolStatus) -> String {
    let fields = [
        ("name", json_string(&pool.name)),
        (
            "size",
            pool.size
                .map_or_else(|| "null".to_owned(), |size| size.to_string()),
        ),
        (
            "workers",
            pool.workers
                .map_or_else(|| "null".to_owned(), |workers| workers.to_string()),
        ),
        ("implicit", pool.implicit.to_string()),
        ("spend", pool.spend_json()),
        ("cap", pool.cap_json()),
        ("window", json_string(pool.window.as_str())),
    ];
    let mut out = String::from("{");
    for (index, (key, value)) in fields.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&format!("\"{key}\": {value}"));
    }
    out.push('}');
    out
}

impl PoolStatus {
    /// The pool's recorded spend in its cap window as a JSON number, or `null`
    /// when the record store could not be read.
    fn spend_json(&self) -> String {
        self.cost.as_ref().map_or_else(
            || "null".to_owned(),
            |cost| json_usd(cost.window(self.window).cost_micro_usd()),
        )
    }

    /// The pool's own cap as a JSON number, or `null` when it sets none.
    fn cap_json(&self) -> String {
        self.cap
            .map_or_else(|| "null".to_owned(), |cap| json_usd(cap.limit_micro_usd()))
    }
}

/// A US-dollar amount as a JSON number, six decimal places like `cost_usd`.
fn json_usd(micro_usd: u64) -> String {
    format!("{:.6}", micro_usd as f64 / 1_000_000.0)
}

/// One Worker's JSON object, the shape `status --json` and `wait --json` emit
/// per Worker.
///
/// The field set and its order live here alone, so the two cannot describe the
/// same Worker differently. `pool` names the Worker's pool - `wait` reports the
/// pool it acts on - so a flat `workers[]` carries the pool dimension.
fn worker_json(
    worker: &WorkerSnapshot,
    pool: &str,
    cost: Option<&PoolCostSnapshot>,
    now: Timestamp,
    watchdog: StuckWindow,
) -> String {
    let fields = [
        ("worker", json_string(worker.worker_id().as_str())),
        (
            "name",
            json_string(if worker.alias().is_empty() {
                "-"
            } else {
                worker.alias()
            }),
        ),
        ("status", json_string(worker.status().as_str())),
        ("stuck", worker.health(now, watchdog).is_stuck().to_string()),
        ("ticket", json_option(worker.ticket())),
        (
            "elapsed",
            json_string(&elapsed_display(worker.started_at(), worker.completed_at())),
        ),
        ("error", json_option(worker.error())),
        (
            "exit_code",
            worker
                .exit_code()
                .map_or_else(|| "null".to_owned(), |code| code.to_string()),
        ),
        ("log_file", json_option(worker.log_file())),
        (
            "cost_usd",
            cost.and_then(|cost| cost.worker(worker.worker_id().as_str()))
                .map_or_else(
                    || "null".to_owned(),
                    |cost| format!("{:.6}", cost.cost_usd()),
                ),
        ),
        ("pool", json_string(pool)),
    ];
    let mut out = String::from("{\n");
    for (index, (key, value)) in fields.iter().enumerate() {
        if index > 0 {
            out.push_str(",\n");
        }
        out.push_str(&format!("      \"{key}\": {value}"));
    }
    out.push_str("\n    }");
    out
}

/// Encode a string as a JSON string literal.
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Encode an optional string as a JSON value (`null` when absent).
fn json_option(value: Option<&str>) -> String {
    value.map_or_else(|| "null".to_owned(), json_string)
}

/// Encode a slice of strings as a JSON array, in order.
fn json_string_array(values: &[String]) -> String {
    let mut out = String::from("[");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&json_string(value));
    }
    out.push(']');
    out
}

fn normalize_worker(value: &str) -> Result<WorkerId> {
    let value = if value.starts_with("worker-") {
        value.to_owned()
    } else {
        format!("worker-{value}")
    };
    Ok(WorkerId::parse(value)?)
}

fn remove_worker_command(repository: &Repository, value: &str) -> Result<()> {
    let worker = normalize_worker(value)?;
    let resize = repository
        .worker_pool()
        .remove_with_progress(worker.clone(), print_hook_output)?;
    eprintln!(
        "Removed {worker} (pool size: {})",
        resize.capacity().as_usize()
    );
    Ok(())
}

fn reset_worker_command(repository: &Repository, value: &str) -> Result<()> {
    let worker = normalize_worker(value)?;
    let outcome = WorkerActions::new(repository.clone()).reset(&worker)?;
    let _restoration = outcome.into_restoration();
    eprintln!("Reset {worker} to idle");
    Ok(())
}

fn elapsed_display(started: Option<&str>, completed: Option<&str>) -> String {
    let Some(started) = started.and_then(|value| Timestamp::from_str(value).ok()) else {
        return "-".to_owned();
    };
    let completed = completed
        .and_then(|value| Timestamp::from_str(value).ok())
        .unwrap_or_else(Timestamp::now);
    seconds_display(completed.duration_since(started))
}

/// A span of time as `Xm Ys`, the ELAPSED column's format. A span that runs
/// backwards (a clock that moved, or a timestamp from the future) reads as `-`.
fn seconds_display(elapsed: jiff::SignedDuration) -> String {
    let seconds = elapsed.as_secs_f64().trunc() as i64;
    if seconds < 0 {
        return "-".to_owned();
    }
    format!("{}m {}s", seconds / 60, seconds % 60)
}

/// `pool destroy [<name>] [--force]`: remove a named pool.
///
/// Removal is an undeclaration (ADR 0014): the declaration is the source of
/// truth, so removing the pool is removing its `[pools.<name>]` table. It is
/// also more than that (ADR 0012): the pool's state directory and its Workers'
/// Workspaces go with it, because leaving them would be a slower, quieter
/// version of the same leak - nothing would ever come back for them.
///
/// Removal drains. It refuses while a Worker is busy, naming them, and refuses
/// when a Worker holds work that is neither pushed nor merged, enumerating each
/// Worker's branch and its state. `--force` is the only path that discards work,
/// and it says so. Unlike `Clean`, which sweeps every non-default Workspace in
/// the Repository, this touches only the removed pool's own Workers - which is
/// what makes removing one lane safe while another keeps working.
///
/// `default` is not removable: a Repository always has at least one pool and
/// `jjfx pool <N>` targets it forever. A Repository that wants only named lanes
/// resizes `default` to zero instead, which is explicit and reversible.
fn destroy_pool_command(repository: &Repository, name: Option<&str>, force: bool) -> Result<()> {
    let name = name.unwrap_or(DEFAULT_POOL_NAME);
    if name == DEFAULT_POOL_NAME {
        bail!(
            "the `default` pool cannot be removed: a Repository always has at least one pool, \
             and `{PROGRAM} pool <N>` targets it. Resize it to zero instead: `{PROGRAM} pool 0`"
        );
    }
    destroy_pool(repository, name, force, &crate::config::load()?)
}

/// One Worker's contribution to the removal report: its current branch, and how
/// far that branch has been delivered.
struct PoolRemovalWorker {
    worker: WorkerId,
    /// The Worker's current bookmark, when it has one.
    branch: Option<String>,
    /// The branch's work lifecycle - `pushed`, `merged`, `dirty`, and so on.
    state: WorkState,
}

/// What removing a pool would touch, gathered before anything is touched.
struct PoolRemovalPlan {
    /// Workers whose Run is in progress. A drain refuses these.
    busy: Vec<WorkerId>,
    /// Every Worker in the pool, with its branch and delivery state.
    workers: Vec<PoolRemovalWorker>,
}

impl PoolRemovalPlan {
    /// The Workers holding work that is neither pushed nor merged.
    fn undelivered(&self) -> impl Iterator<Item = &PoolRemovalWorker> {
        self.workers
            .iter()
            .filter(|worker| !worker.state.is_delivered())
    }

    /// The report: every Worker, its branch, and whether that branch is pushed
    /// or merged. Printed before anything is discarded, and named in the
    /// refusals.
    fn report(&self) -> String {
        self.workers
            .iter()
            .map(|worker| {
                let branch = worker.branch.as_deref().unwrap_or("-");
                format!("  {}  {}  {}", worker.worker, branch, worker.state.label())
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Refuses the removal unless `force` overrides it, naming what stands in
    /// the way: a busy Worker, or a branch that is neither pushed nor merged.
    fn refuse_unless_forced(&self, name: &str, force: bool) -> Result<()> {
        if force {
            return Ok(());
        }
        if !self.busy.is_empty() {
            let workers = self
                .busy
                .iter()
                .map(WorkerId::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "pool `{name}` has busy Workers: {workers}. Wait for them to finish, or \
                 re-run with --force to discard their work"
            );
        }
        let undelivered = self.undelivered().collect::<Vec<_>>();
        if !undelivered.is_empty() {
            let workers = undelivered
                .iter()
                .map(|worker| worker.worker.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            // `?` is a failed read, not necessarily held work - say which, so
            // the operator can tell "push this" from "I could not tell".
            let unknown = undelivered
                .iter()
                .any(|worker| worker.state == WorkState::Unknown);
            let unread = if unknown {
                " A `?` marks a Worker whose branch state could not be read, not one known to \
                 hold work."
            } else {
                ""
            };
            bail!(
                "pool `{name}` holds work that is neither pushed nor merged:\n{}\n\
                 Deliver it (or push the branch), or re-run with --force to discard it. \
                 Undelivered Workers: {workers}.{unread}",
                self.report()
            );
        }
        Ok(())
    }
}

/// Gathers what removing this handle's Pool would touch, without touching
/// anything.
///
/// The Workers come from the Worker Pool's own state, so the plan is scoped to
/// this Pool and never to the Repository's other lanes. Each Worker's delivery state
/// is the [`WorkState`] its Workspace already reports - the same read the list
/// view uses, so the two cannot drift - and a Worker whose Workspace is not on
/// disk has no chain to lose and reads `clean`.
fn pool_removal_plan(repository: &Repository) -> Result<PoolRemovalPlan> {
    let snapshot = repository.read_worker_pool_snapshot();
    let busy = snapshot
        .workers()
        .iter()
        .filter(|worker| worker.status() == WorkerStatus::Busy)
        .map(|worker| worker.worker_id().clone())
        .collect();
    let present = snapshot
        .workers()
        .iter()
        .filter(|worker| {
            repository
                .worker_workspace_path(worker.worker_id())
                .is_dir()
        })
        .map(|worker| worker.worker_id().as_str().to_owned())
        .collect::<Vec<_>>();
    let states = crate::work::snapshot(
        &crate::work::SystemWorkReads::new(repository.root().to_path_buf()),
        &present,
    );
    let workers = snapshot
        .workers()
        .iter()
        .map(|worker| {
            // Every Worker with a Workspace on disk is in the snapshot's map;
            // one without has no chain to lose and reads `clean` rather than a
            // failed-read `?`, which would refuse a removal with nothing to
            // refuse.
            let state = states
                .get(worker.worker_id().as_str())
                .map_or(WorkState::Clean, |work| work.state);
            PoolRemovalWorker {
                worker: worker.worker_id().clone(),
                branch: worker.branch_name().map(str::to_owned),
                state,
            }
        })
        .collect();
    Ok(PoolRemovalPlan { busy, workers })
}

/// The removal, with the user-global config handed in so a test can drive it
/// without reading the developer's own config.
fn destroy_pool(repository: &Repository, name: &str, force: bool, user: &Config) -> Result<()> {
    let pool = PoolName::parse(name).map_err(|error| anyhow::anyhow!("{error}"))?;
    let existing = user.repo_config(repository.root())?;
    if !existing.pools.contains_key(name) {
        bail!(
            "no pool `{name}` is declared in {}",
            repo_config_display(repository)
        );
    }
    // One handle, narrowed to the pool being removed: the plan reads its state
    // and the removal writes it, so the two cannot disagree about the lane.
    let selected = repository.clone().with_pool(pool);

    // Everything the removal would touch is gathered and reported before a
    // single Workspace or state file moves, so a refusal costs nothing and a
    // forced removal has already said what it discards.
    let plan = pool_removal_plan(&selected)?;
    plan.refuse_unless_forced(name, force)?;
    if !plan.workers.is_empty() {
        eprintln!("Pool `{name}` Workers and their branches:");
        eprintln!("{}", plan.report());
    }
    if force {
        let discarded = plan.undelivered().collect::<Vec<_>>();
        if !discarded.is_empty() {
            eprintln!(
                "warning: --force discards {} Worker(s) holding work that is neither pushed nor \
                 merged: {}",
                discarded.len(),
                discarded
                    .iter()
                    .map(|worker| worker.worker.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if !plan.busy.is_empty() {
            eprintln!(
                "warning: --force terminates {} busy Worker(s): {}",
                plan.busy.len(),
                plan.busy
                    .iter()
                    .map(WorkerId::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    selected
        .remove_pool_with_progress(print_hook_output)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    crate::config::undeclare_pool(repository.root(), name, user)?;
    eprintln!(
        "Removed pool `{name}` from {}",
        repo_config_display(repository)
    );
    Ok(())
}

fn configured_runtime(repository: &Repository) -> AgentRuntime {
    repository
        .worker_pool()
        .snapshot()
        .pool()
        .and_then(|pool| pool.agent_runtime())
        .unwrap_or(AgentRuntime::Claude)
}

/// Claims or resolves local Tickets for the current Worker Pool.
///
/// A no-op unless `[tickets] source = "local"`. The writeback is a side effect of
/// a command that already succeeded, so a failure is reported rather than raised.
fn write_back_local_tickets(repository: &Repository, tickets: &TicketsConfig) {
    let (_, failures) = reconcile_pass(repository, tickets);
    report_writeback_failures(&failures);
}

/// One reconciliation pass: observe Worker Runs, then write local Tickets back.
///
/// The single mechanism behind `status`/`pool list`, `ticket reconcile` and
/// `wait`: a reconcile is one pass, and `wait` is this pass repeated until a
/// condition holds. Returns the post-pass snapshot and any writeback failures,
/// which the caller reports as loudly as its own contract requires.
fn reconcile_pass(
    repository: &Repository,
    tickets: &TicketsConfig,
) -> (WorkerPoolSnapshot, Vec<String>) {
    let snapshot = repository.worker_pool().reconcile_runs();
    let failures = apply_local_writeback(repository, &snapshot, tickets);
    (snapshot, failures)
}

/// Reports the failures of one local Ticket writeback.
///
/// A writeback is a side effect of a command that already succeeded, so a
/// failure is reported rather than raised.
fn report_writeback_failures(failures: &[String]) {
    for failure in failures {
        eprintln!("local Ticket writeback failed: {failure}");
    }
}

/// Reconcile local Ticket state on demand, with no Dispatch and no ready Ticket.
///
/// Resolves Tickets whose Run finished and claims those a live Worker is running,
/// reading the durable Run Record store so a finished Run is reconciled even
/// after `pool reset` cleared the Worker's Ticket association. Fails loudly: an
/// explicit reconcile the operator asked for should not report success when a
/// Ticket could not be written.
fn ticket_reconcile_command(
    repository: &Repository,
    tickets_config: fn(&Repository) -> Result<TicketsConfig>,
) -> Result<()> {
    let tickets = tickets_config(repository)?;
    if tickets.source != TicketSource::Local {
        eprintln!("Ticket source is not local; nothing to reconcile");
        return Ok(());
    }
    let (_, failures) = reconcile_pass(repository, &tickets);
    if failures.is_empty() {
        eprintln!("Reconciled local Tickets");
        return Ok(());
    }
    for failure in &failures {
        eprintln!("local Ticket writeback failed: {failure}");
    }
    bail!("local Ticket reconcile failed")
}

/// How one Ticket stands in the backlog `ticket list` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TicketVerdict {
    /// In the Ready set: a `dispatch --all` would take it right now.
    Ready,
    /// Discovered but not runnable, with the reason a Dispatch would print.
    Blocked(String),
    /// Resolved: the work is finished, so nothing stops it and nothing is
    /// waiting for it.
    Done,
}

impl TicketVerdict {
    /// The stable machine-readable state.
    const fn state(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Blocked(_) => "blocked",
            Self::Done => "done",
        }
    }

    /// Whether the Ticket is ready right now.
    const fn ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    /// Why the Ticket is not ready, or `None` when it is ready or resolved.
    fn reason(&self) -> Option<&str> {
        match self {
            Self::Blocked(reason) => Some(reason),
            Self::Ready | Self::Done => None,
        }
    }
}

/// One Ticket in the `ticket list` output: what it is, its Status, its
/// Blockers, and how it stands right now.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TicketListEntry {
    ticket: String,
    /// The Ticket's title, absent for a source that reports a Ticket it cannot
    /// describe in full.
    title: Option<String>,
    /// The Ticket's own `Status:`, absent when the source did not report one.
    status: Option<String>,
    verdict: TicketVerdict,
    /// The Ticket's declared Blockers, resolved to Ticket ids.
    blockers: Vec<String>,
}

/// The backlog `ticket list` reports, folded across the lanes a `--all`
/// Dispatch would discover over.
///
/// A Ticket several lanes see is one entry: ready wins, because a Ticket any
/// lane would take is dispatchable, and when no lane takes it the first lane's
/// reason is kept - one reason is enough to say what stops it. Entries keep
/// discovery order, so the list reads like the source's own scan.
#[derive(Debug, Default)]
struct TicketBacklog {
    entries: Vec<TicketListEntry>,
    index: BTreeMap<String, usize>,
}

impl TicketBacklog {
    /// Folds one local Ticket's readiness in, with the lane's verdict.
    ///
    /// The verdict is the lane's `ReadyTickets`, so it carries the label gate
    /// only `TicketDiscovery` applies; the Status and Blockers come from the
    /// readiness, which the source computed for the same status. A Ticket the
    /// lane neither took nor diagnosed is resolved - the local source reports no
    /// reason for a finished Ticket, so its absence from both sets is what
    /// "finished" means.
    fn record(&mut self, readiness: &LocalTicketReadiness, ready: &ReadyTickets) {
        let ticket = readiness.ticket();
        let id = ticket.id();
        let verdict = if ready
            .tickets()
            .iter()
            .any(|candidate| candidate.id().as_str() == id)
        {
            TicketVerdict::Ready
        } else if let Some(diagnostic) = ready
            .diagnostics()
            .iter()
            .find(|diagnostic| diagnostic.subject() == id)
        {
            TicketVerdict::Blocked(diagnostic.reason().to_owned())
        } else {
            TicketVerdict::Done
        };
        self.insert(TicketListEntry {
            ticket: id.to_owned(),
            title: (!ticket.title().is_empty()).then(|| ticket.title().to_owned()),
            status: (!ticket.status().is_empty()).then(|| ticket.status().to_owned()),
            verdict,
            blockers: readiness.blockers().to_vec(),
        });
    }

    /// Folds one lane's discovery in for a source with no richer backlog: the
    /// Ready Tickets it returned, and a blocked entry for each diagnostic it
    /// produced.
    fn record_discovery(&mut self, ready: &ReadyTickets) {
        for ticket in ready.tickets() {
            self.insert(TicketListEntry {
                ticket: ticket.id().as_str().to_owned(),
                title: Some(ticket.title().as_str().to_owned()),
                status: Some(ticket.status().as_str().to_owned()),
                verdict: TicketVerdict::Ready,
                blockers: ready
                    .blockers(ticket.id())
                    .iter()
                    .map(|blocker| blocker.as_str().to_owned())
                    .collect(),
            });
        }
        for diagnostic in ready.diagnostics() {
            self.insert(TicketListEntry {
                ticket: diagnostic.subject().to_owned(),
                title: None,
                status: None,
                verdict: TicketVerdict::Blocked(diagnostic.reason().to_owned()),
                blockers: Vec::new(),
            });
        }
    }

    /// Inserts one entry, merging it with an entry for the same Ticket.
    fn insert(&mut self, entry: TicketListEntry) {
        if let Some(&position) = self.index.get(&entry.ticket) {
            let existing = &mut self.entries[position];
            // A lane that takes the Ticket wins outright, and brings its fuller
            // description with it; otherwise the first lane's reason stands.
            if entry.verdict.ready() && !existing.verdict.ready() {
                *existing = entry;
            }
            return;
        }
        self.index.insert(entry.ticket.clone(), self.entries.len());
        self.entries.push(entry);
    }

    /// Returns the folded entries in discovery order.
    fn entries(&self) -> &[TicketListEntry] {
        &self.entries
    }
}

/// `ticket list`: every Ticket the configured source discovers, with its Status,
/// its Blockers and whether it is ready right now.
///
/// **Read-only.** It discovers through the source and reports what it finds: it
/// reconciles no Run, writes no Ticket and moves no Worker, so a `--json`
/// consumer can poll it in a loop without changing what it is reading. That is
/// the point of the command - `status`/`pool list` reconcile local Tickets as
/// they read, which is right for a frontend but wrong for a consumer asking only
/// "what is there".
///
/// Readiness is the verdict `dispatch --all` prints its blocked line from: the
/// lanes are the lanes a `--all` Dispatch would fill (`discovery_lanes`), the
/// filter each lane discovers by is `lane_filter`, and for a local source the
/// Status and Blockers come from the same `LocalTickets::backlog` the Dispatch
/// discovery payload is built on. The two cannot disagree.
fn ticket_list_command(
    repository: &Repository,
    args: &TicketListArgs,
    tickets_config: fn(&Repository) -> Result<TicketsConfig>,
) -> Result<()> {
    let config = crate::config::load()?;
    let repo_config = config.repo_config(repository.root())?;
    let available = config.pool_names_for(repository.root())?;
    let routes = config.pool_routes_for(repository.root())?;
    let tickets = tickets_config(repository)?;
    // The list always fans out the way `dispatch --all` does: an operator asking
    // "what is ready" is asking about every lane that would take work, and
    // `--pool` narrows it to one.
    let lanes = discovery_lanes(
        &available,
        &routes,
        args.pool.as_deref(),
        args.label.as_deref(),
        true,
    )?;
    let filters = lanes
        .iter()
        .map(|lane| lane_filter(lane, &repo_config, &tickets))
        .collect::<Result<Vec<_>>>()?;
    let discovery = TicketDiscovery::new(configured_ticket_query(
        repository.root(),
        &tickets,
        configured_runtime(repository),
        &global_ready_label(args.label.as_deref()),
    )?);

    let mut backlog = TicketBacklog::default();
    let local = (tickets.source == TicketSource::Local).then(|| {
        LocalTickets::new(
            repository.root().join(&tickets.local.root),
            tickets.local.ready_status.clone(),
            tickets.local.resolved_status.clone(),
        )
    });
    for filter in &filters {
        let ready = discovery.ready_tickets(filter)?;
        match &local {
            // The lane's Ready set is the verdict - it is what a Dispatch would
            // take, label gate and all - and the backlog supplies the Status and
            // Blockers the verdict alone does not carry.
            Some(local) => {
                for readiness in local.backlog(filter.status().as_str())? {
                    backlog.record(&readiness, &ready);
                }
            }
            None => backlog.record_discovery(&ready),
        }
    }

    let reports = lanes
        .iter()
        .zip(&filters)
        .map(|(lane, filter)| LaneReport {
            pool: lane.pool.as_str(),
            label: filter.label(),
            status: filter.status().as_str(),
        })
        .collect::<Vec<_>>();
    if args.json {
        println!("{}", ticket_list_json(&reports, backlog.entries()));
        return Ok(());
    }
    render_ticket_backlog(&reports, backlog.entries());
    Ok(())
}

/// One lane `ticket list` evaluated, as the prose and JSON report it.
struct LaneReport<'a> {
    pool: &'a str,
    label: &'a str,
    status: &'a str,
}

/// The prose rendering of the backlog: the lanes searched, then one row per
/// Ticket, so an empty list still says which lanes were looked at.
fn render_ticket_backlog(lanes: &[LaneReport<'_>], entries: &[TicketListEntry]) {
    println!("Lanes: {}", lane_summary(lanes));
    if entries.is_empty() {
        println!("No Tickets found");
        return;
    }
    println!("{:<32} {:<16} {:<8} DETAIL", "TICKET", "STATUS", "STATE");
    println!("{:<32} {:<16} {:<8} ------", "------", "------", "-----");
    let mut counts = [0usize; 3];
    for entry in entries {
        counts[match entry.verdict {
            TicketVerdict::Ready => 0,
            TicketVerdict::Blocked(_) => 1,
            TicketVerdict::Done => 2,
        }] += 1;
        println!(
            "{:<32} {:<16} {:<8} {}",
            entry.ticket,
            entry.status.as_deref().unwrap_or("-"),
            entry.verdict.state(),
            entry.verdict.reason().unwrap_or("")
        );
    }
    println!();
    println!(
        "{} ticket(s): {} ready, {} blocked, {} done",
        entries.len(),
        counts[0],
        counts[1],
        counts[2]
    );
}

/// The lanes a report evaluated, as one readable clause.
fn lane_summary(lanes: &[LaneReport<'_>]) -> String {
    lanes
        .iter()
        .map(|lane| {
            format!(
                "{} (label {:?}, status {:?})",
                lane.pool, lane.label, lane.status
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The machine-readable rendering of the backlog.
///
/// An object naming the lanes searched and the Tickets found, so an empty list
/// still says which lanes were looked at and under which label and status. Each
/// Ticket carries the same fields whichever source produced it.
fn ticket_list_json(lanes: &[LaneReport<'_>], entries: &[TicketListEntry]) -> String {
    let mut out = String::from("{\n  \"lanes\": [");
    for (index, lane) in lanes.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "\n    {{\n      \"pool\": {},\n      \"label\": {},\n      \"status\": {}\n    }}",
            json_string(lane.pool),
            json_string(lane.label),
            json_string(lane.status)
        ));
    }
    out.push_str("\n  ],\n  \"tickets\": [");
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let fields = [
            ("ticket", json_string(&entry.ticket)),
            ("title", json_option(entry.title.as_deref())),
            ("status", json_option(entry.status.as_deref())),
            ("state", json_string(entry.verdict.state())),
            ("ready", entry.verdict.ready().to_string()),
            ("blockers", json_string_array(&entry.blockers)),
            ("reason", json_option(entry.verdict.reason())),
        ];
        out.push_str("\n    {\n");
        for (position, (key, value)) in fields.iter().enumerate() {
            if position > 0 {
                out.push_str(",\n");
            }
            out.push_str(&format!("      \"{key}\": {value}"));
        }
        out.push_str("\n    }");
    }
    out.push_str("\n  ]\n}");
    out
}

/// Reports why a Ticket the user named by hand is not in the Ready set, then lets
/// the Dispatch proceed: naming a Ticket is deliberate, so naming it is the
/// override.
///
/// The reason comes from the same seam the `--all` path uses -
/// `ReadyTickets::diagnostics`, which the local source fills from
/// `LocalTickets::not_ready_reason` - so the two paths cannot drift apart. Only
/// the local source can explain a named Ticket it knows about but did not return
/// as ready: the Linear query filters by label and status, so a named Ticket it
/// does not return carries no reason to report, and asking would add a query and a
/// failure mode to every named Dispatch for nothing.
fn report_named_ticket_readiness(
    repository: &Repository,
    args: &DispatchArgs,
    repo_config: &RepoConfig,
    tickets_config: fn(&Repository) -> Result<TicketsConfig>,
) -> Result<()> {
    let tickets = tickets_config(repository)?;
    if tickets.source != TicketSource::Local || args.tickets.is_empty() {
        return Ok(());
    }
    // Readiness, not routing: the report answers "is this Ticket ready to run",
    // so it discovers by the global ready label rather than the selected pool's
    // routing label. A Ticket tagged for another lane is still ready. The
    // *status*, though, is the selected pool's: that pool is where the Ticket
    // will run, so readiness is whatever that lane declares.
    let label = global_ready_label(args.label.as_deref());
    let status = TicketStatus::parse(
        repo_config.pool_ready_status(repository.selected_pool(), tickets.ready_status()),
    )?;
    let filter = ReadyTicketFilter::new(&label, status)?;
    let runtime = configured_runtime(repository);
    let discovery = TicketDiscovery::new(configured_ticket_query(
        repository.root(),
        &tickets,
        runtime,
        &label,
    )?);
    let ready = discovery.ready_tickets(&filter)?;
    for ticket in &args.tickets {
        if let Some(diagnostic) = ready
            .diagnostics()
            .iter()
            .find(|diagnostic| diagnostic.subject() == ticket)
        {
            eprintln!(
                "Ticket {} is not ready: {}; dispatching anyway",
                ticket,
                diagnostic.reason()
            );
        }
    }
    Ok(())
}

/// One Direct Dispatch request per ready Ticket, in request order.
///
/// The details fetch, the model, the budget and the dependency basis are the
/// same whether one pool is filled or `--all` fans out by label, so both paths
/// build their requests here and cannot drift apart.
fn ready_ticket_requests<Q: TicketQuery>(
    repository: &Repository,
    args: &DispatchArgs,
    discovery: &TicketDiscovery<Q>,
    ready: &ReadyTickets,
    ticket_hint: &str,
    model: &Option<AgentModel>,
) -> Result<Vec<DirectDispatchRequest>> {
    ready
        .tickets()
        .iter()
        .map(|ticket| {
            let mut request = DirectDispatchRequest::new(ticket.clone(), args.mode);
            if let Some(details) = fetch_ticket_details(discovery, ticket.id(), ticket_hint)? {
                request = request.with_ticket_details(details);
            }
            if let Some(model) = model.clone() {
                request = request.with_model(model);
            }
            if let Some(dollars) = args.budget {
                request = request.with_budget(DispatchBudget::maximum_usd(dollars)?);
            }
            // Base a dependent on its Blockers' branches, exactly as
            // orchestration does, so a chain lands as a stack rather than as
            // parallel branches off trunk. A Blocker with no usable branch to
            // stack on is stated with the bookmark state that leaves it
            // unusable, rather than left silent, so a dependent that is not fully
            // stacked is never a surprise.
            let basis = dependency_basis(ready.blockers(ticket.id()), |blocker| {
                resolve_ticket_branch(repository.root(), blocker)
            });
            if let Some(context) = basis.context() {
                request = request.with_dependency_context(context.clone());
            }
            let base = basis
                .context()
                .map_or("trunk", DispatchDependencyContext::pull_request_base);
            for blocker in basis.unstacked() {
                eprintln!(
                    "Ticket {} is blocked by {}, whose {}; basing it on {base}",
                    ticket.id(),
                    blocker.ticket(),
                    blocker.reason()
                );
            }
            Ok(request)
        })
        .collect()
}

/// One lane a `--all` Dispatch discovers over: the Worker Pool it fills and the
/// label its Ready Tickets carry.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscoveryLane {
    pool: PoolName,
    label: String,
}

/// The lanes a `--all` Dispatch discovers over.
///
/// Several routable pools with no `--pool` fan out one lane each; every other
/// shape resolves the single pool a Dispatch would fill, which is the loud
/// refusal when a Repository holds several and none is named. `dispatch` and
/// `ticket list` both read their lanes here, so the list and the Dispatch cannot
/// disagree about which lanes exist or which label discovers each one.
fn discovery_lanes(
    available: &[PoolName],
    routes: &[PoolRoute],
    requested_pool: Option<&str>,
    requested_label: Option<&str>,
    all: bool,
) -> Result<Vec<DiscoveryLane>> {
    if all && requested_pool.is_none() && available.len() > 1 && !routes.is_empty() {
        return Ok(routes
            .iter()
            .map(|route| DiscoveryLane {
                pool: route.name().clone(),
                label: route.label().to_owned(),
            })
            .collect());
    }
    let pool = resolve_pool(available, requested_pool)?;
    let label = ready_label_for(requested_label, &pool, routes);
    Ok(vec![DiscoveryLane { pool, label }])
}

/// The Ready Ticket filter one lane discovers by.
///
/// The label is the lane's; the status is the pool's own `ready_status`, falling
/// back to the user-global value. Both `dispatch --all` and `ticket list` build
/// their filter here, so a Ticket cannot be ready to one and blocked to the
/// other.
fn lane_filter(
    lane: &DiscoveryLane,
    repo_config: &RepoConfig,
    tickets: &TicketsConfig,
) -> Result<ReadyTicketFilter> {
    let status =
        TicketStatus::parse(repo_config.pool_ready_status(&lane.pool, tickets.ready_status()))?;
    Ok(ReadyTicketFilter::new(&lane.label, status)?)
}

/// Fans a `--all` Dispatch out across the pools that declare a routing label.
///
/// Each routable pool discovers its own ready work by its own label and the
/// Tickets it returns are dispatched to it, so work lands in the right lane by
/// itself instead of a human routing every Dispatch by hand.
///
/// A Ticket that matches no pool is **reported, not dropped**: the diagnostics
/// every pool's discovery produced are folded together, the Tickets that did
/// route are removed, and what is left is printed. Silence is the failure mode
/// this whole path exists to remove, so an unroutable Ticket is never
/// indistinguishable from one that was never filed.
fn dispatch_all_across_pools(
    repository: &Repository,
    args: &DispatchArgs,
    lanes: &[DiscoveryLane],
    tickets_config: fn(&Repository) -> Result<TicketsConfig>,
    user_config: &Config,
    repo_config: &RepoConfig,
    open_workspaces: fn() -> Result<std::collections::BTreeSet<String>>,
) -> Result<()> {
    let model = selected_model(args.provider.as_deref(), args.model.as_deref())?;
    let tickets = tickets_config(repository)?;
    let ticket_hint = ticket_source_hint(&tickets, repository);
    // The source is built once, with the global ready label: a Ticket that
    // declares no `Labels:` carries that, never a pool's routing label, so an
    // untagged Ticket cannot match every lane at once.
    let runtime = configured_runtime(repository);
    let discovery = TicketDiscovery::new(configured_ticket_query(
        repository.root(),
        &tickets,
        runtime,
        &global_ready_label(args.label.as_deref()),
    )?);

    let mut entries = Vec::new();
    let mut routed = BTreeSet::new();
    let mut reported: Vec<(String, String)> = Vec::new();

    for lane in lanes {
        let scoped = repository.clone().with_pool(lane.pool.clone());
        // Reconcile this pool's finished Runs before discovering its ready work,
        // so a Ticket whose Run completed since the last Dispatch resolves and
        // readies its dependent in this same fan-out.
        write_back_local_tickets(&scoped, &tickets);
        let spend_decision = spend_gate(&scoped, repo_config, user_config)?;
        if let SpendDecision::Warned(message) = &spend_decision {
            eprintln!("{message}");
        }
        eprintln!(
            "Fetching tickets with label '{}' for pool '{}'...",
            lane.label, lane.pool
        );
        // The status a Ticket must be in to be ready is per pool, exactly as its
        // label routes it: a lane that declares its own `ready_status` draws work
        // by that status, and one that declares none keeps the user-global value.
        let filter = lane_filter(lane, repo_config, &tickets)?;
        let ready = discovery.ready_tickets(&filter)?;
        // Fold by subject: every pool reports the same not-ready Ticket, and one
        // reason is enough to say what stops it.
        for diagnostic in ready.diagnostics() {
            if !reported
                .iter()
                .any(|(subject, _)| subject == diagnostic.subject())
            {
                reported.push((
                    diagnostic.subject().to_owned(),
                    diagnostic.reason().to_owned(),
                ));
            }
        }
        if ready.tickets().is_empty() {
            continue;
        }
        // Materialise a declared lane only once it has work to take, so a fan-out
        // never provisions a lane that matched nothing.
        materialise_declared_pool(&scoped, user_config)?;
        let requests =
            ready_ticket_requests(&scoped, args, &discovery, &ready, &ticket_hint, &model)?;
        // A pool whose cap has run out holds its Tickets back rather than firing
        // any of them, so the fan-out reports one lane held instead of aborting
        // the others. Holding reserves no Worker, so it needs no exclusion query.
        let result = match &spend_decision {
            SpendDecision::Refused(message) => scoped.direct_dispatch().hold(&requests, message),
            _ => {
                let exclusions = dispatch_exclusions(&scoped, args.force, open_workspaces)?;
                dispatch_with_capacity_prompt(&scoped, &requests, &exclusions)?
            }
        };
        entries.extend(result.outcomes().iter().map(DispatchEntry::from_outcome));
        routed.extend(
            ready
                .tickets()
                .iter()
                .map(|ticket| ticket.id().as_str().to_owned()),
        );
        write_back_local_tickets(&scoped, &tickets);
    }

    // A Ticket that matched no pool's label is reported with the reason, never
    // silently skipped. A Ticket that did route is not reported, even though
    // every pool it did not match named it.
    let unrouted = reported
        .into_iter()
        .filter(|(subject, _)| !routed.contains(subject))
        .collect::<Vec<_>>();

    if entries.is_empty() {
        let skipped = unrouted
            .iter()
            .map(|(ticket, detail)| DispatchEntry::Skipped {
                ticket: ticket.clone(),
                detail: detail.clone(),
            })
            .collect::<Vec<_>>();
        if args.json {
            // Name every search the fan-out ran - one label per routable pool -
            // ahead of the Tickets that were found but routed nowhere, so an
            // empty batch says which labels it looked for and under which root.
            let mut batch = lanes
                .iter()
                .map(|lane| search_entry(&lane.label, &tickets, repository))
                .collect::<Vec<_>>();
            batch.extend(skipped);
            render_dispatch_entries(&batch, true);
        } else if skipped.is_empty() {
            eprintln!("No tickets matched any Worker Pool's routing label");
        } else {
            eprintln!("No routable tickets:");
            eprint!("{}", dispatch_prose(&skipped));
        }
        return Ok(());
    }

    for (subject, reason) in &unrouted {
        eprintln!("{subject}: {reason}");
    }
    render_dispatch_entries(&entries, args.json);
    if !args.json {
        let dispatched = entries.iter().filter(|entry| entry.dispatched()).count();
        eprintln!("Dispatched {dispatched} ticket(s)");
        let failed = entries.iter().filter(|entry| entry.failed()).count();
        if failed > 0 {
            eprintln!("{failed} ticket(s) failed to dispatch.");
        }
        let held = entries
            .iter()
            .filter(|entry| matches!(entry, DispatchEntry::Waiting { .. }))
            .count();
        if held > 0 {
            // The reason is the binding limit a pool's capacity governor or
            // spend gate named, so a held lane reads as deliberately held rather
            // than as a silent drop.
            let reason = entries
                .iter()
                .find_map(|entry| match entry {
                    DispatchEntry::Waiting { detail, .. } => Some(detail.as_str()),
                    _ => None,
                })
                .unwrap_or("held back");
            eprintln!("Held {held} ticket(s) back: {reason}");
        }
    }
    ensure_dispatch_progressed(&entries)?;
    Ok(())
}

/// `resume`: re-dispatch the work the durable queue records as unfinished.
///
/// Resumption is explicit - an operator asks for it - because re-running a step
/// may have side effects an automatic retry must not take on. It reads the
/// queue's own per-step state rather than a second notion of progress: a step
/// that reached `done` completed and is left alone, and a step whose Run
/// reconciliation has just called live is left running. Everything else - work
/// accepted and waiting, claimed without a Run, failed or abandoned - is
/// re-opened and dispatched again on the same seam a Dispatch uses, so the
/// capacity governor, the spend limits and the occupancy exclusions all apply.
///
/// Reconciliation runs first, so a Run that ended is settled and a Run that is
/// still live is recognised: the resumption reads the queue's verdict rather
/// than a stale in-flight record, and never races a live Run.
fn resume_command(
    repository: &Repository,
    args: &ResumeArgs,
    open_workspaces: fn() -> Result<BTreeSet<String>>,
    tickets_config: fn(&Repository) -> Result<TicketsConfig>,
) -> Result<()> {
    let config = crate::config::load()?;
    let repo_config = config.repo_config(repository.root())?;
    let available = config.pool_names_for(repository.root())?;
    let pool = resolve_pool(&available, args.pool.as_deref())?;
    let pool_name = pool.as_str().to_owned();
    let scoped = repository.clone().with_pool(pool);
    let repository = &scoped;

    // The spend limits bind a resumption exactly as they bind a Dispatch: work
    // jjfx accepted earlier must not spend past the ceiling now. They are
    // evaluated before anything is created, so a refused resumption creates
    // nothing.
    match spend_gate(repository, &repo_config, &config)? {
        SpendDecision::Allowed => {}
        SpendDecision::Warned(message) => eprintln!("{message}"),
        SpendDecision::Refused(message) => bail!("{message}"),
    }
    materialise_declared_pool(repository, &config)?;

    let tickets = tickets_config(repository)?;
    let (snapshot, failures) = reconcile_pass(repository, &tickets);
    report_writeback_failures(&failures);

    // A resumed Run gets the same prompt a Dispatch would: the Ticket's details
    // are inlined when the source can still resolve it. A resumption must not
    // depend on that - the queue recorded the step, so the work is re-run even
    // when the source cannot answer, with the Ticket id alone in the prompt.
    let discovery = TicketDiscovery::new(configured_ticket_query(
        repository.root(),
        &tickets,
        configured_runtime(repository),
        &global_ready_label(None),
    )?);
    let ticket_hint = ticket_source_hint(&tickets, repository);

    let queue = repository.work_queue();
    let mut requests = Vec::new();
    let mut resumed_tickets = Vec::new();
    let mut skipped = Vec::new();
    for item in snapshot.queue() {
        if !args.tickets.is_empty() && !args.tickets.iter().any(|ticket| ticket == &item.subject) {
            continue;
        }
        if !item.state.is_resumable() {
            skipped.push(DispatchEntry::Skipped {
                ticket: item.subject.clone(),
                detail: match item.state {
                    QueueItemState::InFlight => format!(
                        "still in flight on {}; reset the Worker to abandon it, then resume",
                        item.claimed_by
                            .as_ref()
                            .map_or("an unrecorded Worker", WorkerId::as_str)
                    ),
                    state => format!("already {state}"),
                },
            });
            continue;
        }
        // The queue re-opens the record, so the new Run's claim and settlement
        // apply to this step. A step another process re-opened or settled first
        // is left to it rather than dispatched twice - reported rather than
        // dropped, because a resumption that quietly ignored a step would look
        // like one that resumed it.
        if !queue.resume(&item.step)? {
            skipped.push(DispatchEntry::Skipped {
                ticket: item.subject.clone(),
                detail: "was resumed or settled by another process first".to_owned(),
            });
            continue;
        }
        let id = TicketId::parse(item.subject.as_str())?;
        let mut request = DirectDispatchRequest::for_ticket_id(id.clone(), args.mode)?;
        match fetch_ticket_details(&discovery, &id, &ticket_hint) {
            Ok(Some(details)) => request = request.with_ticket_details(details),
            Ok(None) => {}
            Err(error) => eprintln!("warning: {error}"),
        }
        resumed_tickets.push(item.subject.clone());
        requests.push(request);
    }
    // A Ticket named by hand with no recorded step is reported rather than
    // dispatched: `dispatch` is the command that runs work jjfx has not accepted.
    for ticket in &args.tickets {
        if !snapshot.queue().iter().any(|item| &item.subject == ticket) {
            skipped.push(DispatchEntry::Skipped {
                ticket: ticket.clone(),
                detail: "no step is recorded for it; use `dispatch` to run new work".to_owned(),
            });
        }
    }

    let mut entries = Vec::new();
    if !requests.is_empty() {
        let exclusions = dispatch_exclusions(repository, args.force, open_workspaces)?;
        let result = dispatch_with_capacity_prompt(repository, &requests, &exclusions)?;
        entries.extend(result.outcomes().iter().map(DispatchEntry::from_outcome));
    }
    // The resumed entries come first, so the slice is the batch this command
    // actually attempted: a resumption whose every step failed is an error, while
    // one that skipped everything already done is not.
    let resumed = entries.len();
    entries.extend(skipped);
    if args.json {
        render_dispatch_entries(&entries, true);
        return ensure_dispatch_progressed(&entries[..resumed]);
    }
    if entries.is_empty() {
        eprintln!("No unfinished steps recorded in pool '{pool_name}'");
    } else {
        render_dispatch_entries(&entries, false);
        // A resumed Run reports its Worker rather than its Ticket in the
        // Dispatch prose, so the summary names every Ticket this command
        // re-opened - the answer an operator runs `resume` to get.
        if resumed_tickets.is_empty() {
            eprintln!("Resumed 0 step(s) in pool '{pool_name}'");
        } else {
            eprintln!(
                "Resumed {} step(s) in pool '{pool_name}': {}",
                resumed_tickets.len(),
                resumed_tickets.join(", ")
            );
        }
    }
    ensure_dispatch_progressed(&entries[..resumed])
}

fn dispatch_command(
    repository: &Repository,
    args: &DispatchArgs,
    open_workspaces: fn() -> Result<std::collections::BTreeSet<String>>,
    tickets_config: fn(&Repository) -> Result<TicketsConfig>,
) -> Result<()> {
    // The Repository's own settings are read once, before anything is gated,
    // reserved or launched: they name the pools this Dispatch may fill, carry the
    // routing labels `--all` fans out over, and hold the declarations - a pool's
    // spend cap among them - the gate reads.
    let config = crate::config::load()?;
    let repo_config = config.repo_config(repository.root())?;
    let available = config.pool_names_for(repository.root())?;
    let routes = config.pool_routes_for(repository.root())?;

    // The lanes this Dispatch may discover over: `--all` with no `--pool` fans
    // out across the pools that declare a routing label, sending each Ticket to
    // the pool whose label it carries. The convenience is opt-in: a Repository
    // whose pools declare no labels keeps ticket 06's loud refusal, so an
    // unqualified Dispatch is never a silent choice, and an explicit `--pool`
    // always means "only this lane". `ticket list` reads its lanes from the same
    // helper, so the two cannot disagree about which lanes exist.
    let lanes = discovery_lanes(
        &available,
        &routes,
        args.pool.as_deref(),
        args.label.as_deref(),
        args.all,
    )?;
    if lanes.len() > 1 {
        return dispatch_all_across_pools(
            repository,
            args,
            &lanes,
            tickets_config,
            &config,
            &repo_config,
            open_workspaces,
        );
    }

    // Resolve the pool this Dispatch fills before anything is gated, reserved or
    // launched. One pool implies it - every Repository that declares none has one
    // implicit `default` - while several require `--pool`, so an unqualified
    // Dispatch into a Repository with several pools refuses loudly, naming them,
    // rather than silently filling one. `--text` and `--from-file` follow the
    // same rule because they pass through here too.
    let lane = lanes
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("this Repository holds no Worker Pools"))?;
    // The ready label is per pool: `--label` overrides the pool's declared
    // routing label, which overrides the global default. The source's fallback
    // label is the global one, never the pool's: a Ticket that declares no
    // `Labels:` carries that, so it cannot match every lane at once.
    let ready_label = lane.label.clone();
    let source_label = global_ready_label(args.label.as_deref());
    // Every pool-scoped read and write below - the pool document, its Workers,
    // its Run Records, its locks, its spend - follows the handle, so Worker
    // reservation, Dispatch Eligibility and the pool's size are all the selected
    // pool's (ADR 0012).
    let scoped = repository.clone().with_pool(lane.pool.clone());
    let repository = &scoped;

    let model = selected_model(args.provider.as_deref(), args.model.as_deref())?;
    // Every spend limit is evaluated before anything is reserved or launched,
    // including the orchestration handoff below, so a blocked limit refuses the
    // whole Dispatch. A limit in warn mode reports and proceeds. A blocked limit
    // holds a discovered (`--all`) batch back visibly below; an explicit Dispatch
    // refuses loudly here, because the owner named the Ticket rather than asking
    // for whatever fits.
    let spend_decision = spend_gate(repository, &repo_config, &config)?;
    match &spend_decision {
        SpendDecision::Allowed => {}
        SpendDecision::Warned(message) => eprintln!("{message}"),
        SpendDecision::Refused(message) if !args.all => bail!("{message}"),
        SpendDecision::Refused(_) => {}
    }

    // Materialise a declared lane's state before anything reserves or launches in
    // it, so `dispatch --pool <name>` into a freshly declared Pool works with no
    // hand-provisioning. The spend gate runs first, so a refused Dispatch creates
    // nothing.
    materialise_declared_pool(repository, &config)?;

    // A Ticket handed in directly is an input to Direct Dispatch, not a source
    // lookup: it is dispatched here, before any `[tickets]` config is read, so
    // an ad-hoc Dispatch works in a repository that has never been set up.
    if let Some(input) = &args.input {
        let ticket = AdHocTicket::read(input)?;
        return dispatch_adhoc_ticket(repository, args, &ticket, model, open_workspaces);
    }

    // A Ticket named by hand is dispatched even when it is not ready - naming it
    // is the override - but why it is not ready is reported first, from the same
    // seam the `--all` path uses. This runs before the Orchestrator handoff below,
    // so a named Ticket that falls back to a direct Dispatch reports too.
    if !args.all {
        report_named_ticket_readiness(repository, args, &repo_config, tickets_config)?;
    }

    // A single Ticket with orchestration enabled is handed to the Orchestrator.
    // Its fallible front half runs synchronously first: an unresolvable Ticket,
    // a missing Pool, or a rejected discovery fails here and exits non-zero,
    // rather than being swallowed by a background child that dies at once behind
    // a printed `orchestrating`.
    if !args.all && !args.no_orchestrate && args.tickets.len() == 1 {
        if args.mode == RunMode::Foreground {
            return orchestrate_command(
                repository,
                &args.tickets[0],
                model.clone(),
                args.force,
                args.json,
                open_workspaces,
                tickets_config,
            );
        }
        let handoff = prepare_orchestration(
            repository,
            &args.tickets[0],
            model.clone(),
            args.force,
            open_workspaces,
            tickets_config,
        )?;
        match handoff.start {
            crate::engine::OrchestrationStart::Waiting { ticket, detail } => {
                render_dispatch_entries(
                    &[DispatchEntry::Waiting {
                        ticket: ticket.as_str().to_owned(),
                        detail,
                    }],
                    args.json,
                );
                return Ok(());
            }
            crate::engine::OrchestrationStart::Direct(success) => {
                render_dispatch_entries(&[DispatchEntry::from_success(&success)], args.json);
                return Ok(());
            }
            crate::engine::OrchestrationStart::Group => {}
        }
        let log = spawn_background_orchestration(repository, &handoff.parent, args)?;
        render_dispatch_entries(
            &[DispatchEntry::Orchestrating {
                ticket: handoff.parent.as_str().to_owned(),
                detail: format!("log: {}", log.display()),
            }],
            args.json,
        );
        return Ok(());
    }

    // A refused `--all` batch is held back rather than fired, and holding
    // reserves no Worker, so it needs no exclusion query: asking the terminal
    // anyway would let an unreachable terminal mask the spend hold it is about
    // to report.
    let exclusions = if matches!(&spend_decision, SpendDecision::Refused(_)) {
        DispatchExclusions::default()
    } else {
        dispatch_exclusions(repository, args.force, open_workspaces)?
    };
    let tickets = tickets_config(repository)?;
    let ticket_hint = ticket_source_hint(&tickets, repository);
    // Reconcile finished Tickets before discovering ready work, so a Ticket whose
    // Run completed since the last Dispatch resolves and its dependents become
    // ready in this same Dispatch. This is also the only reconciliation an empty
    // ready set reaches: without it, a linear chain deadlocks at `claimed`.
    write_back_local_tickets(repository, &tickets);
    let runtime = configured_runtime(repository);
    let discovery = TicketDiscovery::new(configured_ticket_query(
        repository.root(),
        &tickets,
        runtime,
        &source_label,
    )?);

    if args.all {
        // The ready status is per pool, exactly as the ready label is: a lane
        // that declares its own `ready_status` draws work by that status, and one
        // that declares none keeps the user-global value. The filter comes from
        // the same helper `ticket list` reads, so the two cannot disagree about
        // whether a Ticket is ready.
        let filter = lane_filter(&lane, &repo_config, &tickets)?;
        eprintln!("Fetching tickets with label '{ready_label}'...");
        let ready = discovery.ready_tickets(&filter)?;
        if ready.tickets().is_empty() {
            // An empty ready set must explain itself: name the search (the
            // label and root) and every Ticket that was discovered but not
            // ready (blocked, wrong status, or malformed), so a caller can tell
            // "nothing to do" from "nothing can be done".
            let skipped = ready
                .diagnostics()
                .iter()
                .map(|diagnostic| DispatchEntry::Skipped {
                    ticket: diagnostic.subject().to_owned(),
                    detail: diagnostic.reason().to_owned(),
                })
                .collect::<Vec<_>>();
            if args.json {
                let mut batch = vec![search_entry(&ready_label, &tickets, repository)];
                batch.extend(skipped);
                render_dispatch_entries(&batch, true);
            } else if skipped.is_empty() {
                eprintln!("No tickets found with label '{ready_label}'");
            } else {
                eprintln!("No ready tickets with label '{ready_label}':");
                eprint!("{}", dispatch_prose(&skipped));
            }
            return Ok(());
        }
        for diagnostic in ready.diagnostics() {
            eprintln!("{}: {}", diagnostic.subject(), diagnostic.reason());
        }
        let requests =
            ready_ticket_requests(repository, args, &discovery, &ready, &ticket_hint, &model)?;
        // A blocked spend limit holds the whole discovered batch back rather
        // than firing any of it: nothing is attempted and nothing is reserved,
        // and every Ticket stays visible in `waiting` with the binding limit as
        // its reason. That is the difference from an explicit Dispatch above,
        // which refuses loudly because the owner named the Ticket.
        let result = match &spend_decision {
            SpendDecision::Refused(message) => {
                repository.direct_dispatch().hold(&requests, message)
            }
            _ => dispatch_with_capacity_prompt(repository, &requests, &exclusions)?,
        };
        let entries = render_dispatch_result(&result, requests.len(), true, args.json);
        write_back_local_tickets(repository, &tickets);
        ensure_dispatch_progressed(&entries)?;
        return Ok(());
    }

    let requests = args
        .tickets
        .iter()
        .map(|ticket| {
            let id = TicketId::parse(ticket.clone())?;
            let mut request = DirectDispatchRequest::for_ticket_id(id.clone(), args.mode)?;
            if let Some(details) = fetch_ticket_details(&discovery, &id, &ticket_hint)? {
                request = request.with_ticket_details(details);
            }
            if let Some(model) = model.clone() {
                request = request.with_model(model);
            }
            if let Some(dollars) = args.budget {
                request = request.with_budget(DispatchBudget::maximum_usd(dollars)?);
            }
            Ok(request)
        })
        .collect::<Result<Vec<_>>>()?;
    let result = dispatch_with_capacity_prompt(repository, &requests, &exclusions)?;
    let entries = render_dispatch_result(&result, requests.len(), false, args.json);
    write_back_local_tickets(repository, &tickets);
    ensure_dispatch_progressed(&entries)?;
    Ok(())
}

/// Dispatches a Ticket that was handed in directly.
///
/// It goes down the same Direct Dispatch path as a resolved Ticket: the ad-hoc
/// Ticket carries its own id, title, and body, so nothing is fetched and no
/// source is consulted. There is no Ticket file to write back, and an ad-hoc
/// Ticket has no Parent to orchestrate, so it is always a single direct request.
fn dispatch_adhoc_ticket(
    repository: &Repository,
    args: &DispatchArgs,
    ticket: &AdHocTicket,
    model: Option<AgentModel>,
    open_workspaces: fn() -> Result<std::collections::BTreeSet<String>>,
) -> Result<()> {
    let exclusions = dispatch_exclusions(repository, args.force, open_workspaces)?;
    let mut request = DirectDispatchRequest::new(ticket.ticket(), args.mode)
        .with_ticket_details(ticket.details());
    if let Some(model) = model {
        request = request.with_model(model);
    }
    if let Some(dollars) = args.budget {
        request = request.with_budget(DispatchBudget::maximum_usd(dollars)?);
    }
    let requests = [request];
    let result = dispatch_with_capacity_prompt(repository, &requests, &exclusions)?;
    let entries = render_dispatch_result(&result, requests.len(), false, args.json);
    ensure_dispatch_progressed(&entries)?;
    Ok(())
}

/// The Workers excluded from Dispatch because their Workspace is open in a
/// terminal, or an error when the terminal cannot be queried.
///
/// Fails closed: if jjfx cannot see which Workspaces are open it refuses to
/// dispatch rather than risk colliding with a human. `--force` skips the query
/// and dispatches with no exclusions.
fn dispatch_exclusions(
    repository: &Repository,
    force: bool,
    open_workspaces: fn() -> Result<std::collections::BTreeSet<String>>,
) -> Result<DispatchExclusions> {
    if force {
        return Ok(DispatchExclusions::default());
    }
    let open = open_workspaces().context(
        "cannot determine which Worker Workspaces are open; re-run with --force to dispatch anyway",
    )?;
    Ok(crate::engine::reconcile_occupancy(repository, &open))
}

/// Evaluates every spend limit that applies to one Dispatch.
///
/// The selected pool's own cap totals that pool's records; the Repository
/// ceiling totals every pool's. Both bind, so Dispatch proceeds only when each
/// limit has room, and the most restrictive fired limit is the one named - the
/// pool when its cap is the tighter constraint, the Repository when the ceiling
/// is, because the ceiling is the dial that unblocks every pool. `--budget` is a
/// cap inside both: it stays a per-Dispatch limit and composes by this same rule,
/// never refusing on its own because it is the allowance for the Run it bounds
/// rather than an accumulated total. A limit that cannot be evaluated because
/// the record store is unreadable is an error: failing closed is the only safe
/// answer for a spend gate.
fn spend_gate(
    repository: &Repository,
    repo_config: &RepoConfig,
    user_config: &Config,
) -> Result<SpendDecision> {
    let mut limits = Vec::new();
    if let Some(cap) = user_config.spend.cap()? {
        let names = repo_config.pool_names()?;
        let spend = repository.repository_cost(&names)?;
        limits.push(SpendLimit::new(SpendScope::Repository, cap, spend));
    }
    let pool = repository.selected_pool();
    if let Some(declaration) = repo_config.pools.get(pool.as_str())
        && let Some(cap) = declaration.spend_cap(&user_config.spend)?
    {
        let spend = repository.pool_cost()?;
        limits.push(SpendLimit::new(SpendScope::Pool(pool.clone()), cap, spend));
    }
    Ok(crate::engine::evaluate_spend(limits))
}

/// A live occupancy source for the orchestration runner.
///
/// Unlike [`dispatch_exclusions`], which snapshots the open Workspaces once, the
/// provider re-queries on every claim so mid-run tab changes are honoured
/// (ADR-0011). `--force` dispatches regardless of occupancy, so it has nothing
/// to re-query.
fn exclusions_provider(
    repository: &Repository,
    force: bool,
    open_workspaces: fn() -> Result<std::collections::BTreeSet<String>>,
) -> crate::engine::ExclusionsProvider {
    if force {
        return crate::engine::ExclusionsProvider::fixed(DispatchExclusions::default());
    }
    let repository = repository.clone();
    crate::engine::ExclusionsProvider::new(move || {
        let open = open_workspaces().map_err(|error| {
            format!(
                "cannot determine which Worker Workspaces are open ({error}); \
                 re-run with --force to dispatch anyway"
            )
        })?;
        Ok(crate::engine::reconcile_occupancy(&repository, &open))
    })
}

/// Fetch a Ticket's details to inline into the Dispatch prompt, failing before
/// any Pool or Workspace mutation when the runtime cannot reach the tracker.
///
/// `Ok(None)` is not a failure: the source has no Ticket with that id, which is
/// exactly the local feature Parent - a directory rather than a Ticket file - so
/// orchestration proceeds to its children.
fn fetch_ticket_details<Q: TicketQuery>(
    discovery: &TicketDiscovery<Q>,
    id: &TicketId,
    hint: &str,
) -> Result<Option<TicketDetails>> {
    discovery.ticket_details(id).with_context(|| {
        format!("cannot fetch Ticket {id} to inline into the Dispatch prompt; {hint}")
    })
}

/// The recovery hint for a failed Ticket-details fetch, keyed off the configured
/// source.
///
/// A local source names the root it searched, so the message points at the
/// subsystem that actually failed rather than at Linear - which a repository
/// configured `source = "local"` never consults.
fn ticket_source_hint(tickets: &TicketsConfig, repository: &Repository) -> String {
    match tickets.source {
        TicketSource::Linear => "ensure the pool runtime can reach Linear".to_owned(),
        TicketSource::Local => format!(
            "ensure the local Ticket source under {} can resolve it",
            repository.root().join(&tickets.local.root).display()
        ),
    }
}

fn dispatch_with_capacity_prompt(
    repository: &Repository,
    requests: &[DirectDispatchRequest],
    exclusions: &DispatchExclusions,
) -> Result<crate::engine::DirectDispatchResult> {
    // The concurrency cap is jjfx's own setting, read here rather than passed
    // down: `cli::run` is already at the argument limit, and this is the only
    // place a whole batch is dispatched. Resolved for the Repository so its own
    // `[dispatch]` ceiling tightens the user-global one.
    let cap = crate::config::load()?.dispatch_cap_for(repository.root())?;
    let dispatcher = repository
        .direct_dispatch()
        .with_exclusions(exclusions.clone())
        .with_concurrency_cap(cap);
    match dispatcher.dispatch(requests) {
        Ok(result) => Ok(result),
        Err(DirectDispatchError::WorkerPool(WorkerPoolError::CapacityShortage(shortage))) => {
            let approved = confirm_growth(shortage);
            if approved {
                Ok(dispatcher.dispatch_with_approved_growth(requests, shortage.gap())?)
            } else {
                Ok(dispatcher.dispatch_use_available(requests)?)
            }
        }
        Err(error) => Err(error.into()),
    }
}

/// The capacity question, without the answer hint: `Grow pool by 3?`.
fn growth_question(shortage: CapacityShortage) -> String {
    format!(
        "Pool has {} idle worker(s) but {} ticket(s) to dispatch. Grow pool by {}?",
        shortage.available(),
        shortage.requested(),
        shortage.gap()
    )
}

/// Whether a typed answer approves growing the pool.
///
/// Only an explicit `y`/`yes` does. An empty answer, an EOF, or anything else
/// refuses, so the prompt's default is the cheap option.
fn answer_approves_growth(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Ask whether to grow the pool to fit a whole batch.
///
/// A prompt that cannot be answered must not default to the expensive option:
/// a non-interactive caller - no terminal, or stdin at EOF - refuses, so
/// `jjfx dispatch` in a script can never grow the pool by not answering.
fn confirm_growth(shortage: CapacityShortage) -> bool {
    let question = growth_question(shortage);
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "{question} Not a terminal, so not growing the pool; run `{PROGRAM} pool <N>` to grow it."
        );
        return false;
    }
    eprint!("{question} [y/N] ");
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    answer_approves_growth(&answer)
}

/// Renders one Dispatch batch and returns the shared model the caller inspects
/// to decide the exit code.
fn render_dispatch_result(
    result: &crate::engine::DirectDispatchResult,
    requested: usize,
    all: bool,
    json: bool,
) -> Vec<DispatchEntry> {
    let entries: Vec<DispatchEntry> = result
        .outcomes()
        .iter()
        .map(DispatchEntry::from_outcome)
        .collect();
    render_dispatch_entries(&entries, json);
    if json {
        return entries;
    }
    let dispatched = entries.iter().filter(|entry| entry.dispatched()).count();
    let failed = entries.iter().filter(|entry| entry.failed()).count();
    if result.is_partial() {
        if dispatched == 0 && !all {
            eprintln!("No idle workers. Run: {PROGRAM} pool list");
        } else {
            eprintln!("No more idle workers. Dispatched {dispatched}/{requested} ticket(s).");
        }
    } else if all {
        eprintln!("Dispatched {dispatched} ticket(s)");
    }
    if failed > 0 {
        eprintln!("{failed} ticket(s) failed to dispatch.");
    }
    let held = result.waiting().len();
    if held > 0 {
        // The reason is the binding limit the capacity governor or the spend gate
        // named, so a spend hold reads as deliberately held rather than as a
        // silent drop, and the message never guesses "capacity" for a cap that is
        // about spend.
        let reason = entries
            .iter()
            .find_map(|entry| match entry {
                DispatchEntry::Waiting { detail, .. } => Some(detail.as_str()),
                _ => None,
            })
            .unwrap_or("held back");
        eprintln!("Held {held} ticket(s) back: {reason}");
    }
    entries
}

/// Turns an all-failed Dispatch batch into an error, so a scripting caller can
/// tell "nothing happened" from "it worked".
///
/// A batch where every Ticket failed exits non-zero. A batch with at least one
/// dispatched Ticket, or one deliberately held back under a cap, is still a
/// success: some of the work was done or queued. An empty batch is left alone -
/// it is "nothing was requested", not "everything failed".
fn ensure_dispatch_progressed(entries: &[DispatchEntry]) -> Result<()> {
    if entries.is_empty() || entries.iter().any(DispatchEntry::progressed) {
        return Ok(());
    }
    let tickets = entries
        .iter()
        .filter_map(DispatchEntry::ticket)
        .collect::<Vec<_>>()
        .join(", ");
    bail!("every Ticket failed to dispatch: {tickets}")
}

/// The result for one Ticket in a Dispatch, the shared model both the prose and
/// the JSON renderings are built from.
///
/// A resumption reuses it: a resumed step's outcome is a Direct Dispatch
/// outcome, and the steps a resumption left alone are `Skipped` with the reason,
/// so `resume` reports through the same shape `dispatch` does.
///
/// [`DispatchEntry::from_outcome`] is the only place that matches
/// [`DirectDispatchOutcome`], so a new engine outcome breaks compilation here
/// rather than reaching one rendering and being forgotten in the other.
#[derive(Debug, PartialEq, Eq)]
enum DispatchEntry {
    /// The Ticket was launched on its selected Worker.
    Dispatched {
        ticket: String,
        worker: String,
        /// The background process identifier, absent for a foreground Run.
        pid: Option<u32>,
        /// The human detail the prose prints after the Worker.
        detail: String,
    },
    /// The Ticket failed before a usable Run was launched.
    Failed {
        ticket: String,
        worker: Option<String>,
        phase: DirectDispatchFailurePhase,
        detail: String,
    },
    /// The capacity governor held the Ticket back; nothing was attempted.
    Waiting { ticket: String, detail: String },
    /// The Ticket was discovered but is not ready to run; the detail says why.
    Skipped { ticket: String, detail: String },
    /// The search found nothing at all: the label it looked for and the root it
    /// looked under. Emitted in a `--json` batch in place of a bare `[]`, so a
    /// caller can tell a wrong root or a label that matched nothing from a
    /// Repository that simply has no work.
    Search {
        label: String,
        root: Option<String>,
        detail: String,
    },
    /// The Ticket was handed to the Orchestrator rather than a single Worker.
    Orchestrating { ticket: String, detail: String },
}

impl DispatchEntry {
    /// Builds the shared entry for one Direct Dispatch outcome.
    fn from_outcome(outcome: &DirectDispatchOutcome) -> Self {
        match outcome {
            DirectDispatchOutcome::Succeeded(success) => Self::from_success(success),
            DirectDispatchOutcome::Failed(failure) => Self::Failed {
                ticket: failure.ticket().id().as_str().to_owned(),
                worker: failure.worker().map(|worker| worker.as_str().to_owned()),
                phase: failure.phase(),
                detail: failure.detail().to_owned(),
            },
            DirectDispatchOutcome::Waiting(waiting) => Self::Waiting {
                ticket: waiting.ticket().id().as_str().to_owned(),
                detail: waiting.detail().to_owned(),
            },
        }
    }

    /// Builds the shared entry for one successful Direct Dispatch.
    fn from_success(success: &DirectDispatchSuccess) -> Self {
        let (pid, detail) = match success.execution() {
            DirectDispatchExecution::Background { pid } => (Some(*pid), format!("PID {pid}")),
            DirectDispatchExecution::Session { session_id } => {
                (None, format!("session {session_id}"))
            }
            DirectDispatchExecution::Foreground(completed) => (
                None,
                format!("completed {:?}", completed.result().conclusion()),
            ),
        };
        Self::Dispatched {
            ticket: success.ticket().id().as_str().to_owned(),
            worker: success.worker().as_str().to_owned(),
            pid,
            detail,
        }
    }

    /// Returns the Ticket identifier, or `None` for a search entry, which names
    /// no Ticket because it describes the discovery itself.
    fn ticket(&self) -> Option<&str> {
        match self {
            Self::Dispatched { ticket, .. }
            | Self::Failed { ticket, .. }
            | Self::Waiting { ticket, .. }
            | Self::Skipped { ticket, .. }
            | Self::Orchestrating { ticket, .. } => Some(ticket),
            Self::Search { .. } => None,
        }
    }

    /// Returns the stable machine-readable outcome name.
    const fn outcome(&self) -> &'static str {
        match self {
            Self::Dispatched { .. } => "dispatched",
            Self::Failed { .. } => "failed",
            Self::Waiting { .. } => "waiting",
            Self::Skipped { .. } => "skipped",
            Self::Search { .. } => "skipped",
            Self::Orchestrating { .. } => "orchestrating",
        }
    }

    /// Returns the Worker that took the Ticket, when one did.
    fn worker(&self) -> Option<&str> {
        match self {
            Self::Dispatched { worker, .. } => Some(worker),
            Self::Failed { worker, .. } => worker.as_deref(),
            Self::Waiting { .. }
            | Self::Skipped { .. }
            | Self::Search { .. }
            | Self::Orchestrating { .. } => None,
        }
    }

    /// Returns the stable machine-readable failure phase, when the Ticket failed.
    const fn phase(&self) -> Option<&'static str> {
        match self {
            Self::Failed { phase, .. } => Some(phase.as_str()),
            _ => None,
        }
    }

    /// Returns the background process identifier, when one was launched.
    const fn pid(&self) -> Option<u32> {
        match self {
            Self::Dispatched { pid, .. } => *pid,
            _ => None,
        }
    }

    /// Returns the human detail the prose prints for this outcome.
    fn detail(&self) -> &str {
        match self {
            Self::Dispatched { detail, .. }
            | Self::Failed { detail, .. }
            | Self::Waiting { detail, .. }
            | Self::Skipped { detail, .. }
            | Self::Search { detail, .. }
            | Self::Orchestrating { detail, .. } => detail,
        }
    }

    /// Returns the label a search entry looked for and the root it looked
    /// under, or `None` for a per-Ticket entry.
    fn search(&self) -> Option<(&str, Option<&str>)> {
        match self {
            Self::Search { label, root, .. } => Some((label, root.as_deref())),
            _ => None,
        }
    }

    /// Whether the Ticket was launched on a Worker.
    const fn dispatched(&self) -> bool {
        matches!(self, Self::Dispatched { .. })
    }

    /// Whether the Ticket failed before a usable Run was launched.
    const fn failed(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }

    /// Whether this outcome means the Dispatch did something: launched a Run,
    /// held a Ticket back under a cap, or handed a Parent to the Orchestrator.
    /// Only a failed Ticket did not.
    const fn progressed(&self) -> bool {
        !self.failed()
    }
}

/// Renders one Dispatch batch: JSON on stdout when `json`, else the prose lines
/// on stderr.
fn render_dispatch_entries(entries: &[DispatchEntry], json: bool) {
    if json {
        println!("{}", dispatch_json(entries));
    } else {
        eprint!("{}", dispatch_prose(entries));
    }
}

/// The prose rendering of one Dispatch batch, one line per Ticket.
fn dispatch_prose(entries: &[DispatchEntry]) -> String {
    let mut out = String::new();
    for entry in entries {
        match entry {
            DispatchEntry::Dispatched {
                ticket,
                worker,
                pid: Some(pid),
                ..
            } => out.push_str(&format!("  {worker} (PID {pid}) -> {ticket}\n")),
            DispatchEntry::Dispatched { worker, detail, .. } => {
                out.push_str(&format!("  {worker} {detail}\n"));
            }
            DispatchEntry::Failed {
                ticket,
                phase,
                detail,
                ..
            } => out.push_str(&format!("  {ticket} failed at {phase:?}: {detail}\n")),
            DispatchEntry::Waiting { ticket, detail } => {
                out.push_str(&format!("  {ticket} waiting: {detail}\n"));
            }
            DispatchEntry::Skipped { ticket, detail } => {
                out.push_str(&format!("  {ticket} {detail}\n"));
            }
            DispatchEntry::Search { detail, .. } => {
                out.push_str(&format!("  {detail}\n"));
            }
            DispatchEntry::Orchestrating { ticket, detail } => {
                out.push_str(&format!("Orchestrating {ticket} in background\n"));
                if !detail.is_empty() {
                    out.push_str(&format!("  {detail}\n"));
                }
                out.push_str(&format!(
                    "  Re-run '{PROGRAM} dispatch {ticket}' to check progress\n"
                ));
            }
        }
    }
    out
}

/// The `--json` entry naming one discovery's search: the label it looked for
/// and the root it looked under.
///
/// Emitted when a `--all` discovery finds nothing, so the batch carries the
/// reason it is empty as structure a caller reads rather than a stderr aside it
/// would have to parse. The root is the local markdown root for a local source
/// and absent for Linear, which has no filesystem root to name.
fn search_entry(label: &str, tickets: &TicketsConfig, repository: &Repository) -> DispatchEntry {
    let root = match tickets.source {
        TicketSource::Linear => None,
        TicketSource::Local => Some(
            repository
                .root()
                .join(&tickets.local.root)
                .display()
                .to_string(),
        ),
    };
    DispatchEntry::Search {
        label: label.to_owned(),
        root,
        detail: format!("no ready tickets with label '{label}'"),
    }
}

/// The machine-readable rendering of one Dispatch batch, one object per Ticket
/// in request order.
///
/// Encoded by hand so the emitted field order is fixed.
fn dispatch_json(entries: &[DispatchEntry]) -> String {
    if entries.is_empty() {
        return "[]".to_owned();
    }
    let mut out = String::from("[\n");
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            out.push_str(",\n");
        }
        let mut fields = vec![
            ("ticket", json_option(entry.ticket())),
            ("outcome", json_string(entry.outcome())),
            ("worker", json_option(entry.worker())),
            ("phase", json_option(entry.phase())),
            (
                "pid",
                entry
                    .pid()
                    .map_or_else(|| "null".to_owned(), |pid| pid.to_string()),
            ),
        ];
        // A search entry carries the discovery it describes as structure - the
        // label looked for and the root looked under - so an empty batch says
        // *why* it is empty rather than only that it is.
        if let Some((label, root)) = entry.search() {
            fields.push(("label", json_string(label)));
            fields.push(("root", json_option(root)));
        }
        fields.push(("detail", json_string(entry.detail())));
        out.push_str("  {\n");
        for (field, (key, value)) in fields.iter().enumerate() {
            if field > 0 {
                out.push_str(",\n");
            }
            out.push_str(&format!("    \"{key}\": {value}"));
        }
        out.push_str("\n  }");
    }
    out.push_str("\n]");
    out
}

fn send_command(
    repository: &Repository,
    value: &str,
    prompt: &str,
    mode: RunMode,
    force: bool,
    open_workspaces: fn() -> Result<std::collections::BTreeSet<String>>,
) -> Result<()> {
    let worker = normalize_worker(value)?;
    let exclusions = dispatch_exclusions(repository, force, open_workspaces)?;
    eprintln!("Sending to {worker}...");
    let outcome = WorkerActions::new(repository.clone())
        .with_exclusions(exclusions)
        .send(&worker, prompt, mode)?;
    eprintln!("Agent Runtime: {}", outcome.runtime());
    render_session(outcome.session());
    if let FollowUpExecution::Background(run) = outcome.execution() {
        eprintln!(
            "  {worker} (PID {}) -> {}",
            run.pid(),
            truncate_prompt(prompt)
        );
    }
    Ok(())
}

/// Drive the bounded review loop over a Worker's Pull Request.
///
/// This is the caller ADR 0016 left the loop without: each pass reads the Pull
/// Request's unresolved review threads through `gh` ([`ThreadReviewer`]), and a
/// pass that found something is addressed to the Worker. The bound is
/// [`ReviewLoop`]'s - `[review] max_passes` and the progress rule - and each pass
/// is keyed by its step identity. Replay is scoped to this invocation
/// ([`RecordedReviewLoop::per_invocation`]), so every `jjfx review` reads the
/// threads as they are now rather than trusting a decision recorded when they may
/// have been different. Only convergence is success: a loop that stops on the cap,
/// on a stalled round, or on a review it could not read escalates its unresolved
/// findings and exits non-zero.
fn review_command(repository: &Repository, value: &str, mode: RunMode) -> Result<()> {
    let worker = normalize_worker(value)?;
    let actions = WorkerActions::new(repository.clone());
    // A repository with no forge, or a Worker with no Pull Request, is refused
    // here by name - before the loop can report convergence for a PR that does
    // not exist.
    let target = actions.review_target(&worker)?;
    let config = crate::config::load()?;
    let request = ReviewRequest::new(target.repository(), target.base(), target.branch())
        .with_number(target.number());
    let subject = ReviewSubject::new(target.ticket(), REVIEW_THREADS_DIMENSION);
    let loop_driver = RecordedReviewLoop::per_invocation(repository.work_queue(), subject);
    let reviewer = ThreadReviewer::new(crate::engine::GhReviewThreads);
    eprintln!(
        "Reviewing {worker}: {}#{} (at most {} pass(es))",
        target.repository(),
        target.number(),
        config.review.max_passes
    );
    let summary = loop_driver.run(
        ReviewLoop::new(config.review.max_passes),
        &reviewer,
        &request,
        |pass, outcome| {
            eprintln!(
                "  pass {pass}: {} unresolved review thread(s)",
                outcome.findings().len()
            );
            let sent = actions.address_review(&worker, &target, outcome.findings(), mode)?;
            render_review_outcome(&worker, &sent);
            Ok::<(), WorkerActionError>(())
        },
    )?;
    render_review_loop(&worker, &summary);
    if summary.stop().needs_escalation() {
        bail!(
            "review of {worker} did not converge ({}): {} finding(s) need a human",
            summary.stop().as_str(),
            summary.unresolved().len()
        );
    }
    Ok(())
}

/// Renders what a bounded review loop did, naming every unresolved finding when
/// it did not converge.
fn render_review_loop(worker: &WorkerId, summary: &ReviewLoopSummary) {
    eprintln!(
        "Review of {worker}: {} pass(es), {} finding(s), stopped on {}",
        summary.passes(),
        summary.findings(),
        summary.stop().as_str()
    );
    if summary.replayed() > 0 {
        eprintln!(
            "  {} pass(es) replayed from the decision log",
            summary.replayed()
        );
    }
    if let Some(reason) = summary.reason() {
        eprintln!("  {reason}");
    }
    for finding in summary.unresolved() {
        eprintln!("  unresolved {}", finding.summary());
    }
}

fn render_review_outcome(worker: &WorkerId, outcome: &FollowUpOutcome) {
    eprintln!("Agent Runtime: {}", outcome.runtime());
    render_session(outcome.session());
    if let FollowUpExecution::Background(run) = outcome.execution() {
        eprintln!("  {worker} (PID {}) -> review", run.pid());
    }
}

fn render_session(session: &crate::engine::AgentSessionResolution) {
    match session {
        crate::engine::AgentSessionResolution::Resumed { session_id } => {
            eprintln!("Resumed session {session_id}");
        }
        crate::engine::AgentSessionResolution::Fresh { reason } => {
            eprintln!("Starting fresh session ({reason})");
        }
    }
}

fn truncate_prompt(prompt: &str) -> String {
    prompt.chars().take(60).collect()
}

/// How much of a Worker Run log `logs` prints before following.
const LOG_TAIL_BYTES: u64 = 64 * 1024;

/// How long `logs` waits between follow passes.
const LOG_FOLLOW_INTERVAL: Duration = Duration::from_millis(200);

/// Why following a Worker's log stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FollowStop {
    /// The Run reached a terminal state, so its log is complete.
    RunFinished(WorkerStatus),
    /// The Worker is idle, so it has no Run and nothing is being written.
    Idle,
    /// The Worker left its Worker Pool, so nothing more can be written.
    WorkerGone,
    /// The Run log vanished, so there is nothing left to read.
    LogGone,
}

impl FollowStop {
    /// Prints why follow mode returned, so a caller reads the reason instead of
    /// inferring it from a bare exit.
    fn report(self, worker: &WorkerId) {
        match self {
            Self::RunFinished(status) => {
                eprintln!("Stopped following {worker}: Run is {}", status.as_str());
            }
            Self::Idle => eprintln!("Stopped following {worker}: the Worker is idle"),
            Self::WorkerGone => {
                eprintln!("Stopped following {worker}: the Worker left its Worker Pool");
            }
            Self::LogGone => eprintln!("Stopped following {worker}: the log is gone"),
        }
    }
}

fn logs_command(repository: &Repository, value: &str) -> Result<()> {
    let worker = normalize_worker(value)?;
    let logs = WorkerActions::new(repository.clone()).logs(&worker)?;
    match logs.session() {
        // A Run delivered through its opencode session writes no process log:
        // its activity is recorded in the session, so follow that instead of a
        // file that will never have content.
        Some(session_id) => follow_session(repository, &worker, logs.runtime(), session_id),
        None => follow_log(repository, &worker, &logs),
    }
}

/// Follow a Worker's process log, printing its tail and then each new activity.
fn follow_log(repository: &Repository, worker: &WorkerId, logs: &WorkerLogs) -> Result<()> {
    // Print the existing log first so the command never blocks silently: a
    // terminal Worker whose log does not parse would otherwise follow forever
    // with no output.
    print_log_tail(logs.path())?;
    // A Worker that is already terminal has a complete log, and an idle Worker
    // has no Run at all, so return at once rather than follow a log that will
    // never write again.
    if follow_stop(repository, worker).is_some() {
        return Ok(());
    }
    eprintln!("Following {} log for {worker}", logs.runtime());
    let mut last = None;
    loop {
        // Read the latest activity first, so a Run that ends between passes still
        // prints its final line before the loop exits.
        match logs.open().current_activity() {
            Ok(Some(activity)) => {
                let rendered = render_activity(&activity);
                if last.as_deref() != Some(rendered.as_str()) {
                    println!("{rendered}");
                    last = Some(rendered);
                }
            }
            Ok(None) => {}
            // A log that vanishes mid-follow is a clean end, not a failure:
            // there is nothing left to watch.
            Err(RunLogError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                FollowStop::LogGone.report(worker);
                return Ok(());
            }
            // Anything else is a log the command was asked to show but cannot
            // read, which is worth reporting.
            Err(error) => return Err(error.into()),
        }
        // Give the loop an exit: a Worker that was running when the command
        // attached must not keep it alive once there is no Run left to follow -
        // because the Run ended, or because the Worker was reset to idle. The
        // check is one more reconciliation on a pass that already sleeps.
        if let Some(stop) = follow_stop(repository, worker) {
            stop.report(worker);
            return Ok(());
        }
        std::thread::sleep(LOG_FOLLOW_INTERVAL);
    }
}

/// How many of a session's newest messages `logs` reads on each pass.
///
/// A Run's session can hold far more messages than a follower needs, and the
/// newest are the ones still moving, so the page is bounded rather than read in
/// full.
const SESSION_MESSAGE_PAGE: usize = 20;

/// How many activity lines `logs` prints before it starts following a session.
///
/// The process log's tail is bounded by bytes; a session's is bounded by lines,
/// because a single message can carry a long stretch of activity.
const SESSION_TAIL_ACTIVITIES: usize = 40;

/// Follow a Run delivered through its opencode session.
///
/// A session-delivered Run has no process log to read, so its activity is the
/// session's messages. The session is durable, so a finished Run still has one
/// to show. Reading it needs the daemon: when that is unreachable the command
/// says plainly that there is no process log and names the session to look at,
/// rather than following an empty file forever.
fn follow_session(
    repository: &Repository,
    worker: &WorkerId,
    runtime: AgentRuntime,
    session_id: &str,
) -> Result<()> {
    let api = OpenCodeApi::new();
    let mut feed = SessionFeed::default();
    // Print the activity already recorded first, so the command never blocks
    // silently, exactly as a process log's tail does.
    match session_activity(&api, session_id, &mut feed) {
        Ok(lines) => {
            let tail = lines.len().saturating_sub(SESSION_TAIL_ACTIVITIES);
            for line in &lines[tail..] {
                println!("{line}");
            }
        }
        Err(error) => {
            report_unreadable_session(worker, session_id, &error);
            return Ok(());
        }
    }
    // A Worker that is already terminal has a complete session, and an idle
    // Worker has no Run at all, so return rather than follow activity that will
    // never arrive.
    if follow_stop(repository, worker).is_some() {
        return Ok(());
    }
    eprintln!("Following {runtime} session {session_id} for {worker}");
    loop {
        match session_activity(&api, session_id, &mut feed) {
            Ok(lines) => {
                for line in lines {
                    println!("{line}");
                }
            }
            // A session that stops answering mid-follow is a clean end: there is
            // nothing left to read, and the reason is reported rather than
            // swallowed.
            Err(error) => {
                report_unreadable_session(worker, session_id, &error);
                return Ok(());
            }
        }
        if let Some(stop) = follow_stop(repository, worker) {
            stop.report(worker);
            return Ok(());
        }
        std::thread::sleep(LOG_FOLLOW_INTERVAL);
    }
}

/// The activity lines a session has produced that `feed` has not printed yet.
fn session_activity(
    api: &OpenCodeApi,
    session_id: &str,
    feed: &mut SessionFeed,
) -> Result<Vec<String>, OpenCodeError> {
    let messages = api.session_messages(session_id, SESSION_MESSAGE_PAGE)?;
    Ok(feed.fresh(&messages))
}

/// Says plainly that a session-delivered Run has no process log, naming the
/// session its activity is in, why that could not be read, and where to look
/// instead.
fn report_unreadable_session(worker: &WorkerId, session_id: &str, error: &OpenCodeError) {
    eprintln!(
        "No process log for {worker}: its Run is delivered through opencode session \
         {session_id}, whose activity could not be read ({error})."
    );
    eprintln!("Follow that session from opencode, or re-run once its server is reachable.");
}

/// The activity lines already printed for each of a session's messages.
///
/// A message is not immutable: an assistant message grows as its content
/// streams, and a tool part moves from running to completed in place. Tracking
/// the rendered lines per message, rather than which messages have been seen,
/// prints each new or updated activity exactly once and never reprints one that
/// has not changed.
#[derive(Debug, Default)]
struct SessionFeed {
    seen: HashMap<String, Vec<String>>,
}

impl SessionFeed {
    /// Records `messages` and returns the activity lines that are new or changed
    /// since the last pass, in message order.
    fn fresh(&mut self, messages: &[SessionMessage]) -> Vec<String> {
        let mut fresh = Vec::new();
        for message in messages {
            let rendered: Vec<String> = message.activities().iter().map(render_activity).collect();
            let previous = self.seen.entry(message.id().to_owned()).or_default();
            for (index, line) in rendered.iter().enumerate() {
                if previous.get(index) != Some(line) {
                    fresh.push(line.clone());
                }
            }
            *previous = rendered;
        }
        fresh
    }
}

/// Print the final [`LOG_TAIL_BYTES`] of a Worker Run log, trimmed to a whole
/// line at the start so a partial record is never shown.
fn print_log_tail(path: &std::path::Path) -> Result<()> {
    use std::io::{Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let length = file.metadata()?.len();
    let start = length.saturating_sub(LOG_TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut contents = Vec::new();
    file.read_to_end(&mut contents)?;
    let text = String::from_utf8_lossy(&contents);
    let text = if start == 0 {
        text.as_ref()
    } else {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    };
    if !text.is_empty() {
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

/// Why `logs` should stop following a Worker's log, or `None` while the Run is
/// still going.
///
/// Re-read on every pass: the Run can end at any moment after the command
/// attaches, so the terminal states (`Done` and `Failed`) end the follow. A
/// Worker that has left its Worker Pool will never write again either, so it ends the
/// follow too rather than hanging on a log nothing can append to.
///
/// `logs` follows the current Run, not the Worker: `Idle` means there is no Run
/// to follow, so it ends the follow as well. A Worker reset between Runs keeps
/// the log file its last Run wrote but stops writing to it, so following through
/// `idle` would block forever on output that can never grow again. A caller that
/// wants the next Run re-attaches, and `wait` is the command that blocks for one.
fn follow_stop(repository: &Repository, worker: &WorkerId) -> Option<FollowStop> {
    let snapshot = repository.worker_pool().reconcile_runs();
    let Some(state) = snapshot.worker(worker.as_str()) else {
        return Some(FollowStop::WorkerGone);
    };
    match state.status() {
        WorkerStatus::Busy => None,
        WorkerStatus::Idle => Some(FollowStop::Idle),
        WorkerStatus::Done | WorkerStatus::Failed => Some(FollowStop::RunFinished(state.status())),
    }
}

/// The exit code `wait` returns when it gave up on time: the negative
/// confirmation that the Worker is not done.
const WAIT_EXIT_TIMED_OUT: i32 = 1;
/// The exit code `wait` returns when a Worker reached a terminal state that does
/// not satisfy `--until` (it failed while the wait wanted `done`).
const WAIT_EXIT_UNSATISFIED: i32 = 2;

/// How long `wait` sleeps between reconciliation passes.
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Tests a Worker's Ticket for resolution. A boxed closure rather than `impl Fn`
/// because `wait` chooses between two closures at runtime.
type TicketResolver = Box<dyn Fn(&str) -> bool>;

/// Block until every target Worker satisfies `--until`, or give up.
///
/// The exit code carries the answer: `0` the condition was reached,
/// [`WAIT_EXIT_TIMED_OUT`] the wait gave up, [`WAIT_EXIT_UNSATISFIED`] a Worker
/// reached a terminal state that does not satisfy `--until`. Keeping the three
/// distinct is the point: a caller tells "still working" from "broke" without
/// parsing prose.
///
/// Every pass runs [`reconcile_pass`] - the same pass `status` and `ticket
/// reconcile` run - so a Worker whose process died silently still advances to a
/// terminal state instead of hanging the wait, and a finished Run still resolves
/// its local Ticket. The condition is evaluated before the first sleep, so a
/// Worker already in the waited-for state returns immediately: there is no lost
/// wakeup.
fn wait_command(repository: &Repository, args: &WaitArgs) -> Result<()> {
    let config = crate::config::load()?;
    // Resolve the pool this wait acts on through the same seam `dispatch` uses,
    // so the two cannot drift: one pool implies it - every Repository that
    // declares none has one implicit `default` - while several require `--pool`,
    // and an unknown name is refused naming the pools that exist. `--all` then
    // means every Worker in the selected pool, and a Worker `wait` prints names
    // the same pool `status` would. Without this, a multi-pool Repository would
    // act on `default` alone and neither refuse nor say which lane it meant.
    let available = config.pool_names_for(repository.root())?;
    let pool = resolve_pool(&available, args.pool.as_deref())?;
    let scoped = repository.clone().with_pool(pool);
    let repository = &scoped;

    let tickets = config.tickets_for(repository.root())?;
    let targets = wait_targets(repository, args)?;
    let ticket_resolved = wait_ticket_resolver(repository, args, &tickets, &targets)?;
    let watchdog = config.watchdog.stuck_window();
    // The pool `wait` acts on travels with the handle, so a Worker object it
    // prints names the same pool `status` would.
    let pool = repository.selected_pool().as_str();
    let deadline = args
        .timeout
        .map(|seconds| Instant::now() + Duration::from_secs(seconds));
    loop {
        let (snapshot, failures) = reconcile_pass(repository, &tickets);
        report_writeback_failures(&failures);
        let now = Timestamp::now();
        match wait_verdict(args.until, &targets, &snapshot, ticket_resolved.as_ref()) {
            WaitVerdict::Satisfied => {
                print_wait_workers(&targets, &snapshot, pool, args.json, now, watchdog);
                return Ok(());
            }
            WaitVerdict::Unsatisfied => {
                print_wait_workers(&targets, &snapshot, pool, args.json, now, watchdog);
                eprintln!(
                    "a Worker reached a terminal state that does not satisfy --until {}",
                    args.until.as_str()
                );
                std::process::exit(WAIT_EXIT_UNSATISFIED);
            }
            WaitVerdict::Pending => {}
        }
        if let Some(deadline) = deadline
            && Instant::now() >= deadline
        {
            print_wait_workers(&targets, &snapshot, pool, args.json, now, watchdog);
            eprintln!(
                "timed out after {}s waiting for --until {}; still pending: {}",
                args.timeout.unwrap_or_default(),
                args.until.as_str(),
                pending_workers(&targets, &snapshot).join(", ")
            );
            std::process::exit(WAIT_EXIT_TIMED_OUT);
        }
        std::thread::sleep(WAIT_POLL_INTERVAL);
    }
}

/// The Workers `wait` blocks for: the named ones, or every Worker with `--all`.
///
/// Read without reconciling, so a Worker whose Run has not been observed yet is
/// still a valid target. A named Worker that is not in the Worker Pool is an error:
/// waiting on a typo would otherwise block until the timeout.
fn wait_targets(repository: &Repository, args: &WaitArgs) -> Result<Vec<WorkerId>> {
    let snapshot = repository.worker_pool().snapshot();
    if args.all {
        let targets = snapshot
            .workers()
            .iter()
            .map(|worker| worker.worker_id().clone())
            .collect::<Vec<_>>();
        if targets.is_empty() {
            bail!("No Workers to wait for");
        }
        return Ok(targets);
    }
    let mut targets = Vec::new();
    for name in &args.workers {
        let worker = normalize_worker(name)?;
        if snapshot.worker(worker.as_str()).is_none() {
            bail!("no such Worker: {worker}");
        }
        if !targets.contains(&worker) {
            targets.push(worker);
        }
    }
    Ok(targets)
}

/// The predicate `wait_verdict` uses to test a Worker's Ticket for resolution.
///
/// `--until ticket-resolved` is the end-to-end confirmation, so it needs a local
/// Ticket to read: the configured source must be `local`, and every target must
/// carry a Ticket. Every other condition never consults the predicate, so the
/// config is not validated for them and the predicate always says "not
/// resolved".
fn wait_ticket_resolver(
    repository: &Repository,
    args: &WaitArgs,
    tickets: &TicketsConfig,
    targets: &[WorkerId],
) -> Result<TicketResolver> {
    if args.until != WaitCondition::TicketResolved {
        return Ok(Box::new(|_| false));
    }
    if tickets.source != TicketSource::Local {
        bail!(
            "--until ticket-resolved needs `[tickets] source = \"local\"`; \
             only a local Ticket's resolution is observable"
        );
    }
    let snapshot = repository.worker_pool().snapshot();
    for target in targets {
        if snapshot
            .worker(target.as_str())
            .and_then(|worker| worker.ticket())
            .is_none()
        {
            bail!("{target} has no Ticket to wait for with --until ticket-resolved");
        }
    }
    let local = crate::engine::LocalTickets::new(
        repository.root().join(&tickets.local.root),
        tickets.local.ready_status.clone(),
        tickets.local.resolved_status.clone(),
    );
    let resolved_status = tickets.local.resolved_status.clone();
    Ok(Box::new(move |ticket: &str| {
        local
            .ticket(ticket)
            .ok()
            .flatten()
            .is_some_and(|ticket| ticket.status().eq_ignore_ascii_case(&resolved_status))
    }))
}

/// The outcome of one evaluation of `--until` over the target Workers.
enum WaitVerdict {
    /// Every target satisfies the condition.
    Satisfied,
    /// A target reached a terminal state that does not satisfy the condition, so
    /// waiting can only ever time out.
    Unsatisfied,
    /// No target has failed the condition, and at least one has not reached it.
    Pending,
}

/// Evaluates `condition` over `targets` against one reconciled `snapshot`.
///
/// Failure wins: a terminal Worker that does not satisfy the condition decides
/// the verdict immediately, rather than waiting on the rest.
fn wait_verdict(
    condition: WaitCondition,
    targets: &[WorkerId],
    snapshot: &WorkerPoolSnapshot,
    ticket_resolved: &dyn Fn(&str) -> bool,
) -> WaitVerdict {
    let mut pending = false;
    for target in targets {
        let Some(worker) = snapshot.worker(target.as_str()) else {
            // A target that left its Worker Pool can never satisfy the condition.
            return WaitVerdict::Unsatisfied;
        };
        let satisfied = match condition {
            WaitCondition::Done => worker.status() == WorkerStatus::Done,
            WaitCondition::Failed => worker.status() == WorkerStatus::Failed,
            WaitCondition::Terminal => {
                matches!(worker.status(), WorkerStatus::Done | WorkerStatus::Failed)
            }
            WaitCondition::TicketResolved => worker.ticket().is_some_and(ticket_resolved),
        };
        if satisfied {
            continue;
        }
        if matches!(worker.status(), WorkerStatus::Done | WorkerStatus::Failed) {
            return WaitVerdict::Unsatisfied;
        }
        pending = true;
    }
    if pending {
        WaitVerdict::Pending
    } else {
        WaitVerdict::Satisfied
    }
}

/// The target Workers that have not reached a terminal state, for the timeout
/// diagnostic. On a timeout every target is non-terminal by construction.
fn pending_workers(targets: &[WorkerId], snapshot: &WorkerPoolSnapshot) -> Vec<String> {
    targets
        .iter()
        .filter(|target| match snapshot.worker(target.as_str()) {
            Some(worker) => !matches!(worker.status(), WorkerStatus::Done | WorkerStatus::Failed),
            None => true,
        })
        .map(ToString::to_string)
        .collect()
}

/// Prints the target Workers in the requested format.
///
/// `--json` prints the same per-Worker object `status --json` emits, wrapped in
/// the same `{"workers": [ ... ]}` top level, so one parser reads both. Prose
/// prints one line per Worker.
fn print_wait_workers(
    targets: &[WorkerId],
    snapshot: &WorkerPoolSnapshot,
    pool: &str,
    json: bool,
    now: Timestamp,
    watchdog: StuckWindow,
) {
    if json {
        println!(
            "{}",
            wait_workers_json(targets, snapshot, pool, now, watchdog)
        );
        return;
    }
    for target in targets {
        let Some(worker) = snapshot.worker(target.as_str()) else {
            continue;
        };
        match worker.ticket() {
            Some(ticket) => println!(
                "{} {} {ticket}",
                worker.worker_id(),
                worker.status().as_str()
            ),
            None => println!("{} {}", worker.worker_id(), worker.status().as_str()),
        }
    }
}

/// The target Workers as `{"workers": [ ... ]}`, each object the shape
/// `status --json` emits per Worker.
fn wait_workers_json(
    targets: &[WorkerId],
    snapshot: &WorkerPoolSnapshot,
    pool: &str,
    now: Timestamp,
    watchdog: StuckWindow,
) -> String {
    let workers = targets
        .iter()
        .filter_map(|target| snapshot.worker(target.as_str()))
        .collect::<Vec<_>>();
    let mut out = String::from("{\n  \"workers\": [");
    for (index, worker) in workers.iter().copied().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("\n    ");
        out.push_str(&worker_json(worker, pool, snapshot.cost(), now, watchdog));
    }
    if workers.is_empty() {
        out.push_str("]\n}");
    } else {
        out.push_str("\n  ]\n}");
    }
    out
}

fn render_activity(activity: &RunActivity) -> String {
    match activity.kind() {
        RunActivityKind::SessionStarted => "session started".to_owned(),
        RunActivityKind::Message { text } => text.clone(),
        RunActivityKind::Reasoning { text } => format!("reasoning: {text}"),
        RunActivityKind::Warning { message } => format!("warning: {message}"),
        RunActivityKind::FileChanges { paths } => format!("files: {}", paths.join(", ")),
        RunActivityKind::Plan { completed, total } => format!("plan: {completed}/{total}"),
        RunActivityKind::Tool {
            name,
            detail,
            status,
        } => {
            format!(
                "tool {name} {status:?}{}",
                detail
                    .as_deref()
                    .map_or(String::new(), |value| format!(" {value}"))
            )
        }
        RunActivityKind::Collaboration(event) => format!("collaboration: {:?}", event),
    }
}

fn mount_command(
    repository: &Repository,
    value: &str,
    terminal_settings: fn() -> Result<TerminalSettings>,
) -> Result<()> {
    let worker = normalize_worker(value)?;
    let settings = terminal_settings()?;
    let outcome = WorkerActions::new(repository.clone())
        .with_first_pane_command(settings.first_pane_command)
        .with_layout(settings.layout)
        .mount(&worker)?;
    render_session(outcome.session());
    eprintln!(
        "Mounted {worker} with {} in kitty tab {}",
        outcome.runtime(),
        outcome.tab_id()
    );
    Ok(())
}

/// Rebase and push the change a name identifies - a Worker's, or a bookmark's.
///
/// The two are the same operation on the same thing. A Worker is a reusable slot
/// and the bookmark it delivered is not, so a change whose Worker has already
/// been reused is re-welded by its bookmark: `jjfx rebase adam/<ticket>-<slug>`.
/// When no Workspace holds the change, one is created for it and named, because a
/// rebase has to rewrite a working copy and a conflict it records is only
/// reachable through a Workspace's own chain.
fn rebase_command(repository: &Repository, value: &str) -> Result<()> {
    let actions = WorkerActions::new(repository.clone());
    let target = actions.landing_target(value)?;
    // Reported before the rebase: a Workspace jjfx made is a fact whether or not
    // the rebase that follows it succeeds, and a conflicted one is exactly when
    // the operator needs to know where it is.
    if target.was_created() {
        eprintln!(
            "Created Workspace {} to hold {}; remove it with `jjfx rm {}` when you are done",
            target.workspace(),
            target.named(),
            target.workspace()
        );
    }
    let outcome = actions.rebase(&target)?;
    eprintln!("Rebased {}", outcome.branch());
    Ok(())
}

fn absorb_command(repository: &Repository, value: &str) -> Result<()> {
    let worker = normalize_worker(value)?;
    let outcome = WorkerActions::new(repository.clone()).absorb(&worker)?;
    let report = outcome.report();
    if report.is_empty() {
        eprintln!(
            "Nothing to absorb for {}: no working-copy edit belongs in an earlier commit",
            outcome.branch()
        );
        return Ok(());
    }
    eprintln!(
        "Absorbed edits into {} commit(s) on {}",
        report.revisions(),
        outcome.branch()
    );
    if report.has_remaining_changes() {
        eprintln!(
            "  Some edits stayed in the working copy: no ancestor unambiguously owns those lines."
        );
    }
    Ok(())
}

/// Open the Pull Request a name identifies in the browser - a Worker's, or a
/// bookmark's.
///
/// A Worker's name resolves to the bookmark its Ticket names, and a bookmark
/// resolves to itself; neither needs a Workspace, because the Pull Request is
/// keyed by the branch.
fn open_pr_command(repository: &Repository, value: &str) -> Result<()> {
    let outcome = WorkerActions::new(repository.clone()).open_pull_request(value)?;
    eprintln!("Opened Pull Request for {}", outcome.branch());
    Ok(())
}

/// Deliver one Worker's change through the one forge path: weld onto trunk, run
/// the repository's check, push, and open one Pull Request.
///
/// This is the delivery the Dispatch prompt promises ("jjfx delivers the
/// change"), reachable from the shell so an agent need not run `gh pr create`
/// itself. It drives the *same* pipeline the TUI's `f` key does (ADR 0016's one
/// forge path), so the two cannot disagree about what delivering means. A
/// repository with no forge - `[forge] pull_requests = false`, or no `origin`
/// remote to open one against - is a supported path: the Pull Request step does
/// not apply, and a clean weld, check and push still succeeds.
///
/// Whether the Pull Request opens as a draft is the repository's own choice
/// (`<repo>/.jjfx/config.toml` `[forge] draft`, else the user-global setting). A
/// PR left a draft is reported, naming the `gh pr ready` that marks it mergeable,
/// so the landing flow never stalls on a step jjfx never mentions.
///
/// The change is named by a Worker or by its bookmark. A Worker is a reusable
/// slot - the next Dispatch takes it back the moment its Run ends - so a change
/// whose Worker has been reused is re-welded by its bookmark, which is the
/// durable name of the change. Naming the bookmark is also how a change is
/// re-welded after a sibling merged and moved trunk under it: the same delivery,
/// run again, on the name that survived.
fn deliver_command(repository: &Repository, value: &str) -> Result<()> {
    let actions = WorkerActions::new(repository.clone());
    let target = actions.landing_target(value)?;
    if target.was_created() {
        eprintln!(
            "Created Workspace {} to hold {}; remove it with `jjfx rm {}` when you are done",
            target.workspace(),
            target.named(),
            target.workspace()
        );
    }
    // The repository's own `[checks]` and `[forge]` when it sets them, else the
    // user-global values, resolved exactly as the TUI resolves them.
    let config = crate::config::load()?;
    let forge = crate::forge::Forge::new(
        repository.root().to_path_buf(),
        crate::forge::Settings {
            forge: config.forge_for(repository.root())?,
            checks: config.checks_for(repository.root())?,
        },
    );
    eprintln!(
        "Delivering {}: weld, check, push, Pull Request",
        target.named()
    );
    let report = forge.deliver(crate::forge::Target::new(
        target.workspace().to_owned(),
        target.path().to_path_buf(),
    ));
    for notice in report.notices() {
        eprintln!("  {notice}");
    }
    if report.delivered() {
        eprintln!("Delivered {}", target.named());
        return Ok(());
    }
    bail!(
        "delivery of {} stopped: {}",
        target.named(),
        report
            .reason()
            .unwrap_or("the delivery did not finish cleanly")
    )
}

/// Resolve a Workspace's conflicted commits with `jj resolve`.
///
/// Interactive: the merge tool inherits this terminal, so a caller that has no
/// terminal must open one first (the TUI opens a tab running this command).
fn resolve_command(repository: &Repository, value: &str, tool: Option<&str>) -> Result<()> {
    // The revset needs the Workspace's real name; a short Worker id (`3`) names
    // `worker-3`, and any name jj already reports is used as it is.
    let workspace = if crate::jj::workspace_names(repository.root())?
        .iter()
        .any(|name| name == value)
    {
        value.to_owned()
    } else {
        format!("worker-{value}")
    };
    let dir = workspace_path(repository, &workspace);
    if !dir.is_dir() {
        bail!(
            "no Workspace directory for {workspace} at {}",
            dir.display()
        );
    }

    let before = crate::jj::conflicted_commits(repository.root(), &workspace)?;
    if before.is_empty() {
        eprintln!("{workspace} has no conflicts to resolve");
        return Ok(());
    }
    eprintln!(
        "resolving {} conflicted commit(s) on {workspace}: {}",
        before.len(),
        before.join(", ")
    );

    let resolved = crate::jj::resolve_chain(repository.root(), &dir, &workspace, tool)?;
    let remaining = crate::jj::conflicted_commits(repository.root(), &workspace)?;
    if remaining.is_empty() {
        eprintln!("resolved {workspace}: {}", resolved.join(", "));
        return Ok(());
    }
    // The merge tool exited without resolving everything - say exactly what is
    // left rather than reporting a success that is not one.
    bail!(
        "{} conflicted commit(s) remain on {workspace}: {}",
        remaining.len(),
        remaining.join(", ")
    )
}

/// Undo the last jjfx-initiated mutation on a Workspace, or list what can be
/// undone when no Workspace is named.
fn undo_command(repository: &Repository, workspace: Option<&str>, force: bool) -> Result<()> {
    let Some(workspace) = workspace else {
        return list_undoable(repository);
    };
    let workspace = undo_workspace_key(repository, workspace)?;
    match repository.undo_mutation(&workspace, force)? {
        UndoOutcome::Nothing => {
            bail!("nothing recorded to undo for {workspace}")
        }
        UndoOutcome::Restored {
            kind,
            discarded,
            later,
        } => {
            eprintln!("Undid the last {kind} on {workspace} ({discarded} operations)");
            if later > 0 {
                eprintln!(
                    "warning: the restore is repository-wide and also discarded {later} later operation(s)"
                );
            }
            Ok(())
        }
        UndoOutcome::Refused { kind, later } => match later {
            Some(later) => bail!(
                "refusing to undo the last {kind} on {workspace}: {later} operation(s) have happened since, \
                 and a restore is repository-wide. Re-run with --force to discard them, or inspect `jj op log`."
            ),
            None => bail!(
                "refusing to undo the last {kind} on {workspace}: its recorded operation is no longer in the log, \
                 so what a restore would discard cannot be established. Inspect `jj op log`."
            ),
        },
    }
}

/// Resolve the undo record key a user named: a Worker in either spelling, any
/// Workspace name, or the repository-wide marker.
///
/// A name that already has a record is used as it is, so an ad-hoc Workspace is
/// reachable by its own name; otherwise the name is normalized to a Worker's
/// `worker-<id>`, which is how `3` reaches `worker-3`.
fn undo_workspace_key(repository: &Repository, value: &str) -> Result<String> {
    if value == REPOSITORY_WIDE_MUTATION || value.eq_ignore_ascii_case("repo") {
        return Ok(REPOSITORY_WIDE_MUTATION.to_owned());
    }
    if repository.state_store().undo().mutation(value)?.is_some() {
        return Ok(value.to_owned());
    }
    Ok(match normalize_worker(value) {
        Ok(worker) => worker.as_str().to_owned(),
        Err(_) => value.to_owned(),
    })
}

/// Print the jjfx-initiated mutations that can be undone.
fn list_undoable(repository: &Repository) -> Result<()> {
    let mutations = match repository.state_store().undo().load()? {
        Loaded::Present(versioned) => versioned.value.mutations,
        Loaded::Missing => BTreeMap::new(),
    };
    if mutations.is_empty() {
        eprintln!("Nothing recorded to undo.");
        return Ok(());
    }
    println!("{:<20} {:<8} WHEN", "WORKSPACE", "MUTATION");
    for (workspace, record) in mutations {
        println!("{workspace:<20} {:<8} {}", record.kind.as_str(), record.at);
    }
    Ok(())
}

fn completion_command(shell: &str) -> Result<()> {
    if shell != "zsh" {
        bail!("Unsupported shell: {shell} (supported: zsh)");
    }
    print!("{ZSH_COMPLETION}");
    Ok(())
}

fn internal_complete_command(mode: &str) -> Result<()> {
    let Ok(repository) = Repository::open(".") else {
        return Ok(());
    };
    match mode {
        "workers" => {
            for worker in repository.worker_pool().snapshot().workers() {
                println!("{}", worker.worker_id());
            }
        }
        "workspaces" => {
            for entry in repository.workspaces().snapshot()?.entries() {
                println!("{}", entry.name());
            }
        }
        // The names a landing verb accepts: every Worker, and every bookmark -
        // the durable name of a delivered change, and the only name left once
        // the Worker that delivered it has been reused.
        "changes" => {
            for worker in repository.worker_pool().snapshot().workers() {
                println!("{}", worker.worker_id());
            }
            if let Ok(bookmarks) = crate::jj::local_bookmark_names(repository.root()) {
                for bookmark in bookmarks {
                    println!("{bookmark}");
                }
            }
        }
        "idle-workers" | "done-workers" | "failed-workers" | "non-busy-workers" => {
            for worker in repository.worker_pool().reconcile_runs().workers() {
                let status = worker.status();
                let include = match mode {
                    "idle-workers" => status == WorkerStatus::Idle,
                    "done-workers" => status == WorkerStatus::Done,
                    "failed-workers" => status == WorkerStatus::Failed,
                    _ => status != WorkerStatus::Busy,
                };
                if include {
                    println!(
                        "{}\t{}{}",
                        worker.worker_id(),
                        status.as_str(),
                        worker
                            .ticket()
                            .map_or(String::new(), |ticket| format!(" {ticket}"))
                    );
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// A Parent Ticket resolved and prepared for orchestration, before either the
/// foreground watch or the detached runner starts.
///
/// The fallible front half of orchestration - config, discovery, the Parent's
/// own fetch, and the group prepare/resume - is done in
/// [`prepare_orchestration`], so a caller runs it synchronously and surfaces its
/// error before blocking (foreground) or spawning a child (background).
struct OrchestrationHandoff {
    parent: TicketId,
    request: crate::engine::OrchestrationRequest,
    runner: crate::engine::OrchestrationRunner,
    start: crate::engine::OrchestrationStart,
    resumed: bool,
    tickets: TicketsConfig,
}

/// Resolves a Parent Ticket and prepares its Dispatch Group.
///
/// This is the whole fallible front half of orchestration, run synchronously so
/// an unresolvable Ticket, a missing Pool, or a rejected discovery fails where
/// the caller can see it rather than inside a detached child.
fn prepare_orchestration(
    repository: &Repository,
    parent: &str,
    model: Option<AgentModel>,
    force: bool,
    open_workspaces: fn() -> Result<std::collections::BTreeSet<String>>,
    tickets_config: fn(&Repository) -> Result<TicketsConfig>,
) -> Result<OrchestrationHandoff> {
    let id = TicketId::parse(parent.to_owned())?;
    // The runner re-queries occupancy per claim, so a Workspace a human opens or
    // closes while the group runs changes the next decision (ADR-0011).
    let exclusions = exclusions_provider(repository, force, open_workspaces);
    let tickets = tickets_config(repository)?;
    let ticket_hint = ticket_source_hint(&tickets, repository);
    let runtime = configured_runtime(repository);
    let discovery = TicketDiscovery::new(configured_ticket_query(
        repository.root(),
        &tickets,
        runtime,
        DEFAULT_READY_LABEL,
    )?);
    // Fetch the Parent's details before `prepare` reserves a placeholder, so the
    // no-children direct fallback inlines the Ticket body. A local feature Parent
    // has none - it is a directory, not a Ticket file - and resolves to `None`.
    let details = fetch_ticket_details(&discovery, &id, &ticket_hint)?;
    let mut request = crate::engine::OrchestrationRequest::new(id.clone(), runtime);
    if let Some(details) = details {
        request = request.with_ticket_details(details);
    }
    let request = match model {
        Some(selection) => request.with_model(selection),
        None => request,
    };
    // The concurrency cap bounds a wave too, not just a batch: a Parent's
    // Sub-issues are Runs the same governor counts, so a wave that would exceed
    // the Repository's `dispatch.max_concurrent` holds the excess ready rather
    // than firing it. The Repository's own ceiling tightens the user-global one.
    let request =
        request.with_concurrency_cap(crate::config::load()?.dispatch_cap_for(repository.root())?);
    let ticket = DirectDispatchRequest::for_ticket_id(id.clone(), RunMode::Background)?
        .ticket()
        .clone();
    let parent_ticket = crate::engine::ParentTicket::new(ticket.id().clone());
    let runner = repository
        .orchestration_runner()
        .with_exclusions(exclusions);
    let preparation = runner.prepare(&request, &parent_ticket, &discovery)?;
    let resumed = preparation.resumed();
    Ok(OrchestrationHandoff {
        parent: id,
        request,
        runner,
        start: preparation.into_start(),
        resumed,
        tickets,
    })
}

fn orchestrate_command(
    repository: &Repository,
    parent: &str,
    model: Option<AgentModel>,
    force: bool,
    json: bool,
    open_workspaces: fn() -> Result<std::collections::BTreeSet<String>>,
    tickets_config: fn(&Repository) -> Result<TicketsConfig>,
) -> Result<()> {
    let handoff = prepare_orchestration(
        repository,
        parent,
        model,
        force,
        open_workspaces,
        tickets_config,
    )?;
    match handoff.start {
        // No free Worker is a capacity gap, not a failure: the Parent is
        // reported as waiting, in the same shape a held-back Dispatch uses.
        crate::engine::OrchestrationStart::Waiting { ticket, detail } => {
            render_dispatch_entries(
                &[DispatchEntry::Waiting {
                    ticket: ticket.as_str().to_owned(),
                    detail,
                }],
                json,
            );
            return Ok(());
        }
        crate::engine::OrchestrationStart::Direct(success) => {
            if !json {
                render_orchestration_event(&OrchestrationEvent::Started {
                    parent: handoff.parent.clone(),
                    resumed: handoff.resumed,
                });
            }
            render_dispatch_entries(&[DispatchEntry::from_success(&success)], json);
            return Ok(());
        }
        crate::engine::OrchestrationStart::Group => {}
    }
    if !json {
        render_orchestration_event(&OrchestrationEvent::Started {
            parent: handoff.parent.clone(),
            resumed: handoff.resumed,
        });
    }
    let options = crate::engine::OrchestrationOptions::new();
    let summary = handoff.runner.run(&handoff.request, &options, |event| {
        if !json {
            render_orchestration_event(&event);
        }
    })?;
    if json {
        let counts = summary.counts();
        render_dispatch_entries(
            &[DispatchEntry::Orchestrating {
                ticket: handoff.parent.as_str().to_owned(),
                detail: format!(
                    "{} done, {} failed, {} skipped, {} blocked",
                    counts.done(),
                    counts.failed(),
                    counts.skipped(),
                    counts.blocked()
                ),
            }],
            true,
        );
    } else {
        render_orchestration_event(&OrchestrationEvent::Terminal(summary));
    }
    write_back_local_tickets(repository, &handoff.tickets);
    Ok(())
}

/// Spawns the detached orchestration runner for a prepared Parent Ticket.
///
/// The child's stdout and stderr go to a per-Parent log under the pool
/// directory (`.jj/pools/<name>/`), and
/// the log path is returned so the Dispatch result can name it: a failure after
/// the handoff is reachable even though the parent has already returned.
fn spawn_background_orchestration(
    repository: &Repository,
    parent: &TicketId,
    args: &DispatchArgs,
) -> Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let mut command = std::process::Command::new(executable);
    command.arg("__orchestrate").arg(parent.as_str());
    // The detached runner re-discovers the Repository, so it must be told which
    // pool to fill: without this a named pool's orchestration would run against
    // `default`, and its Workers, locks and log would all land in the wrong lane.
    command.args(["--pool", repository.selected_pool().as_str()]);
    if let Some(provider) = args.provider.as_deref() {
        command.args(["--provider", provider]);
    }
    if let Some(model) = args.model.as_deref() {
        command.args(["--model", model]);
    }
    if args.force {
        command.arg("--force");
    }
    let log = orchestration_log_path(repository, parent);
    if let Some(directory) = log.parent() {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("cannot create {}", directory.display()))?;
    }
    let stdout = std::fs::File::create(&log)
        .with_context(|| format!("cannot create orchestration log {}", log.display()))?;
    let stderr = stdout
        .try_clone()
        .with_context(|| format!("cannot open orchestration log {}", log.display()))?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(stdout))
        .stderr(std::process::Stdio::from(stderr))
        .spawn()
        .with_context(|| format!("cannot start background orchestration for {parent}"))?;
    Ok(log)
}

/// The log a detached orchestration runner writes to, under the Worker Pool's
/// directory.
///
/// A [`TicketId`] is a safe filename component, so it needs no escaping. The
/// name is lowercased to match the orchestration lock, which is case-folded
/// because the filesystem may be case-insensitive.
fn orchestration_log_path(repository: &Repository, parent: &TicketId) -> PathBuf {
    repository.state_store().orchestration_log(parent)
}

fn render_orchestration_event(event: &OrchestrationEvent) {
    match event {
        OrchestrationEvent::Started { parent, resumed } => eprintln!(
            "{} {}",
            if *resumed { "Resuming" } else { "Starting" },
            parent
        ),
        OrchestrationEvent::Dispatched { ticket, worker } => eprintln!("  {ticket} -> {worker}"),
        OrchestrationEvent::Completed { ticket, worker, .. } => {
            eprintln!("  {ticket} completed on {worker}")
        }
        OrchestrationEvent::Retrying {
            ticket, attempt, ..
        } => eprintln!("  retrying {ticket} (attempt {attempt})"),
        OrchestrationEvent::QuotaBlocked { ticket, detail, .. } => eprintln!(
            "  quota-blocked {ticket}: {} (waiting; not retried)",
            detail
                .as_deref()
                .unwrap_or("provider quota or billing wall")
        ),
        OrchestrationEvent::WaitingForCapacity { ticket } => {
            eprintln!("  waiting for capacity: {ticket}")
        }
        OrchestrationEvent::LaunchFailed { ticket, detail, .. } => {
            eprintln!("  launch failed {ticket}: {detail}")
        }
        OrchestrationEvent::BranchRevalidated {
            ticket, current, ..
        } => eprintln!("  repaired {ticket} -> {current}"),
        OrchestrationEvent::Failed { ticket, detail, .. } => eprintln!(
            "  failed {ticket}: {}",
            detail.as_deref().unwrap_or("unknown error")
        ),
        OrchestrationEvent::Terminal(summary) => {
            let counts = summary.counts();
            eprintln!(
                "Orchestration complete: {} done, {} failed, {} skipped, {} blocked",
                counts.done(),
                counts.failed(),
                counts.skipped(),
                counts.blocked()
            );
        }
    }
}

const ZSH_COMPLETION: &str = r#"#compdef jjfx

__jjfx_workers() {
  local -a workers
  workers=("${(@f)$(jjfx __complete workers 2>/dev/null)}")
  _describe 'worker' workers
}
__jjfx_non_busy_workers() {
  local -a workers
  workers=("${(@f)$(jjfx __complete non-busy-workers 2>/dev/null)}")
  _describe 'worker' workers
}
__jjfx_workspaces() {
  local -a workspaces
  workspaces=("${(@f)$(jjfx __complete workspaces 2>/dev/null)}")
  _describe 'workspace' workspaces
}
__jjfx_changes() {
  local -a changes
  changes=("${(@f)$(jjfx __complete changes 2>/dev/null)}")
  _describe 'change' changes
}

_jjfx() {
  local -a commands
  commands=(
    'add:Create workspace' 'rm:Remove workspace' 'list:List workspaces'
    'clean:Remove workspaces' 'root:Print repository root'
    'where:Show repository paths' 'path:Print workspace path'
    'refresh:Rebuild workspace cache' 'pool:Manage Worker Pool'
    'dispatch:Dispatch Tickets' 'send:Send a Follow-up'
    'review:Address review comments' 'mount:Open Worker in kitty'
    'reset:Reset Worker' 'status:Show Pool status' 'logs:Follow Worker log'
    'rebase:Rebase change' 'open-pr:Open change Pull Request'
    'deliver:Deliver change (weld, check, push, PR)'
    'resolve:Resolve a Workspace conflict'
    'absorb:Fold Worker edits into base commits'
    'wait:Block until Workers reach a condition'
    'undo:Undo the last jjfx mutation' 'ticket:Reconcile local Tickets' 'completion:Print shell completion' 'help:Show help'
  _arguments -C '1:command:->command' '*::arg:->args'
  case $state in
    command) _describe 'command' commands ;;
    args)
      case $words[1] in
        rm|remove|path) _arguments '1:workspace:__jjfx_workspaces' ;;
        clean) _arguments '--pool[Worker Pool to scope the sweep to]:name:' ;;
        send|s) _arguments '--fg[run in foreground]' '--bg[run in background]' '1:worker:__jjfx_non_busy_workers' '2:prompt:' ;;
        review|rev) _arguments '--fg[run in foreground]' '--bg[run in background]' '1:worker:__jjfx_non_busy_workers' ;;
        mount|m|reset|logs|log|absorb|ab) _arguments '1:worker:__jjfx_workers' ;;
        rebase|rb|open-pr|pr|deliver) _arguments '1:change:__jjfx_changes' ;;
        wait) _arguments '--until[condition to wait for]:condition:(done failed terminal ticket-resolved)' '--timeout[give up after N seconds]:secs:' '--all[wait for every Worker in the pool]' '--json[print machine-readable results]' '--pool[Worker Pool to wait on]:pool:' '*:worker:__jjfx_workers' ;;
        resolve) _arguments '--tool[merge tool]:tool:' '1:worker:__jjfx_workers' ;;
        undo) _arguments '--force[discard later operations]' '1:worker:__jjfx_workers' ;;
        ticket) _arguments '1:subcommand:(reconcile)' ;;
        pool) _arguments '1:subcommand:(list create destroy resize rm reset profile)' '2:runtime:(claude codex opencode)' '--provider[model provider]:provider:' '--model[model]:model:' '--pool[pool name]:name:' '--size[pool size]:N:' '--profile[Agent Runtime Profile]:name:' '--source[Ticket source]:(linear local)' '--label[routing label]:label:' '--ready-status[ready Status:]:status:' '--cap-usd[spend cap in USD]:dollars:' '--window[cap window]:(today week total)' '--on-exceed[cap mode]:(block warn)' ;;
        dispatch|d) _arguments '--fg[run in foreground]' '--bg[run in background]' '--all[dispatch all ready Tickets]' '--text[dispatch a one-off Ticket from text]:text:' '--from-file[dispatch a Ticket from a markdown file]:path:_files' '--id[Ticket id for ad-hoc input]:id:' '--no-orchestrate[skip orchestration]' '--json[print machine-readable results]' '--pool[Worker Pool to fill]:pool:' '--provider[model provider]:provider:' '--model[model]:model:' '--budget[maximum USD]:dollars:' '--label[label]:label:' '*:Ticket:' ;;
      esac
      ;;
  esac
}
compdef _jjfx jjfx
"#;

fn default_command(launch: fn(PathBuf) -> Result<()>) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        print!("{}", crate::cli_help::top_level());
        return Ok(());
    }
    let repository = Repository::open(".").map_err(|_| anyhow::anyhow!("Not in a jj repo"))?;
    launch(repository.root().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::RunActivityStatus;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn text_activity(text: &str) -> RunActivity {
        RunActivity::new(RunActivityKind::Message {
            text: text.to_owned(),
        })
    }

    fn tool_activity(status: RunActivityStatus) -> RunActivity {
        RunActivity::new(RunActivityKind::Tool {
            name: "bash".to_owned(),
            detail: Some("cargo test".to_owned()),
            status,
        })
    }

    #[test]
    fn session_feed_prints_a_message_that_grows_one_activity_at_a_time() {
        let mut feed = SessionFeed::default();
        let first = SessionMessage::new("msg_1", vec![text_activity("reading the ticket")]);

        assert_eq!(
            feed.fresh(&[first]),
            ["reading the ticket"],
            "the first pass prints the message's activity"
        );

        // An unchanged message is not reprinted: the follower only reports what
        // is new.
        let unchanged = SessionMessage::new("msg_1", vec![text_activity("reading the ticket")]);
        assert!(feed.fresh(&[unchanged]).is_empty());

        // The assistant message grows as its content streams, and only the part
        // that arrived is printed.
        let grown = SessionMessage::new(
            "msg_1",
            vec![
                text_activity("reading the ticket"),
                tool_activity(RunActivityStatus::InProgress),
            ],
        );
        assert_eq!(feed.fresh(&[grown]), ["tool bash InProgress cargo test"]);
    }

    #[test]
    fn session_feed_reprints_an_activity_that_changed_in_place() {
        let mut feed = SessionFeed::default();
        let running =
            SessionMessage::new("msg_1", vec![tool_activity(RunActivityStatus::InProgress)]);
        feed.fresh(&[running]);

        // A tool part moves from running to completed in place: the updated line
        // is printed even though the message already existed.
        let completed =
            SessionMessage::new("msg_1", vec![tool_activity(RunActivityStatus::Completed)]);

        assert_eq!(feed.fresh(&[completed]), ["tool bash Completed cargo test"]);
    }

    #[test]
    fn session_feed_prints_messages_in_order_and_skips_inactivity() {
        let mut feed = SessionFeed::default();
        let messages = [
            SessionMessage::new("msg_1", Vec::new()),
            SessionMessage::new("msg_2", vec![text_activity("first")]),
            SessionMessage::new("msg_3", vec![text_activity("second")]),
        ];

        assert_eq!(feed.fresh(&messages), ["first", "second"]);
    }

    #[test]
    fn session_feed_does_not_reprint_a_message_that_leaves_the_page() {
        let mut feed = SessionFeed::default();
        let older = SessionMessage::new("msg_1", vec![text_activity("older")]);
        let newer = SessionMessage::new("msg_2", vec![text_activity("newer")]);
        feed.fresh(&[older.clone(), newer.clone()]);

        // The page only ever holds the newest messages, so an older one drops
        // out. It must not be printed again when the page is read.
        assert!(feed.fresh(&[newer]).is_empty());
    }

    /// `clean` is Repository-wide by default, and `--pool <name>` scopes it.
    #[test]
    fn clean_parser_reads_an_optional_pool_scope() {
        assert_eq!(
            parse(&args(&["clean"])).expect("a bare clean parses"),
            Command::Clean { pool: None }
        );
        assert_eq!(
            parse(&args(&["clean", "--pool", "bugs"])).expect("--pool parses"),
            Command::Clean {
                pool: Some("bugs".to_owned()),
            }
        );
        assert!(
            parse(&args(&["clean", "--pool"])).is_err(),
            "a value-taking flag with no value must fail"
        );
        assert!(
            parse(&args(&["clean", "bugs"])).is_err(),
            "clean takes no positional argument"
        );
    }

    #[test]
    fn ticket_list_parser_takes_the_lane_and_the_output_format() {
        assert_eq!(
            parse(&args(&["ticket", "list"])).expect("a bare list parses"),
            Command::Ticket(TicketCommand::List(TicketListArgs {
                pool: None,
                label: None,
                json: false,
            }))
        );
        assert_eq!(
            parse(&args(&[
                "ticket", "list", "--json", "--label", "bug", "--pool", "bugs"
            ]))
            .expect("the flags parse"),
            Command::Ticket(TicketCommand::List(TicketListArgs {
                pool: Some("bugs".to_owned()),
                label: Some("bug".to_owned()),
                json: true,
            }))
        );
        assert!(
            parse(&args(&["ticket", "list", "--label"])).is_err(),
            "a value-taking flag with no value must fail"
        );
        assert!(
            parse(&args(&["ticket", "list", "bugs"])).is_err(),
            "the list takes no positional argument"
        );
    }

    #[test]
    fn dispatch_parser_preserves_mode_model_budget_and_controls() {
        let command = parse(&args(&[
            "dispatch",
            "AMBA-42",
            "--fg",
            "--provider",
            "anthropic",
            "--model",
            "opus",
            "--budget",
            "12",
            "--no-orchestrate",
        ]))
        .expect("dispatch arguments should parse");
        assert_eq!(
            command,
            Command::Dispatch(DispatchArgs {
                tickets: vec!["AMBA-42".to_owned()],
                all: false,
                mode: RunMode::Foreground,
                provider: Some("anthropic".to_owned()),
                model: Some("opus".to_owned()),
                budget: Some(12),
                label: None,
                pool: None,
                no_orchestrate: true,
                force: false,
                json: false,
                input: None,
            })
        );
    }

    #[test]
    fn parser_rejects_unknown_dispatch_options_and_budgeted_orchestration() {
        assert!(parse(&args(&["dispatch", "AMBA-42", "--wat"])).is_err());
        assert!(parse(&args(&["dispatch", "AMBA-42", "--budget", "12"])).is_err());
    }

    /// The ad-hoc Ticket inputs: free text, a file, and stdin, each with an
    /// optional explicit id.
    #[test]
    fn dispatch_parser_reads_ad_hoc_ticket_inputs() {
        let text = parse(&args(&[
            "dispatch",
            "--text",
            "fix the bug",
            "--id",
            "ENG-7",
        ]))
        .expect("--text should parse");
        assert_eq!(
            text,
            Command::Dispatch(DispatchArgs {
                tickets: Vec::new(),
                all: false,
                mode: RunMode::Background,
                provider: None,
                model: None,
                budget: None,
                label: None,
                pool: None,
                no_orchestrate: false,
                force: false,
                json: false,
                input: Some(AdHocInput {
                    source: AdHocSource::Text("fix the bug".to_owned()),
                    id: Some("ENG-7".to_owned()),
                }),
            })
        );

        let file = parse(&args(&["dispatch", "--from-file", "notes/thing.md"]))
            .expect("--from-file should parse");
        assert!(matches!(
            file,
            Command::Dispatch(DispatchArgs {
                input: Some(AdHocInput {
                    source: AdHocSource::File(path),
                    id: None,
                }),
                ..
            }) if path.to_str() == Some("notes/thing.md")
        ));

        for stdin in [
            args(&["dispatch", "--from-file", "-"]),
            args(&["dispatch", "-"]),
        ] {
            let command = parse(&stdin).expect("stdin should parse");
            assert!(matches!(
                command,
                Command::Dispatch(DispatchArgs {
                    input: Some(AdHocInput {
                        source: AdHocSource::Stdin,
                        ..
                    }),
                    ..
                })
            ));
        }
    }

    #[test]
    fn dispatch_parser_rejects_conflicting_ad_hoc_inputs() {
        // One Ticket at a time: a second input, or a positional Ticket
        // alongside one, is ambiguous.
        assert!(parse(&args(&["dispatch", "--text", "a", "--from-file", "b.md"])).is_err());
        assert!(parse(&args(&["dispatch", "--text", "a", "--text", "b"])).is_err());
        assert!(parse(&args(&["dispatch", "-", "-"])).is_err());
        assert!(parse(&args(&["dispatch", "--text", "a", "AMBA-42"])).is_err());
        // `--all` discovers from a source, which an ad-hoc Ticket bypasses.
        assert!(parse(&args(&["dispatch", "--all", "--text", "a"])).is_err());
        // `--id` names the ad-hoc Ticket, so it needs one.
        assert!(parse(&args(&["dispatch", "AMBA-42", "--id", "ENG-7"])).is_err());
        assert!(parse(&args(&["dispatch", "--id", "ENG-7"])).is_err());
    }

    /// A bare `resume` is the recovery an operator runs after an interruption:
    /// it names no Ticket and means every unfinished step the queue holds.
    #[test]
    fn resume_parser_reads_the_steps_to_resume() {
        assert_eq!(
            parse(&args(&["resume"])).expect("a bare resume parses"),
            Command::Resume(ResumeArgs {
                tickets: Vec::new(),
                mode: RunMode::Background,
                pool: None,
                force: false,
                json: false,
            })
        );

        assert_eq!(
            parse(&args(&[
                "resume", "ENG-1", "ENG-2", "--fg", "--pool", "bugs", "--force", "--json",
            ]))
            .expect("resume with every flag parses"),
            Command::Resume(ResumeArgs {
                tickets: vec!["ENG-1".to_owned(), "ENG-2".to_owned()],
                mode: RunMode::Foreground,
                pool: Some("bugs".to_owned()),
                force: true,
                json: true,
            })
        );
    }

    #[test]
    fn resume_parser_rejects_unknown_options_and_a_missing_value() {
        let error = parse(&args(&["resume", "--all"]))
            .expect_err("resume has no --all: a bare resume already means everything");
        assert!(error.to_string().contains("--all"), "{error}");
        let error =
            parse(&args(&["resume", "--pool"])).expect_err("--pool needs a Worker Pool name");
        assert!(error.to_string().contains("--pool"), "{error}");
    }

    #[test]
    fn dispatch_parser_accepts_a_budget_for_an_ad_hoc_ticket() {
        // Ad-hoc Tickets are always direct, so the single-Ticket budget guard
        // (which exists for orchestration) does not apply.
        let command = parse(&args(&["dispatch", "--text", "a", "--budget", "5"]))
            .expect("--budget with --text should parse");
        assert!(matches!(
            command,
            Command::Dispatch(DispatchArgs {
                budget: Some(5),
                ..
            })
        ));
    }

    #[test]
    fn dispatch_parser_accepts_json() {
        let command =
            parse(&args(&["dispatch", "AMBA-42", "--json"])).expect("dispatch --json should parse");
        assert!(matches!(
            command,
            Command::Dispatch(DispatchArgs { json: true, .. })
        ));
    }

    /// `--pool` names the lane a Dispatch fills. It parses for a positional
    /// Ticket, `--all`, and the ad-hoc inputs alike, because all three pass
    /// through the same resolution.
    #[test]
    fn dispatch_parser_reads_the_pool_flag() {
        for arguments in [
            args(&["dispatch", "AMBA-42", "--pool", "bugs"]),
            args(&["dispatch", "--all", "--pool", "bugs"]),
            args(&["dispatch", "--text", "a", "--pool", "bugs"]),
        ] {
            let command = parse(&arguments).expect("--pool should parse");
            assert!(
                matches!(
                    command,
                    Command::Dispatch(DispatchArgs {
                        pool: Some(ref name),
                        ..
                    }) if name == "bugs"
                ),
                "{arguments:?}"
            );
        }
    }

    #[test]
    fn check_parser_accepts_migrations_and_rejects_anything_else() {
        assert_eq!(
            parse(&args(&["check", "migrations"])).unwrap(),
            Command::Check(CheckCommand::Migrations)
        );
        assert!(parse(&args(&["check"])).is_err());
        assert!(parse(&args(&["check", "wat"])).is_err());
    }

    #[test]
    fn colocation_parser_accepts_the_three_actions_and_rejects_anything_else() {
        assert_eq!(
            parse(&args(&["colocation", "enable"])).unwrap(),
            Command::Colocation {
                action: ColocationAction::Enable,
            }
        );
        assert_eq!(
            parse(&args(&["colocate", "disable"])).unwrap(),
            Command::Colocation {
                action: ColocationAction::Disable,
            }
        );
        assert_eq!(
            parse(&args(&["colocation", "status"])).unwrap(),
            Command::Colocation {
                action: ColocationAction::Status,
            }
        );
        assert!(parse(&args(&["colocation"])).is_err());
        assert!(parse(&args(&["colocation", "wat"])).is_err());
        assert!(parse(&args(&["colocation", "enable", "extra"])).is_err());
    }

    /// A colocated jj repository in a temporary directory, signing disabled.
    /// `colocation status` reports colocation, so the test has to ask for it.
    fn colocated_repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().expect("temporary repository");
        let output = crate::jj::command()
            .args(["git", "init", "--colocate"])
            .arg(repo.path())
            .output()
            .expect("jj should be installed");
        assert!(
            output.status.success(),
            "jj git init --colocate failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        repo
    }

    /// `colocation status` is a read, so it must not wait on jj's working-copy
    /// lock - a plain `jj git colocation status` snapshots, and while another jj
    /// process holds that lock it blocks silently for as long as the holder runs
    /// (dogfood 08). The read carries `--ignore-working-copy` and returns anyway.
    ///
    /// The negative control re-establishes that the lock really is held, so a
    /// prompt result is the flag's doing and not a lock that was never taken.
    #[test]
    fn colocation_status_does_not_wait_on_the_working_copy_lock() {
        let repo = colocated_repo();
        let repository = Repository::open(repo.path()).expect("repository");
        let _lock = crate::test_support::hold_the_working_copy_lock(repository.root());

        let mut snapshotting = std::process::Command::new("jj")
            .args(["git", "colocation", "status"])
            .current_dir(repository.root())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("jj should be installed");
        std::thread::sleep(Duration::from_secs(2));
        let blocked = snapshotting.try_wait().expect("polling jj").is_none();
        snapshotting.kill().ok();
        snapshotting.wait().ok();
        assert!(
            blocked,
            "the working-copy lock must block a snapshotting jj, or the lock is not held"
        );

        // Bounded and detached rather than joined: the defect this guards is an
        // *unbounded* wait, so a regression must fail the test rather than hang
        // the suite. Dropping the lock on the way out unblocks the child.
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(colocation_command(&repository, &ColocationAction::Status));
        });
        match receiver.recv_timeout(crate::test_support::LIVENESS_TIMEOUT) {
            Ok(outcome) => outcome.expect("colocation status succeeds"),
            Err(_) => panic!("colocation status waited on the working-copy lock"),
        }
    }

    #[test]
    fn ticket_reconcile_parses_and_rejects_anything_else() {
        assert_eq!(
            parse(&args(&["ticket", "reconcile"])).unwrap(),
            Command::Ticket(TicketCommand::Reconcile)
        );
        assert!(parse(&args(&["ticket"])).is_err());
        assert!(parse(&args(&["ticket", "wat"])).is_err());
        assert!(parse(&args(&["ticket", "reconcile", "extra"])).is_err());
    }

    #[test]
    fn wait_parses_workers_condition_timeout_json_and_all() {
        assert_eq!(
            parse(&args(&["wait", "3"])).unwrap(),
            Command::Wait(WaitArgs {
                workers: vec!["3".to_owned()],
                all: false,
                pool: None,
                until: WaitCondition::Terminal,
                timeout: None,
                json: false,
            })
        );
        assert_eq!(
            parse(&args(&[
                "wait",
                "3",
                "worker-4",
                "--until",
                "done",
                "--timeout",
                "5",
                "--json",
            ]))
            .unwrap(),
            Command::Wait(WaitArgs {
                workers: vec!["3".to_owned(), "worker-4".to_owned()],
                all: false,
                pool: None,
                until: WaitCondition::Done,
                timeout: Some(5),
                json: true,
            })
        );
        assert_eq!(
            parse(&args(&["wait", "--all"])).unwrap(),
            Command::Wait(WaitArgs {
                workers: Vec::new(),
                all: true,
                pool: None,
                until: WaitCondition::Terminal,
                timeout: None,
                json: false,
            })
        );
    }

    /// `--pool` names the lane a wait acts on, for a named Worker and `--all`
    /// alike - both pass through the same resolution `dispatch` uses.
    #[test]
    fn wait_parser_reads_the_pool_flag() {
        for arguments in [
            args(&["wait", "3", "--pool", "bugs"]),
            args(&["wait", "--all", "--pool", "bugs"]),
        ] {
            let command = parse(&arguments).expect("--pool should parse");
            assert!(
                matches!(
                    command,
                    Command::Wait(WaitArgs {
                        pool: Some(ref name),
                        ..
                    }) if name == "bugs"
                ),
                "{arguments:?}"
            );
        }
    }

    #[test]
    fn wait_rejects_a_bad_request() {
        // No Worker and no --all is a usage error.
        assert!(parse(&args(&["wait"])).is_err());
        // --all and explicit Workers are mutually exclusive.
        assert!(parse(&args(&["wait", "3", "--all"])).is_err());
        // An unknown condition or a non-numeric timeout is rejected.
        assert!(parse(&args(&["wait", "3", "--until", "soon"])).is_err());
        assert!(parse(&args(&["wait", "3", "--timeout", "soon"])).is_err());
        // A missing value is not silently swallowed.
        assert!(parse(&args(&["wait", "3", "--until"])).is_err());
        assert!(parse(&args(&["wait", "3", "--timeout"])).is_err());
        assert!(parse(&args(&["wait", "3", "--pool"])).is_err());
        // An unknown flag is named.
        let error = parse(&args(&["wait", "3", "--wat"])).unwrap_err();
        assert!(error.to_string().contains("--wat"), "{error}");
    }

    #[test]
    fn status_and_pool_list_accept_json_and_reject_other_options() {
        assert_eq!(
            parse(&args(&["status"])).unwrap(),
            Command::Status { json: false }
        );
        assert_eq!(
            parse(&args(&["status", "--json"])).unwrap(),
            Command::Status { json: true }
        );
        assert!(parse(&args(&["status", "--wat"])).is_err());
        assert_eq!(
            parse(&args(&["pool", "list"])).unwrap(),
            Command::Pool(PoolCommand::List { json: false })
        );
        assert_eq!(
            parse(&args(&["pool", "list", "--json"])).unwrap(),
            Command::Pool(PoolCommand::List { json: true })
        );
        assert_eq!(
            parse(&args(&["pool"])).unwrap(),
            Command::Pool(PoolCommand::List { json: false })
        );
        assert!(parse(&args(&["pool", "list", "--wat"])).is_err());
    }

    /// `children` names a parent, or nothing at all: omitted, the command asks
    /// about the session that ran it, which is how a parent asks from its own
    /// shell.
    #[test]
    fn children_takes_one_session_and_json_or_neither() {
        assert_eq!(
            parse(&args(&["children"])).unwrap(),
            Command::Children {
                session: None,
                json: false
            }
        );
        assert_eq!(
            parse(&args(&["children", "ses_x", "--json"])).unwrap(),
            Command::Children {
                session: Some("ses_x".to_owned()),
                json: true
            }
        );
        // A second session and an unknown option are both refused, naming the
        // option when there is one to name.
        assert!(parse(&args(&["children", "ses_a", "ses_b"])).is_err());
        let error = parse(&args(&["children", "--wat"])).unwrap_err();
        assert!(error.to_string().contains("--wat"), "{error}");
    }

    #[test]
    fn pool_create_parses_a_named_declaration() {
        let command = parse(&args(&[
            "pool",
            "create",
            "bugs",
            "--size",
            "2",
            "--profile",
            "fast",
            "--source",
            "local",
            "--label",
            "bug",
            "--ready-status",
            "ready-for-agent",
            "--cap-usd",
            "5.5",
            "--window",
            "week",
            "--on-exceed",
            "warn",
        ]))
        .expect("pool create parses");
        assert_eq!(
            command,
            Command::Pool(PoolCommand::Create(PoolCreateArgs {
                name: "bugs".to_owned(),
                size: Some("2".to_owned()),
                profile: Some("fast".to_owned()),
                source: Some("local".to_owned()),
                label: Some("bug".to_owned()),
                ready_status: Some("ready-for-agent".to_owned()),
                cap_usd: Some("5.5".to_owned()),
                window: Some("week".to_owned()),
                on_exceed: Some("warn".to_owned()),
            }))
        );
        // `pool c` is the same verb, and a bare create is a bare declaration.
        assert_eq!(
            parse(&args(&["pool", "c", "features"])).unwrap(),
            Command::Pool(PoolCommand::Create(PoolCreateArgs {
                name: "features".to_owned(),
                ..PoolCreateArgs::default()
            }))
        );
    }

    #[test]
    fn pool_create_requires_a_name_and_rejects_unknown_options() {
        assert!(parse(&args(&["pool", "create"])).is_err());
        assert!(parse(&args(&["pool", "create", "--size", "2"])).is_err());
        assert!(parse(&args(&["pool", "create", "bugs", "--size"])).is_err());
        assert!(parse(&args(&["pool", "create", "bugs", "--wat"])).is_err());
        assert!(parse(&args(&["pool", "create", "bugs", "extra"])).is_err());
    }

    #[test]
    fn pool_destroy_parses_a_name_force_and_defaults_to_default() {
        assert_eq!(
            parse(&args(&["pool", "destroy"])).unwrap(),
            Command::Pool(PoolCommand::Destroy {
                name: None,
                force: false
            })
        );
        assert_eq!(
            parse(&args(&["pool", "destroy", "bugs"])).unwrap(),
            Command::Pool(PoolCommand::Destroy {
                name: Some("bugs".to_owned()),
                force: false
            })
        );
        // `--force` is accepted before or after the name, and has a short form.
        for arguments in [
            vec!["pool", "destroy", "bugs", "--force"],
            vec!["pool", "destroy", "--force", "bugs"],
            vec!["pool", "destroy", "bugs", "-f"],
        ] {
            assert_eq!(
                parse(&args(&arguments)).unwrap(),
                Command::Pool(PoolCommand::Destroy {
                    name: Some("bugs".to_owned()),
                    force: true
                }),
                "{arguments:?}"
            );
        }
        assert!(parse(&args(&["pool", "destroy", "bugs", "extra"])).is_err());
        assert!(parse(&args(&["pool", "destroy", "--wat"])).is_err());
    }

    #[test]
    fn pool_resize_takes_an_optional_pool() {
        for (arguments, pool) in [
            (vec!["pool", "3"], None),
            (vec!["pool", "resize", "3"], None),
            (vec!["pool", "r", "3"], None),
            (vec!["pool", "resize", "3", "--pool", "bugs"], Some("bugs")),
            (vec!["pool", "r", "--pool", "bugs", "3"], Some("bugs")),
        ] {
            assert_eq!(
                parse(&args(&arguments)).unwrap(),
                Command::Pool(PoolCommand::Resize {
                    size: "3".to_owned(),
                    pool: pool.map(str::to_owned),
                }),
                "{arguments:?}"
            );
        }
        assert!(parse(&args(&["pool", "resize"])).is_err());
    }

    /// A temporary Repository with a `.jj` marker and an optional
    /// `.jjfx/config.toml`, for the pool lifecycle tests.
    fn lifecycle_repo(config: Option<&str>) -> tempfile::TempDir {
        let repo = tempfile::tempdir().expect("temporary repository");
        std::fs::create_dir_all(repo.path().join(".jj")).expect("repository marker");
        if let Some(text) = config {
            let directory = repo.path().join(".jjfx");
            std::fs::create_dir_all(&directory).expect("creating .jjfx");
            std::fs::write(directory.join("config.toml"), text).expect("writing the config");
        }
        repo
    }

    /// The repository config text a lifecycle test wrote.
    fn lifecycle_config(repository: &Repository) -> String {
        std::fs::read_to_string(crate::config::repo_config_path(repository.root()))
            .expect("the config file was written")
    }

    #[test]
    fn create_pool_declares_the_pool_and_refuses_a_duplicate() {
        let repo = lifecycle_repo(None);
        let repository = Repository::open(repo.path()).expect("repository");
        let user = toml::from_str::<Config>("[profiles.fast]\nruntime = \"claude\"\n")
            .expect("user config");
        let args = PoolCreateArgs {
            name: "bugs".to_owned(),
            size: Some("2".to_owned()),
            profile: Some("fast".to_owned()),
            ..PoolCreateArgs::default()
        };
        create_pool(&repository, &args, &user).expect("the pool is declared");

        let text = lifecycle_config(&repository);
        assert!(text.contains("[pools.bugs]"), "{text}");
        assert!(text.contains("size = 2"), "{text}");
        assert!(text.contains("profile = \"fast\""), "{text}");

        // A second create of the same pool is refused rather than silently
        // replacing a lane's settings.
        let error = create_pool(&repository, &args, &user).expect_err("a duplicate is refused");
        assert!(error.to_string().contains("bugs"), "{error}");
    }

    /// `pool create <N>` used to be a synonym for `pool <N>`, so a numeric name
    /// must fail loudly rather than silently declare a pool called `3`.
    #[test]
    fn create_pool_refuses_a_numeric_name_that_used_to_mean_resize() {
        let repo = lifecycle_repo(None);
        let repository = Repository::open(repo.path()).expect("repository");
        let user = toml::from_str::<Config>("").expect("user config");
        let args = PoolCreateArgs {
            name: "3".to_owned(),
            ..PoolCreateArgs::default()
        };
        let error = create_pool(&repository, &args, &user).expect_err("a numeric name is refused");
        assert!(error.to_string().contains("pool 3"), "{error}");
        assert!(
            !crate::config::repo_config_path(repository.root()).exists(),
            "the refusal writes nothing"
        );
    }

    #[test]
    fn destroy_pool_removes_the_declaration_but_never_default() {
        let repo = lifecycle_repo(Some("[pools.bugs]\nsize = 2\n"));
        let repository = Repository::open(repo.path()).expect("repository");
        let user = toml::from_str::<Config>("").expect("user config");

        // `default` is not removable, and both spellings say why.
        let error = destroy_pool_command(&repository, Some("default"), false)
            .expect_err("default is refused");
        let message = error.to_string();
        assert!(message.contains("default"), "{message}");
        assert!(message.contains("Resize"), "{message}");
        let bare = destroy_pool_command(&repository, None, false)
            .expect_err("a bare destroy targets default");
        assert!(bare.to_string().contains("default"), "{bare}");

        // The declaration was the file's only content, so removing it leaves no
        // empty file behind.
        destroy_pool(&repository, "bugs", false, &user).expect("the pool is removed");
        assert!(
            !crate::config::repo_config_path(repository.root()).exists(),
            "the emptied config file is gone"
        );

        // Removing a pool that is not declared names it.
        let error = destroy_pool(&repository, "ghost", false, &user).expect_err("no such pool");
        assert!(error.to_string().contains("ghost"), "{error}");
    }

    /// The pool state a removal test reads: `[pools.<name>]` declared in the
    /// repo config, with the pool's own state directory under `.jj/pools/`.
    fn declare_pool_with_state(repo: &tempfile::TempDir, pool: &str, workers: &[(&str, &str)]) {
        let directory = repo.path().join(".jjfx");
        std::fs::create_dir_all(&directory).expect("creating .jjfx");
        std::fs::write(
            directory.join("config.toml"),
            format!("[pools.{pool}]\nsize = {}\n", workers.len()),
        )
        .expect("writing the config");
        let pool_directory = repo.path().join(".jj/pools").join(pool);
        std::fs::create_dir_all(&pool_directory).expect("creating the pool directory");
        let members = workers
            .iter()
            .map(|(worker, _)| format!("\"{worker}\""))
            .collect::<Vec<_>>()
            .join(", ");
        std::fs::write(
            pool_directory.join("pool.json"),
            format!(
                "{{\"created_at\":\"2026-07-27T10:00:00Z\",\"gh_repo\":\"\",\"size\":{},\
                 \"workers\":[{members}]}}",
                workers.len()
            ),
        )
        .expect("writing the pool state");
        for (worker, status) in workers {
            std::fs::write(
                pool_directory.join(format!("{worker}.json")),
                format!(
                    "{{\"agent\":\"claude\",\"branch_name\":\"eng-101\",\"completed_at\":null,\
                     \"error\":null,\"exit_code\":null,\"log_file\":null,\"pid\":null,\
                     \"started_at\":null,\"status\":\"{status}\",\"ticket\":\"ENG-101\"}}"
                ),
            )
            .expect("writing the worker state");
        }
    }

    /// A drain refuses a busy Worker and names it, so a removal can never
    /// silently kill work in progress.
    #[test]
    fn destroy_pool_refuses_a_busy_pool_and_names_the_workers() {
        let repo = lifecycle_repo(None);
        declare_pool_with_state(
            &repo,
            "bugs",
            &[("worker-01", "busy"), ("worker-02", "idle")],
        );
        let repository = Repository::open(repo.path()).expect("repository");
        let user = toml::from_str::<Config>("").expect("user config");

        let error =
            destroy_pool(&repository, "bugs", false, &user).expect_err("a busy pool is refused");
        let message = error.to_string();
        assert!(message.contains("worker-01"), "{message}");
        assert!(message.contains("busy"), "{message}");
        assert!(
            !message.contains("worker-02"),
            "only the busy Worker is named: {message}"
        );
        // The refusal is a drain, not a partial removal: nothing moved.
        assert!(repo.path().join(".jj/pools/bugs/pool.json").exists());
        assert!(
            crate::config::repo_config_path(repository.root()).exists(),
            "the declaration survives a refusal"
        );
    }

    /// A Worker holding work that is neither pushed nor merged is refused unless
    /// forced, and the refusal enumerates the branch.
    #[test]
    fn pool_removal_refuses_undelivered_work_unless_forced() {
        let plan = PoolRemovalPlan {
            busy: Vec::new(),
            workers: vec![
                PoolRemovalWorker {
                    worker: WorkerId::parse("worker-01").expect("worker id"),
                    branch: Some("eng-101".to_owned()),
                    state: WorkState::Merged,
                },
                PoolRemovalWorker {
                    worker: WorkerId::parse("worker-02").expect("worker id"),
                    branch: Some("eng-102".to_owned()),
                    state: WorkState::Dirty {
                        added: 12,
                        removed: 4,
                    },
                },
            ],
        };

        let error = plan
            .refuse_unless_forced("bugs", false)
            .expect_err("undelivered work is refused");
        let message = error.to_string();
        assert!(message.contains("worker-02"), "{message}");
        assert!(message.contains("eng-102"), "{message}");
        assert!(message.contains("dirty +12/-4"), "{message}");
        assert!(
            message.contains("eng-101"),
            "the whole report is shown: {message}"
        );
        assert!(message.contains("merged"), "{message}");

        // `--force` is the only path that discards it, and passes the drain.
        plan.refuse_unless_forced("bugs", true)
            .expect("--force overrides the drain");
    }

    /// A branch state that could not be read refuses too - "could not tell" and
    /// "safe to discard" are different facts - and the refusal says which.
    #[test]
    fn pool_removal_refuses_an_unreadable_branch_and_says_so() {
        let plan = PoolRemovalPlan {
            busy: Vec::new(),
            workers: vec![PoolRemovalWorker {
                worker: WorkerId::parse("worker-01").expect("worker id"),
                branch: Some("eng-101".to_owned()),
                state: WorkState::Unknown,
            }],
        };

        let error = plan
            .refuse_unless_forced("bugs", false)
            .expect_err("an unreadable branch is refused");
        let message = error.to_string();
        assert!(message.contains("could not be read"), "{message}");
        assert!(message.contains('?'), "{message}");
        plan.refuse_unless_forced("bugs", true)
            .expect("--force overrides the drain");
    }

    /// A pool whose Workers have all delivered is removed without a refusal.
    #[test]
    fn pool_removal_allows_delivered_work() {
        let plan = PoolRemovalPlan {
            busy: Vec::new(),
            workers: vec![
                PoolRemovalWorker {
                    worker: WorkerId::parse("worker-01").expect("worker id"),
                    branch: Some("eng-101".to_owned()),
                    state: WorkState::Merged,
                },
                PoolRemovalWorker {
                    worker: WorkerId::parse("worker-02").expect("worker id"),
                    branch: Some("eng-102".to_owned()),
                    state: WorkState::Pushed,
                },
            ],
        };
        plan.refuse_unless_forced("bugs", false)
            .expect("delivered work does not block removal");
        assert!(plan.undelivered().next().is_none());
    }

    /// The report enumerates every Worker, its branch and whether that branch is
    /// pushed or merged - including a Worker that never took a branch.
    #[test]
    fn pool_removal_report_enumerates_branches_and_delivery() {
        let plan = PoolRemovalPlan {
            busy: Vec::new(),
            workers: vec![
                PoolRemovalWorker {
                    worker: WorkerId::parse("worker-01").expect("worker id"),
                    branch: Some("eng-101".to_owned()),
                    state: WorkState::Merged,
                },
                PoolRemovalWorker {
                    worker: WorkerId::parse("worker-02").expect("worker id"),
                    branch: None,
                    state: WorkState::Clean,
                },
            ],
        };
        let report = plan.report();
        assert!(report.contains("worker-01"), "{report}");
        assert!(report.contains("eng-101"), "{report}");
        assert!(report.contains("merged"), "{report}");
        assert!(report.contains("worker-02"), "{report}");
        assert!(report.contains("clean"), "{report}");
    }

    #[test]
    fn resizing_a_named_pool_edits_its_declaration() {
        let repo = lifecycle_repo(Some("[pools.bugs]\nsize = 2\nlabel = \"bug\"\n"));
        let repository = Repository::open(repo.path()).expect("repository");
        let user = toml::from_str::<Config>("").expect("user config");

        resize_declared_pool(&repository, "bugs", 5, &user).expect("the size is set");
        let text = lifecycle_config(&repository);
        assert!(text.contains("size = 5"), "{text}");
        assert!(text.contains("label = \"bug\""), "{text}");

        let error = resize_declared_pool(&repository, "ghost", 5, &user)
            .expect_err("an undeclared pool is refused");
        assert!(error.to_string().contains("ghost"), "{error}");
    }

    /// The report `status`/`pool list` builds for `repository`, with `user` as
    /// the user-global config.
    fn report_for(repository: &Repository, user: &Config) -> PoolReport {
        let repo_config = user
            .repo_config(repository.root())
            .expect("the config parses");
        let tickets = user
            .tickets_for(repository.root())
            .expect("the tickets config parses");
        pool_report(user, &repo_config, repository, &tickets).expect("pool report")
    }

    #[test]
    fn pool_report_names_every_pool_including_an_implicit_default() {
        let repo = lifecycle_repo(Some("[pools.bugs]\nsize = 2\n"));
        let repository = Repository::open(repo.path()).expect("repository");
        let user = toml::from_str::<Config>("").expect("user config");

        let report = report_for(&repository, &user);
        assert_eq!(report.pools.len(), 2, "`default` plus the declared pool");
        assert_eq!(report.pools[0].name, DEFAULT_POOL_NAME);
        assert!(report.pools[0].implicit, "nothing declares `default`");
        assert_eq!(report.pools[1].name, "bugs");
        assert!(!report.pools[1].implicit);
        assert_eq!(report.pools[1].size, Some(2));

        // `pool list --json` names every pool too, with its spend against its cap.
        let json = pool_list_json_string(&report, Timestamp::now(), StuckWindow::DEFAULT);
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(value["pools"][0]["name"], "default");
        assert_eq!(value["pools"][0]["implicit"], true);
        assert_eq!(value["pools"][1]["name"], "bugs");
        assert_eq!(value["pools"][1]["size"], 2);
        assert_eq!(value["pools"][1]["implicit"], false);
        assert_eq!(value["pools"][1]["cap"], serde_json::Value::Null);
        assert_eq!(value["pools"][1]["window"], "week");
        assert_eq!(value["pools"][1]["spend"], 0.0);
        assert_eq!(value["orphans"], serde_json::json!([]));
    }

    #[test]
    fn a_declared_default_is_named_but_no_longer_implicit() {
        let repo = lifecycle_repo(Some("[pools.default]\nprofile = \"fast\"\n"));
        let repository = Repository::open(repo.path()).expect("repository");
        let user = toml::from_str::<Config>("[profiles.fast]\nruntime = \"claude\"\n")
            .expect("user config");

        let report = report_for(&repository, &user);
        assert_eq!(report.pools.len(), 1);
        assert_eq!(report.pools[0].name, DEFAULT_POOL_NAME);
        assert!(!report.pools[0].implicit, "a declaration names it");
    }

    struct FailingQuery;

    impl TicketQuery for FailingQuery {
        fn query(
            &self,
            _request: &crate::engine::TicketQueryRequest,
        ) -> Result<String, crate::engine::TicketQueryError> {
            Err(crate::engine::TicketQueryError::permanent(
                "no Linear tools in this session",
            ))
        }
    }

    #[test]
    fn fetch_ticket_details_aborts_with_context() {
        let discovery = TicketDiscovery::new(FailingQuery);
        let id = TicketId::parse("AMBA-42").expect("Ticket ID");

        let error =
            fetch_ticket_details(&discovery, &id, "ensure the pool runtime can reach Linear")
                .expect_err("a runtime that cannot fetch the Ticket must abort Dispatch");
        let message = error.to_string();
        assert!(message.contains("AMBA-42"), "{message}");
        assert!(message.contains("Linear"), "{message}");
    }

    #[test]
    fn the_details_hint_names_the_local_root_rather_than_linear() {
        let directory = tempfile::tempdir().expect("temporary repository");
        std::fs::create_dir(directory.path().join(".jj")).expect("repository marker");
        let repository = Repository::open(directory.path()).expect("open repository");
        let local = TicketsConfig {
            source: TicketSource::Local,
            ..TicketsConfig::default()
        };

        let hint = ticket_source_hint(&local, &repository);

        assert!(hint.contains(".scratch"), "{hint}");
        assert!(!hint.contains("Linear"), "{hint}");
        let linear = TicketsConfig::default();
        assert!(
            ticket_source_hint(&linear, &repository).contains("Linear"),
            "the default source still names Linear"
        );
    }

    #[test]
    fn json_string_escapes_and_options_render_null() {
        assert_eq!(json_string("plain"), "\"plain\"");
        assert_eq!(json_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(json_string("line\nbreak"), "\"line\\nbreak\"");
        assert_eq!(json_string("\u{1}"), "\"\\u0001\"");
        assert_eq!(json_option(None), "null");
        assert_eq!(json_option(Some("x")), "\"x\"");
    }

    /// A one-Worker Pool snapshot whose waiting set is `waiting`, as a capped
    /// Dispatch would have persisted it to `.jj/pools/default/waiting.json`.
    fn snapshot_with_waiting(waiting: &[&str]) -> crate::engine::WorkerPoolSnapshot {
        let temp = tempfile::tempdir().expect("temp repository");
        let pool = temp.path().join(".jj/pools/default");
        std::fs::create_dir_all(&pool).expect("pool directory");
        std::fs::write(
            temp.path().join(".jj/pools/default/pool.json"),
            br#"{"size":1,"gh_repo":"Jarvvski/jjfx","workers":["worker-01"],"created_at":"2026-07-27T10:00:00Z","names":{}}"#,
        )
        .expect("pool state");
        std::fs::write(
            pool.join("worker-01.json"),
            br#"{"status":"idle","agent":"claude","ticket":null,"pid":null,"started_at":null,"completed_at":null,"log_file":null,"branch_name":null,"exit_code":null,"error":null}"#,
        )
        .expect("worker state");
        if !waiting.is_empty() {
            let tickets = waiting
                .iter()
                .map(|ticket| json_string(ticket))
                .collect::<Vec<_>>()
                .join(", ");
            std::fs::write(
                pool.join("waiting.json"),
                format!("{{\"tickets\":[{tickets}]}}"),
            )
            .expect("waiting state");
        }
        crate::engine::Repository::open(temp.path())
            .expect("repository")
            .read_worker_pool_snapshot()
    }

    /// A one-Worker Pool snapshot whose durable queue holds `queue`, the JSON
    /// body a Dispatch would have written to `.jj/pools/default/queue.json`.
    fn snapshot_with_queue(queue: &str) -> crate::engine::WorkerPoolSnapshot {
        let temp = tempfile::tempdir().expect("temp repository");
        let pool = temp.path().join(".jj/pools/default");
        std::fs::create_dir_all(&pool).expect("pool directory");
        std::fs::write(
            temp.path().join(".jj/pools/default/pool.json"),
            br#"{"size":1,"gh_repo":"Jarvvski/jjfx","workers":["worker-01"],"created_at":"2026-07-27T10:00:00Z","names":{}}"#,
        )
        .expect("pool state");
        std::fs::write(
            pool.join("worker-01.json"),
            br#"{"status":"idle","agent":"claude","ticket":null,"pid":null,"started_at":null,"completed_at":null,"log_file":null,"branch_name":null,"exit_code":null,"error":null}"#,
        )
        .expect("worker state");
        std::fs::write(pool.join("queue.json"), queue).expect("queue state");
        crate::engine::Repository::open(temp.path())
            .expect("repository")
            .read_worker_pool_snapshot()
    }

    /// The durable queue is structure a script can read: one entry per step, with
    /// stable field names for what it is, when it was claimed, by what, and how it
    /// ended - tagged with the pool whose queue holds it, as `waiting` is.
    #[test]
    fn status_json_carries_the_durable_queue_tagged_with_its_pool() {
        let report = report_with_snapshot(snapshot_with_queue(
            r#"{"items":[{"step":"dispatch:AMBA-42","kind":"dispatch","subject":"AMBA-42","state":"in_flight","sequence":0,"queued_at":"2026-07-27T10:00:00Z","claimed_at":"2026-07-27T10:00:01Z","claimed_by":"worker-01","started_at":"2026-07-27T10:00:02Z","in_flight":{"worker":"worker-01","branch":"amba-42","runtime":"opencode","session":"ses_1"}}]}"#,
        ));
        let json = pool_list_json_string(&report, Timestamp::now(), StuckWindow::DEFAULT);
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

        assert_eq!(
            value["queue"],
            serde_json::json!([{
                "step": "dispatch:AMBA-42",
                "kind": "dispatch",
                "subject": "AMBA-42",
                "state": "in_flight",
                "pool": "default",
                "sequence": 0,
                "queued_at": "2026-07-27T10:00:00Z",
                "claimed_at": "2026-07-27T10:00:01Z",
                "claimed_by": "worker-01",
                "started_at": "2026-07-27T10:00:02Z",
                "finished_at": null,
                "detail": null,
                "in_flight": {
                    "worker": "worker-01",
                    "workspace": null,
                    "branch": "amba-42",
                    "runtime": "opencode",
                    "provider": null,
                    "model": null,
                    "session": "ses_1",
                    "pid": null
                }
            }])
        );
    }

    /// A Repository that has never queued anything emits an empty array rather
    /// than omitting the key, so a caller never has to tell "no queue" from
    /// "this jjfx does not have one".
    #[test]
    fn status_json_queue_is_empty_without_a_queue() {
        let report = report_with_snapshot(snapshot_with_waiting(&[]));
        let json = pool_list_json_string(&report, Timestamp::now(), StuckWindow::DEFAULT);
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

        assert_eq!(value["queue"], serde_json::json!([]));
    }

    /// A one-pool report from a `default`-pool snapshot, for the JSON tests.
    fn report_with_snapshot(snapshot: WorkerPoolSnapshot) -> PoolReport {
        let cost = snapshot.cost().cloned();
        let members = snapshot.workers().to_vec();
        let waiting = snapshot.waiting().to_vec();
        let queue = snapshot.queue().to_vec();
        let profile = snapshot.pool().and_then(|pool| pool.profile()).cloned();
        let size = snapshot.pool().map(|pool| pool.size() as u64);
        let workers = snapshot.pool().map(|_| members.len());
        let running = members
            .iter()
            .filter(|worker| worker.status() == WorkerStatus::Busy)
            .count();
        let held = waiting.len();
        PoolReport {
            pools: vec![PoolStatus {
                name: DEFAULT_POOL_NAME.to_owned(),
                implicit: true,
                size,
                workers,
                cap: None,
                window: SpendWindow::Week,
                cost: cost.clone(),
                members,
                waiting,
                queue,
                profile,
            }],
            total_cost: cost,
            limit: None,
            running,
            held,
            orphans: Vec::new(),
            diagnostics: Vec::new(),
            failures: Vec::new(),
        }
    }

    #[test]
    fn status_json_carries_the_waiting_set_tagged_with_its_pool() {
        let held = report_with_snapshot(snapshot_with_waiting(&["AMBA-42", "AMBA-43"]));
        let json = pool_list_json_string(&held, Timestamp::now(), StuckWindow::DEFAULT);
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(
            value["waiting"],
            serde_json::json!([
                { "ticket": "AMBA-42", "pool": "default" },
                { "ticket": "AMBA-43", "pool": "default" }
            ])
        );
        assert_eq!(value["workers"][0]["worker"], "worker-01");
        assert_eq!(value["workers"][0]["pool"], "default");
    }

    #[test]
    fn status_json_waiting_is_empty_without_a_capped_dispatch() {
        let idle = report_with_snapshot(snapshot_with_waiting(&[]));
        let json = pool_list_json_string(&idle, Timestamp::now(), StuckWindow::DEFAULT);
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(value["waiting"], serde_json::json!([]));
    }

    /// Seeds `.jj/pools/<pool>/` with a one-Worker idle pool, so a report has
    /// materialised state to read.
    fn seed_pool(root: &std::path::Path, pool: &str, worker: &str) {
        let directory = root.join(".jj/pools").join(pool);
        std::fs::create_dir_all(&directory).expect("pool directory");
        std::fs::write(
            directory.join("pool.json"),
            format!(
                r#"{{"size":1,"gh_repo":"Jarvvski/jjfx","workers":["{worker}"],"created_at":"2026-07-27T10:00:00Z","names":{{}}}}"#
            ),
        )
        .expect("pool state");
        std::fs::write(
            directory.join(format!("{worker}.json")),
            br#"{"status":"idle","agent":"claude","ticket":null,"pid":null,"started_at":null,"completed_at":null,"log_file":null,"branch_name":null,"exit_code":null,"error":null}"#,
        )
        .expect("worker state");
    }

    /// The documented way to filter a multi-pool `status --json`: the flat
    /// `workers[]` carries `pool`, so `jq '.workers[] | select(.pool == "bugs")'`
    /// answers the per-pool question without a nested shape - and a stray pool
    /// directory is named rather than adopted.
    #[test]
    fn status_json_workers_filter_by_pool_the_documented_way() {
        let repo = lifecycle_repo(Some("[pools.bugs]\nsize = 1\ncap_usd = 5.0\n"));
        seed_pool(repo.path(), DEFAULT_POOL_NAME, "worker-01");
        seed_pool(repo.path(), "bugs", "worker-02");
        // A directory no declaration claims: reported, never adopted.
        seed_pool(repo.path(), "stray", "worker-03");
        let repository = Repository::open(repo.path()).expect("repository");
        let user = toml::from_str::<Config>("").expect("user config");

        let report = report_for(&repository, &user);
        let json = pool_list_json_string(&report, Timestamp::now(), StuckWindow::DEFAULT);
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

        // `jq '.workers[] | select(.pool == "bugs")'`.
        let bugs = value["workers"]
            .as_array()
            .expect("workers array")
            .iter()
            .filter(|worker| worker["pool"] == "bugs")
            .collect::<Vec<_>>();
        assert_eq!(bugs.len(), 1, "{json}");
        assert_eq!(bugs[0]["worker"], "worker-02");
        // Every Worker names its pool, and the stray pool's Worker is not one.
        assert!(
            value["workers"]
                .as_array()
                .expect("workers")
                .iter()
                .all(|worker| worker["pool"] == "default" || worker["pool"] == "bugs"),
            "{json}"
        );

        // The per-pool subtotal states its own size, cap and window.
        assert_eq!(value["pools"][1]["name"], "bugs");
        assert_eq!(value["pools"][1]["size"], 1);
        assert_eq!(value["pools"][1]["cap"], 5.0);
        assert_eq!(value["pools"][1]["window"], "week");
        assert_eq!(value["pools"][1]["spend"], 0.0);
        // `default` sets no cap, so it reports `null` against the ceiling.
        assert_eq!(value["pools"][0]["cap"], serde_json::Value::Null);

        assert_eq!(value["orphans"], serde_json::json!(["stray"]));
    }

    #[test]
    fn a_pool_cap_speaks_in_its_own_window() {
        let repo = lifecycle_repo(Some("[pools.bugs]\ncap_usd = 5.0\nwindow = \"today\"\n"));
        seed_pool(repo.path(), DEFAULT_POOL_NAME, "worker-01");
        seed_pool(repo.path(), "bugs", "worker-02");
        let repository = Repository::open(repo.path()).expect("repository");
        let user = toml::from_str::<Config>("").expect("user config");

        let report = report_for(&repository, &user);
        assert_eq!(report.pools[1].window, SpendWindow::Today);
        assert_eq!(
            report.pools[1].cap.expect("a cap").limit_display(),
            "$5.0000"
        );
        // `default` sets no cap, so it follows the global window.
        assert_eq!(report.pools[0].cap, None);
        assert_eq!(report.pools[0].window, SpendWindow::Week);
    }

    fn ticket(id: &str) -> crate::engine::Ticket {
        crate::engine::Ticket::new(
            TicketId::parse(id).expect("Ticket ID"),
            crate::engine::TicketTitle::parse(id).expect("Ticket title"),
            TicketStatus::parse("Todo").expect("Ticket status"),
        )
    }

    /// A batch exercising all three Direct Dispatch outcomes: a launched Ticket,
    /// a failure, and one the capacity governor held back.
    fn dispatch_entries() -> Vec<DispatchEntry> {
        crate::engine::DirectDispatchResult::new(
            vec![
                DirectDispatchOutcome::Succeeded(crate::engine::DirectDispatchSuccess::new(
                    ticket("ENG-401"),
                    WorkerId::parse("worker-01").expect("Worker ID"),
                    DirectDispatchExecution::Background { pid: 4242 },
                )),
                DirectDispatchOutcome::Failed(crate::engine::DirectDispatchFailure::new(
                    ticket("ENG-402"),
                    Some(WorkerId::parse("worker-02").expect("Worker ID")),
                    DirectDispatchFailurePhase::Workspace,
                    "cannot prepare Workspace",
                )),
            ],
            true,
        )
        .with_waiting(vec![crate::engine::DirectDispatchWaiting::new(
            ticket("ENG-403"),
            "waiting for capacity",
        )])
        .outcomes()
        .iter()
        .map(DispatchEntry::from_outcome)
        .collect()
    }

    #[test]
    fn dispatch_json_names_each_ticket_outcome_worker_and_detail() {
        let value: serde_json::Value =
            serde_json::from_str(&dispatch_json(&dispatch_entries())).expect("valid JSON");

        let entries = value.as_array().expect("one object per Ticket");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["ticket"], "ENG-401");
        assert_eq!(entries[0]["outcome"], "dispatched");
        assert_eq!(entries[0]["worker"], "worker-01");
        assert_eq!(entries[0]["pid"], 4242);
        assert_eq!(entries[0]["detail"], "PID 4242");
        assert_eq!(entries[1]["ticket"], "ENG-402");
        assert_eq!(entries[1]["outcome"], "failed");
        assert_eq!(entries[1]["worker"], "worker-02");
        assert_eq!(entries[1]["phase"], "workspace");
        assert_eq!(entries[1]["detail"], "cannot prepare Workspace");
        assert_eq!(entries[2]["ticket"], "ENG-403");
        assert_eq!(entries[2]["outcome"], "waiting");
        assert!(entries[2]["worker"].is_null());
        assert_eq!(entries[2]["detail"], "waiting for capacity");
    }

    #[test]
    fn dispatch_json_is_an_empty_array_when_no_ticket_was_requested() {
        assert_eq!(dispatch_json(&[]), "[]");
    }

    #[test]
    fn dispatch_json_reports_a_skipped_ticket_with_its_reason() {
        let entries = vec![DispatchEntry::Skipped {
            ticket: "widgets--02-dependent".to_owned(),
            detail: "blocked by widgets--01-foundation (claimed)".to_owned(),
        }];

        let value: serde_json::Value =
            serde_json::from_str(&dispatch_json(&entries)).expect("valid JSON");

        let entries = value.as_array().expect("one object per Ticket");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["ticket"], "widgets--02-dependent");
        assert_eq!(entries[0]["outcome"], "skipped");
        assert!(entries[0]["worker"].is_null());
        assert!(entries[0]["phase"].is_null());
        assert!(entries[0]["pid"].is_null());
        assert_eq!(
            entries[0]["detail"],
            "blocked by widgets--01-foundation (claimed)"
        );
    }

    #[test]
    fn dispatch_json_names_the_search_alongside_the_skipped_tickets() {
        let entries = vec![
            DispatchEntry::Search {
                label: "ready-for-agent".to_owned(),
                root: Some("/repo/.scratch".to_owned()),
                detail: "no ready tickets with label 'ready-for-agent'".to_owned(),
            },
            DispatchEntry::Skipped {
                ticket: "widgets--02-dependent".to_owned(),
                detail: "blocked by widgets--01-foundation (claimed)".to_owned(),
            },
        ];

        let value: serde_json::Value =
            serde_json::from_str(&dispatch_json(&entries)).expect("valid JSON");

        let entries = value.as_array().expect("one object per entry");
        assert_eq!(entries.len(), 2);
        let search = &entries[0];
        assert!(search["ticket"].is_null());
        assert_eq!(search["outcome"], "skipped");
        assert_eq!(search["label"], "ready-for-agent");
        assert_eq!(search["root"], "/repo/.scratch");
        assert_eq!(
            search["detail"],
            "no ready tickets with label 'ready-for-agent'"
        );
        // A search with no filesystem root - a Linear source - omits it as null.
        assert!(search["worker"].is_null());
        // The per-Ticket diagnostic keeps its own shape and carries no search.
        assert_eq!(entries[1]["ticket"], "widgets--02-dependent");
        assert_eq!(entries[1]["outcome"], "skipped");
        assert!(entries[1]["label"].is_null());
        assert!(entries[1]["root"].is_null());
    }

    #[test]
    fn dispatch_json_search_root_is_null_for_a_linear_source() {
        let entries = vec![DispatchEntry::Search {
            label: "ready-for-agent".to_owned(),
            root: None,
            detail: "no ready tickets with label 'ready-for-agent'".to_owned(),
        }];

        let value: serde_json::Value =
            serde_json::from_str(&dispatch_json(&entries)).expect("valid JSON");

        assert!(value[0]["root"].is_null());
        assert_eq!(value[0]["label"], "ready-for-agent");
    }

    #[test]
    fn dispatch_prose_is_unchanged_by_the_shared_model() {
        assert_eq!(
            dispatch_prose(&dispatch_entries()),
            concat!(
                "  worker-01 (PID 4242) -> ENG-401\n",
                "  ENG-402 failed at Workspace: cannot prepare Workspace\n",
                "  ENG-403 waiting: waiting for capacity\n",
            )
        );
    }

    fn failed_entry(ticket: &str) -> DispatchEntry {
        DispatchEntry::Failed {
            ticket: ticket.to_owned(),
            worker: None,
            phase: DirectDispatchFailurePhase::Workspace,
            detail: "cannot prepare Workspace".to_owned(),
        }
    }

    fn dispatched_entry() -> DispatchEntry {
        DispatchEntry::Dispatched {
            ticket: "ENG-1".to_owned(),
            worker: "worker-01".to_owned(),
            pid: Some(4242),
            detail: "PID 4242".to_owned(),
        }
    }

    fn waiting_entry() -> DispatchEntry {
        DispatchEntry::Waiting {
            ticket: "ENG-2".to_owned(),
            detail: "held back by the concurrency cap (dispatch.max_concurrent = 1): 1 Run(s) in \
                     flight, so a Run must finish before another may start"
                .to_owned(),
        }
    }

    #[test]
    fn an_all_failed_dispatch_is_an_error_but_a_dispatched_or_held_batch_is_not() {
        // Nothing requested is not "everything failed".
        assert!(ensure_dispatch_progressed(&[]).is_ok());

        // One dispatched or deliberately held-back Ticket is a success.
        assert!(ensure_dispatch_progressed(&[dispatched_entry()]).is_ok());
        assert!(ensure_dispatch_progressed(&[waiting_entry()]).is_ok());
        assert!(ensure_dispatch_progressed(&[failed_entry("ENG-1"), waiting_entry()]).is_ok());

        // Every Ticket failing is the case a scripting caller must be able to
        // tell apart from success, and the message names the Tickets.
        let error = ensure_dispatch_progressed(&[failed_entry("ENG-1"), failed_entry("ENG-2")])
            .expect_err("every Ticket failed");
        let message = error.to_string();
        assert!(message.contains("ENG-1"), "{message}");
        assert!(message.contains("ENG-2"), "{message}");
    }

    #[test]
    fn a_background_orchestration_names_its_log_in_json_and_prose() {
        let entries = [DispatchEntry::Orchestrating {
            ticket: "ENG-100".to_owned(),
            detail: "log: /repo/.jj/pools/default/orchestrate-ENG-100.log".to_owned(),
        }];

        let value: serde_json::Value =
            serde_json::from_str(&dispatch_json(&entries)).expect("valid JSON");
        assert_eq!(value[0]["outcome"], "orchestrating");
        assert_eq!(
            value[0]["detail"],
            "log: /repo/.jj/pools/default/orchestrate-ENG-100.log"
        );

        let prose = dispatch_prose(&entries);
        assert!(
            prose.contains("Orchestrating ENG-100 in background"),
            "{prose}"
        );
        assert!(prose.contains("orchestrate-ENG-100.log"), "{prose}");
    }

    /// The other half of the help drift check: every spelling `jjfx help`
    /// answers for must be one the parser accepts. The probe is the bare word,
    /// which a command that needs arguments answers with its usage - only an
    /// unknown word says `Unknown command`.
    #[test]
    fn the_parser_accepts_every_documented_alias() {
        for (alias, _) in crate::cli_help::ALIASES {
            let arguments = args(&alias.split(' ').collect::<Vec<_>>());
            if let Err(error) = parse(&arguments) {
                assert!(
                    !error.to_string().contains("Unknown command"),
                    "`{alias}` is documented but the parser rejects it: {error}"
                );
            }
        }
    }

    #[test]
    fn pool_help_forms_all_parse_to_help() {
        for form in ["help", "--help", "-h"] {
            assert_eq!(
                parse(&args(&["pool", form])).unwrap(),
                Command::Pool(PoolCommand::Help),
                "pool {form} should print help"
            );
        }
    }

    #[test]
    fn completion_parser_defaults_to_zsh_and_keeps_hidden_commands_private() {
        assert_eq!(
            parse(&args(&["completion"])).unwrap(),
            Command::Completion {
                shell: "zsh".to_owned(),
            }
        );
        assert!(crate::cli_help::top_level().contains("completion [zsh]"));
        assert!(!crate::cli_help::top_level().contains("__complete"));
    }

    #[test]
    fn worker_action_aliases_cover_review_logs_mount_rebase_and_open_pr() {
        assert!(matches!(
            parse(&args(&["rev", "worker-1"])),
            Ok(Command::Review { .. })
        ));
        assert!(matches!(
            parse(&args(&["log", "worker-1"])),
            Ok(Command::Logs { .. })
        ));
        assert!(matches!(
            parse(&args(&["m", "worker-1"])),
            Ok(Command::Mount { .. })
        ));
        assert!(matches!(
            parse(&args(&["rb", "worker-1"])),
            Ok(Command::Rebase { .. })
        ));
        assert!(matches!(
            parse(&args(&["ab", "worker-1"])),
            Ok(Command::Absorb { .. })
        ));
        assert!(matches!(
            parse(&args(&["pr", "worker-1"])),
            Ok(Command::OpenPullRequest { .. })
        ));
    }

    /// `deliver` is the one forge path as a shell verb; it takes the Worker and
    /// nothing else.
    #[test]
    fn deliver_parser_takes_a_worker() {
        assert_eq!(
            parse(&args(&["deliver", "worker-1"])).unwrap(),
            Command::Deliver {
                worker: "worker-1".to_owned(),
            }
        );
        assert!(parse(&args(&["deliver"])).is_err());
    }

    #[test]
    fn resolve_parser_takes_a_workspace_and_an_optional_tool() {
        assert_eq!(
            parse(&args(&["resolve", "worker-3"])).unwrap(),
            Command::Resolve {
                workspace: "worker-3".to_owned(),
                tool: None,
            }
        );
        assert_eq!(
            parse(&args(&["resolve", "3", "--tool", ":theirs"])).unwrap(),
            Command::Resolve {
                workspace: "3".to_owned(),
                tool: Some(":theirs".to_owned()),
            }
        );
        // The workspace is required: a bare `resolve` says how to call it.
        assert!(parse(&args(&["resolve"])).is_err());
        assert!(parse(&args(&["resolve", "--tool"])).is_err());
        assert!(parse(&args(&["resolve", "--nope"])).is_err());
        assert!(parse(&args(&["resolve", "a", "b"])).is_err());
    }

    #[test]
    fn undo_parser_takes_an_optional_workspace_and_force() {
        assert_eq!(
            parse(&args(&["undo"])).unwrap(),
            Command::Undo {
                workspace: None,
                force: false,
            }
        );
        assert_eq!(
            parse(&args(&["undo", "worker-3"])).unwrap(),
            Command::Undo {
                workspace: Some("worker-3".to_owned()),
                force: false,
            }
        );
        assert_eq!(
            parse(&args(&["undo", "*", "--force"])).unwrap(),
            Command::Undo {
                workspace: Some("*".to_owned()),
                force: true,
            }
        );
        assert!(parse(&args(&["undo", "--nope"])).is_err());
        assert!(parse(&args(&["undo", "a", "b"])).is_err());
    }

    #[test]
    fn undo_workspace_key_reaches_a_worker_and_an_ad_hoc_workspace() {
        let root = crate::store::test_local_repo("cli-undo-key");
        let repository = Repository::open(&root).expect("repository");

        // Nothing recorded yet: a short Worker id resolves to its Workspace name.
        assert_eq!(undo_workspace_key(&repository, "3").unwrap(), "worker-3");
        assert_eq!(
            undo_workspace_key(&repository, "repo").unwrap(),
            REPOSITORY_WIDE_MUTATION
        );

        // A recorded name is used as it is, so an ad-hoc Workspace - which has no
        // `worker-` prefix and may not even be a valid Worker id - is reachable.
        repository
            .state_store()
            .undo()
            .record(&crate::engine::MutationRecord {
                workspace: "my work".to_owned(),
                kind: crate::engine::MutationKind::Weld,
                before: "before".to_owned(),
                after: "after".to_owned(),
                at: crate::engine::WireTimestamp::new("2026-10-15T00:00:00Z"),
            })
            .unwrap();
        assert_eq!(
            undo_workspace_key(&repository, "my work").unwrap(),
            "my work"
        );
        assert_eq!(
            undo_workspace_key(&repository, "worker-3").unwrap(),
            "worker-3"
        );
    }

    #[test]
    fn send_parser_preserves_prompt_and_foreground_mode() {
        assert_eq!(
            parse(&args(&["s", "worker-1", "follow up", "--fg"])).unwrap(),
            Command::Send {
                worker: "worker-1".to_owned(),
                prompt: "follow up".to_owned(),
                mode: RunMode::Foreground,
                force: false,
            }
        );
    }

    #[test]
    fn dispatch_and_send_accept_force() {
        let dispatch = parse(&args(&[
            "dispatch",
            "AMBA-42",
            "--no-orchestrate",
            "--force",
        ]))
        .expect("dispatch --force should parse");
        assert!(matches!(
            dispatch,
            Command::Dispatch(DispatchArgs { force: true, .. })
        ));

        let send = parse(&args(&["send", "worker-1", "go", "--force"]))
            .expect("send --force should parse");
        assert!(matches!(send, Command::Send { force: true, .. }));
    }

    #[test]
    fn foreground_and_background_prompt_rendering_preserves_ordered_inputs() {
        assert_eq!(truncate_prompt("é".repeat(80).as_str()).chars().count(), 60);
        assert_eq!(truncate_prompt("first second"), "first second");
    }

    #[test]
    fn capacity_prompt_uses_the_locked_shortage_gap() {
        let shortage = CapacityShortage::new(5, 2);
        assert_eq!(
            growth_question(shortage),
            "Pool has 2 idle worker(s) but 5 ticket(s) to dispatch. Grow pool by 3?"
        );
    }

    /// A prompt that cannot be answered must not default to the expensive
    /// option: only an explicit `y`/`yes` grows the pool, so an empty answer or
    /// an EOF refuses it.
    #[test]
    fn only_an_explicit_yes_grows_the_pool() {
        for answer in ["y", "Y", "yes", "YES", " y \n"] {
            assert!(answer_approves_growth(answer), "`{answer}` should approve");
        }
        for answer in ["", "\n", "n", "no", "nope", "maybe", "y n"] {
            assert!(!answer_approves_growth(answer), "`{answer}` should refuse");
        }
    }

    /// Only an explicit `y` confirms a clean sweep, matching the contract
    /// `clean` has always had: an `n` or an empty answer declines.
    #[test]
    fn only_an_explicit_yes_confirms_a_clean() {
        for answer in ["y", "Y", " y \n"] {
            assert!(answer_confirms_clean(answer), "`{answer}` should confirm");
        }
        for answer in ["", "\n", "n", "no", "nope", "maybe", "y n"] {
            assert!(!answer_confirms_clean(answer), "`{answer}` should decline");
        }
    }

    /// A development build must say so: its version carries a `-dev` suffix, so
    /// a binary built from `main` cannot be mistaken for the release it
    /// followed. A release build reports the clean version.
    #[test]
    fn a_development_build_is_marked_in_the_version() {
        assert_eq!(
            version_string(true),
            concat!(env!("CARGO_PKG_VERSION"), "-dev")
        );
        assert_eq!(version_string(false), env!("CARGO_PKG_VERSION"));
        assert_ne!(version_string(true), version_string(false));
    }

    #[test]
    fn parser_preserves_ready_ticket_bulk_label() {
        let command = parse(&args(&["dispatch", "--all", "--label", "needs-review"]))
            .expect("bulk dispatch arguments should parse");
        assert_eq!(
            command,
            Command::Dispatch(DispatchArgs {
                tickets: Vec::new(),
                all: true,
                mode: RunMode::Background,
                provider: None,
                model: None,
                budget: None,
                label: Some("needs-review".to_owned()),
                pool: None,
                no_orchestrate: false,
                force: false,
                json: false,
                input: None,
            })
        );
    }

    #[test]
    fn parser_accepts_action_aliases() {
        assert_eq!(
            parse(&args(&["d", "AMBA-42", "--no-orchestrate"])).unwrap(),
            Command::Dispatch(DispatchArgs {
                tickets: vec!["AMBA-42".to_owned()],
                all: false,
                mode: RunMode::Background,
                provider: None,
                model: None,
                budget: None,
                label: None,
                pool: None,
                no_orchestrate: true,
                force: false,
                json: false,
                input: None,
            })
        );
        assert_eq!(
            parse(&args(&["rb", "worker-1"])).unwrap(),
            Command::Rebase {
                worker: "worker-1".to_owned(),
            }
        );
        assert_eq!(
            parse(&args(&["absorb", "worker-1"])).unwrap(),
            Command::Absorb {
                worker: "worker-1".to_owned(),
            }
        );
    }

    #[test]
    fn the_spend_summary_totals_the_recorded_runs() {
        let temp = tempfile::tempdir().expect("temp repository");
        std::fs::create_dir(temp.path().join(".jj")).expect("repository marker");
        let repository = Repository::open(temp.path()).expect("repository");
        append_run(&repository, "worker-01", Some(250_000));
        append_run(&repository, "worker-02", Some(750_000));
        let cost = repository.pool_cost().expect("pool cost");

        let summary = spend_summary(Some(&cost));
        assert!(summary.contains("$1.0000 total"), "{summary}");
        assert!(summary.contains("across 2 Run(s)"), "{summary}");
        assert_eq!(worker_spend(Some(&cost), "worker-01"), "$0.2500");
        assert_eq!(worker_spend(Some(&cost), "worker-03"), "-");
        assert_eq!(
            spend_summary(None),
            "Spend: record store unavailable",
            "an unreadable store is unavailable, never $0"
        );
    }

    /// A user config whose Repository-wide ceiling is `cap_usd` in `mode`.
    ///
    /// Parsed from TOML rather than built by struct update: [`Config`] keeps a
    /// private legacy field, so its only public constructors are parsing and
    /// [`Config::default`].
    fn config_with_spend(cap_usd: f64, mode: SpendCapMode) -> Config {
        let text = format!(
            "[spend]\ncap_usd = {cap_usd}\non_exceed = \"{}\"\n",
            mode.as_str()
        );
        toml::from_str(&text).expect("valid spend config")
    }

    /// A repository-local config declaring `default`'s own cap.
    fn repo_config_with_pool_cap(cap_usd: f64, mode: SpendCapMode) -> RepoConfig {
        let declaration = PoolConfig {
            cap_usd: Some(cap_usd),
            on_exceed: Some(mode),
            ..PoolConfig::default()
        };
        RepoConfig {
            pools: [("default".to_owned(), declaration)].into_iter().collect(),
            ..RepoConfig::default()
        }
    }

    #[test]
    fn the_dispatch_spend_gate_allows_warns_and_blocks() {
        let temp = tempfile::tempdir().expect("temp repository");
        std::fs::create_dir(temp.path().join(".jj")).expect("repository marker");
        let repository = Repository::open(temp.path()).expect("repository");
        append_run(&repository, "worker-01", Some(6_000_000));
        let no_pools = RepoConfig::default();

        // No cap configured: Dispatch is never gated.
        assert_eq!(
            spend_gate(&repository, &no_pools, &Config::default()).expect("no cap"),
            SpendDecision::Allowed
        );

        // Under the cap: allowed.
        let under = config_with_spend(10.0, SpendCapMode::Block);
        assert_eq!(
            spend_gate(&repository, &no_pools, &under).expect("under"),
            SpendDecision::Allowed
        );

        // Over the cap in warn mode: a message, and Dispatch proceeds.
        let warn = config_with_spend(5.0, SpendCapMode::Warn);
        let SpendDecision::Warned(warning) =
            spend_gate(&repository, &no_pools, &warn).expect("warn")
        else {
            panic!("expected a warning");
        };
        assert!(warning.contains("dispatching anyway"), "{warning}");

        // Over the cap in block mode: a refusal naming the cap and the override.
        let block = config_with_spend(5.0, SpendCapMode::Block);
        let SpendDecision::Refused(message) =
            spend_gate(&repository, &no_pools, &block).expect("blocked")
        else {
            panic!("expected a refusal");
        };
        assert!(message.contains("Repository"), "{message}");
        assert!(message.contains("spend.cap_usd"), "{message}");
        assert!(message.contains("spend.on_exceed"), "{message}");
    }

    /// A pool's own cap binds on top of the ceiling, and the refusal names the
    /// pool - its own setting is the dial, not the Repository's.
    #[test]
    fn the_dispatch_spend_gate_applies_a_pools_own_cap() {
        let temp = tempfile::tempdir().expect("temp repository");
        std::fs::create_dir(temp.path().join(".jj")).expect("repository marker");
        let repository = Repository::open(temp.path()).expect("repository");
        append_run(&repository, "worker-01", Some(6_000_000));

        let declared = repo_config_with_pool_cap(5.0, SpendCapMode::Block);
        let SpendDecision::Refused(message) =
            spend_gate(&repository, &declared, &Config::default()).expect("pool cap")
        else {
            panic!("expected a refusal");
        };
        assert!(message.contains("pool `default`"), "{message}");
        assert!(message.contains("pools.default.cap_usd"), "{message}");
        assert!(!message.contains("spend.cap_usd"), "{message}");

        // The pool's cap defaults its window and mode to the global ceiling, so
        // a declaration that states only `cap_usd` still gates.
        let global_warn = config_with_spend(50.0, SpendCapMode::Warn);
        let declaration = PoolConfig {
            cap_usd: Some(5.0),
            ..PoolConfig::default()
        };
        let declared = RepoConfig {
            pools: [("default".to_owned(), declaration)].into_iter().collect(),
            ..RepoConfig::default()
        };
        let SpendDecision::Warned(message) =
            spend_gate(&repository, &declared, &global_warn).expect("inherited warn mode")
        else {
            panic!("expected a warning");
        };
        assert!(message.contains("pools.default.on_exceed"), "{message}");
    }

    /// Both limits bind: the tighter pool cap wins when both fire, and the
    /// Repository ceiling wins when only it has run out.
    #[test]
    fn the_dispatch_spend_gate_names_the_most_restrictive_limit() {
        let temp = tempfile::tempdir().expect("temp repository");
        std::fs::create_dir(temp.path().join(".jj")).expect("repository marker");
        let repository = Repository::open(temp.path()).expect("repository");
        // `default` alone records $6, so it is over a $5 cap and the $6 ceiling.
        append_run(&repository, "worker-01", Some(6_000_000));

        // A $5 pool cap under a $10 ceiling: the pool is the tighter dial.
        let pool_tighter = config_with_spend(10.0, SpendCapMode::Block);
        let declared = repo_config_with_pool_cap(5.0, SpendCapMode::Block);
        let SpendDecision::Refused(message) =
            spend_gate(&repository, &declared, &pool_tighter).expect("pool cap")
        else {
            panic!("expected a refusal");
        };
        assert!(message.contains("pool `default`"), "{message}");

        // A $5 ceiling over a $50 pool cap: the ceiling has run out while the
        // pool still has room, so the refusal names the Repository.
        let ceiling_tighter = config_with_spend(5.0, SpendCapMode::Block);
        let declared = repo_config_with_pool_cap(50.0, SpendCapMode::Block);
        let SpendDecision::Refused(message) =
            spend_gate(&repository, &declared, &ceiling_tighter).expect("ceiling")
        else {
            panic!("expected a refusal");
        };
        assert!(message.contains("Repository"), "{message}");
        assert!(message.contains("spend.cap_usd"), "{message}");
    }

    /// Appends one completed Run record to `repository`'s store.
    fn append_run(repository: &Repository, worker: &str, cost_micro_usd: Option<u64>) {
        repository
            .state_store()
            .runs()
            .append(&crate::engine::RunRecord {
                run_id: format!("{worker}@1"),
                worker: crate::engine::WorkerId::parse(worker).expect("worker id"),
                ticket: None,
                workspace: None,
                branch: None,
                runtime: None,
                provider: None,
                model: None,
                cost_micro_usd,
                usage: None,
                duration_ms: None,
                turns: None,
                conclusion: crate::engine::RunRecordConclusion::Succeeded,
                check: None,
                error: None,
                started_at: None,
                completed_at: Some(crate::engine::WireTimestamp::new(
                    jiff::Timestamp::now().to_string(),
                )),
            })
            .expect("append record");
    }
}
