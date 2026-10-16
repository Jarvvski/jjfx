use std::collections::BTreeMap;
use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
const PROGRAM: &str = env!("CARGO_PKG_NAME");

use crate::adhoc_ticket::{AdHocInput, AdHocSource, AdHocTicket};
use crate::cli_help::{
    ADD_FLAGS, DISPATCH_FLAGS, Flag, POOL_LIST_FLAGS, POOL_PROFILE_FLAGS, POOL_RESIZE_FLAGS,
    RESOLVE_FLAGS, REVIEW_FLAGS, RM_FLAGS, SEND_FLAGS, STATUS_FLAGS, UNDO_FLAGS,
};
use crate::config::{MigrationsConfig, SpendConfig, TicketSource, TicketsConfig};
use crate::engine::{
    AgentModel, AgentRuntime, AgentRuntimeProfile, CapacityShortage, CleanDecision,
    DirectDispatchError, DirectDispatchExecution, DirectDispatchFailurePhase,
    DirectDispatchOutcome, DirectDispatchRequest, DirectDispatchSuccess, DispatchBudget,
    DispatchDependencyContext, DispatchExclusions, FollowUpExecution, FollowUpOutcome, Loaded,
    OrchestrationEvent, PoolCapacity, REPOSITORY_WIDE_MUTATION, ReadyTicketFilter, Repository,
    ReviewLoop, RunActivity, RunActivityKind, RunMode, StuckWindow, TerminalSettings,
    TicketDetails, TicketDiscovery, TicketId, TicketQuery, TicketStatus, UndoOutcome, WireAgent,
    WorkerActions, WorkerId, WorkerPoolError, WorkerPoolSnapshot, WorkerStatus,
    WorkspaceAddOutcome, WorkspaceHookStream, dependency_basis, resolve_ticket_branch,
};
use crate::migrations::FindingKind;
use crate::reviewer_source::configured_reviewer;
use crate::ticket_source::{apply_local_writeback, configured_ticket_query};

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
    Clean,
    Root,
    Where,
    Path {
        name: String,
    },
    Refresh,
    Colocation {
        action: ColocationAction,
    },
    Status {
        json: bool,
    },
    Dispatch(DispatchArgs),
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
    Mount {
        worker: String,
    },
    Rebase {
        worker: String,
    },
    Absorb {
        worker: String,
    },
    OpenPullRequest {
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
    },
    Pool(PoolCommand),
    Ticket(TicketCommand),
}

/// The `ticket` subcommands: local Ticket state, reconciled on demand.
#[derive(Debug, PartialEq, Eq)]
pub enum TicketCommand {
    /// `ticket reconcile`: resolve finished local Tickets without a Dispatch.
    Reconcile,
}

/// Which colocation transition the `colocation` command performs.
#[derive(Debug, PartialEq, Eq)]
pub enum ColocationAction {
    Enable,
    Disable,
    Status,
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
    pub label: String,
    pub no_orchestrate: bool,
    pub force: bool,
    pub json: bool,
    /// A Ticket handed in directly (`--text`, `--from-file`, or stdin) rather
    /// than resolved through a configured source. `None` for the ordinary path.
    pub input: Option<AdHocInput>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PoolCommand {
    Resize {
        size: String,
    },
    List {
        json: bool,
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
    Destroy,
    Help,
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
        "clean" => Ok(Command::Clean),
        "root" => Ok(Command::Root),
        "where" | "info" => Ok(Command::Where),
        "path" => parse_single(
            concat!("Usage: ", env!("CARGO_PKG_NAME"), " path <name>"),
            &args[1..],
        )
        .map(|name| Command::Path { name }),
        "refresh" | "sync" => Ok(Command::Refresh),
        "colocation" | "colocate" => parse_colocation(&args[1..]),
        "status" => parse_status(&args[1..]),
        "reset" => parse_single_exact(
            concat!("Usage: ", env!("CARGO_PKG_NAME"), " reset <worker>"),
            &args[1..],
        )
        .map(|worker| Command::Pool(PoolCommand::Reset { worker })),
        "dispatch" | "d" => parse_dispatch(&args[1..]),
        "send" | "s" => parse_send(&args[1..]),
        "review" | "rev" => parse_review(&args[1..]),
        "logs" | "log" => parse_worker_command(
            concat!("Usage: ", env!("CARGO_PKG_NAME"), " logs <worker>"),
            &args[1..],
        )
        .map(|worker| Command::Logs { worker }),
        "mount" | "m" => parse_worker_command(
            concat!("Usage: ", env!("CARGO_PKG_NAME"), " mount <worker>"),
            &args[1..],
        )
        .map(|worker| Command::Mount { worker }),
        "rebase" | "rb" => parse_worker_command(
            concat!("Usage: ", env!("CARGO_PKG_NAME"), " rebase <worker>"),
            &args[1..],
        )
        .map(|worker| Command::Rebase { worker }),
        "absorb" | "ab" => parse_worker_command(
            concat!("Usage: ", env!("CARGO_PKG_NAME"), " absorb <worker>"),
            &args[1..],
        )
        .map(|worker| Command::Absorb { worker }),
        "open-pr" | "pr" => parse_worker_command(
            concat!("Usage: ", env!("CARGO_PKG_NAME"), " open-pr <worker>"),
            &args[1..],
        )
        .map(|worker| Command::OpenPullRequest { worker }),
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
    let usage = concat!("Usage: ", env!("CARGO_PKG_NAME"), " ticket reconcile");
    match args.first().map(String::as_str) {
        Some("reconcile") if args.len() == 1 => Ok(Command::Ticket(TicketCommand::Reconcile)),
        _ => bail!("{usage}"),
    }
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
        "destroy" => Ok(PoolCommand::Destroy.into()),
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
        "create" | "c" | "resize" | "r" => parse_pool_size(&args[1..]),
        value if value.parse::<i64>().is_ok() => parse_pool_size(args),
        unknown => bail!("Unknown pool command: {unknown}"),
    }
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

fn parse_pool_size(args: &[String]) -> Result<Command> {
    reject_unknown_flags("pool resize", POOL_RESIZE_FLAGS, args)?;
    let mut size = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--size" | "-s" => {
                size = args.get(index + 1).cloned();
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
            " pool resize <N>"
        ))
    })?;
    Ok(PoolCommand::Resize { size }.into())
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
    let mut label = "ready-for-agent".to_owned();
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
            "--provider" | "--model" | "--label" | "--budget" => {
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
                    "--label" => label = value.clone(),
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
        no_orchestrate,
        force,
        json,
        input: input.map(|source| AdHocInput { source, id }),
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
            " __orchestrate <PARENT-TICKET> [--provider PROVIDER] [--model MODEL]"
        ));
    };
    let mut provider = None;
    let mut model = None;
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
    /// Where Dispatch discovers Ready Tickets.
    pub tickets: fn() -> Result<TicketsConfig>,
    /// The aggregate spend cap Dispatch is gated on.
    pub spend: fn() -> Result<SpendConfig>,
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
        spend,
        migrations,
    } = settings;
    match parse(&args)? {
        Command::Help { command } => print!("{}", crate::cli_help::text(command.as_deref())?),
        Command::Version => println!("{PROGRAM} {}", env!("CARGO_PKG_VERSION")),
        Command::Default => default_command(launch)?,
        Command::Root => root_command(&repository(colocate)?)?,
        Command::Where => where_command(&repository(colocate)?)?,
        Command::Path { name } => path_command(&repository(colocate)?, &name)?,
        Command::Refresh => refresh_command(&repository(colocate)?)?,
        Command::Colocation { action } => colocation_command(&repository(colocate)?, &action)?,
        Command::Add { name, revision } => {
            add_command(&repository(colocate)?, &name, revision.as_deref())?
        }
        Command::List => list_command(&repository(colocate)?)?,
        Command::Remove { force, names } => remove_command(&repository(colocate)?, force, &names)?,
        Command::Clean => clean_command(&repository(colocate)?)?,
        Command::Status { json } => pool_list_command(&repository(colocate)?, json)?,
        Command::Dispatch(args) => dispatch_command(
            &repository(colocate)?,
            &args,
            open_workspaces,
            tickets,
            spend,
        )?,
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
        Command::Mount { worker } => {
            mount_command(&repository(colocate)?, &worker, terminal_settings)?
        }
        Command::Rebase { worker } => rebase_command(&repository(colocate)?, &worker)?,
        Command::Absorb { worker } => absorb_command(&repository(colocate)?, &worker)?,
        Command::OpenPullRequest { worker } => open_pr_command(&repository(colocate)?, &worker)?,
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
        } => orchestrate_command(
            &repository(colocate)?,
            &parent,
            selected_model(provider.as_deref(), model.as_deref())?,
            force,
            false,
            open_workspaces,
            tickets,
        )?,
        Command::Pool(command) => pool_command(&repository(colocate)?, command)?,
        Command::Ticket(TicketCommand::Reconcile) => {
            ticket_reconcile_command(&repository(colocate)?, tickets)?
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
    Repository::open(".")
        .map(|repository| repository.with_colocate(colocate))
        .map_err(|_| anyhow::anyhow!("Not in a jj repo"))
}

fn print_hook_output(_stream: WorkspaceHookStream, line: &str) {
    if !line.is_empty() {
        eprintln!("{line}");
    }
}

fn root_command(repository: &Repository) -> Result<()> {
    println!("{}", repository.root().display());
    Ok(())
}

fn where_command(repository: &Repository) -> Result<()> {
    println!("repo:       {}", repository.root().display());
    println!(
        "workspaces: {}",
        repository.workspaces().base_dir().display()
    );
    Ok(())
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
    // Dispatch and the Pool list use the short Worker id, while the Workspace
    // is named `worker-<id>`. Resolve either form against the real projection so
    // `path <short-id>` returns the directory that actually exists.
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
fn colocation_command(repository: &Repository, action: &ColocationAction) -> Result<()> {
    let verb = match action {
        ColocationAction::Enable => "enable",
        ColocationAction::Disable => "disable",
        ColocationAction::Status => "status",
    };
    let status = std::process::Command::new("jj")
        .args(["git", "colocation", verb])
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

fn clean_command(repository: &Repository) -> Result<()> {
    let workspaces = repository.workspaces();
    let plan = workspaces.plan_clean()?;
    if plan.entries().is_empty() {
        println!("No workspaces to remove");
        return Ok(());
    }
    println!("Remove {} workspace(s)?", plan.entries().len());
    for entry in plan.entries() {
        println!("  {}", entry.name());
    }
    print!("Confirm? (y/n) ");
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let decision = if input.trim().eq_ignore_ascii_case("y") {
        CleanDecision::Confirmed
    } else {
        CleanDecision::Declined
    };
    workspaces.clean_with_progress(&plan, decision, print_hook_output)?;
    Ok(())
}

fn pool_command(repository: &Repository, command: PoolCommand) -> Result<()> {
    match command {
        PoolCommand::Resize { size } => resize_pool_command(repository, &size),
        PoolCommand::List { json } => pool_list_command(repository, json),
        PoolCommand::Remove { worker } => remove_worker_command(repository, &worker),
        PoolCommand::Reset { worker } => reset_worker_command(repository, &worker),
        PoolCommand::Profile {
            runtime,
            provider,
            model,
        } => pool_profile_command(repository, &runtime, provider.as_deref(), model.as_deref()),
        PoolCommand::Destroy => destroy_pool_command(repository),
        PoolCommand::Help => {
            print!("{}", crate::cli_help::text(Some("pool"))?);
            Ok(())
        }
    }
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

fn resize_pool_command(repository: &Repository, value: &str) -> Result<()> {
    let size = value
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("Invalid pool size: {value}"))?;
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

fn pool_list_command(repository: &Repository, json: bool) -> Result<()> {
    let config = crate::config::load()?;
    let snapshot = repository.worker_pool().reconcile_runs();
    // `status`/`pool list` read Pool state, so they also reconcile local Tickets:
    // a finished Run resolves its Ticket here, with no Dispatch needed.
    report_local_writeback(repository, &snapshot, &config.tickets);
    let pool = snapshot
        .pool()
        .ok_or_else(|| anyhow::anyhow!("No pool. Run: {PROGRAM} pool create --size N"))?;
    // The watchdog's window is jjfx's own setting, read here rather than passed
    // down: `pool list` is the only CLI command that surfaces Run health, and
    // `cli::run` is already at the argument limit.
    let watchdog = config.watchdog.stuck_window();
    let now = Timestamp::now();
    if json {
        return pool_list_json(&snapshot, now, watchdog);
    }
    for diagnostic in snapshot.diagnostics() {
        eprintln!("{}", diagnostic.message());
    }
    println!(
        "{:<10} {:<12} {:<10} {:<14} {:<11} ELAPSED",
        "WORKER", "NAME", "STATUS", "TICKET", "SPEND"
    );
    println!(
        "{:<10} {:<12} {:<10} {:<14} {:<11} -------",
        "------", "----", "------", "------", "-----"
    );
    let mut counts = [0usize; 4];
    let mut stuck = 0usize;
    for worker in snapshot.workers() {
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
        let spend = worker_spend(&snapshot, worker.worker_id().as_str());
        let elapsed = elapsed_display(worker.started_at(), worker.completed_at());
        println!(
            "{short:<10} {name:<12} {:<10} {ticket:<14} {spend:<11} {elapsed}",
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
        // A terminal Worker's reason is only useful when surfaced here: the raw
        // diagnosis lives in its Run log, and the persisted error may be a
        // generic fallback.
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
    println!();
    if let Some(profile) = pool.profile() {
        println!("Profile: {}", render_profile(profile));
    }
    let stuck = if stuck > 0 {
        format!(", {stuck} stuck")
    } else {
        String::new()
    };
    println!(
        "Pool: {} idle, {} busy{}, {} done, {} failed ({} total)",
        counts[0],
        counts[1],
        stuck,
        counts[2],
        counts[3],
        pool.size()
    );
    println!("{}", spend_summary(&snapshot));
    if !snapshot.waiting().is_empty() {
        println!("Waiting: {}", snapshot.waiting().join(", "));
    }
    Ok(())
}

/// One Worker's recorded spend, or `-` when it recorded no Run.
fn worker_spend(snapshot: &crate::engine::WorkerPoolSnapshot, worker: &str) -> String {
    snapshot
        .cost()
        .and_then(|cost| cost.worker(worker))
        .map_or_else(|| "-".to_owned(), crate::engine::PoolCost::cost_display)
}

/// The pool-level cost/usage summary line.
fn spend_summary(snapshot: &crate::engine::WorkerPoolSnapshot) -> String {
    let Some(cost) = snapshot.cost() else {
        return "Spend: record store unavailable".to_owned();
    };
    let total = cost.total();
    let unpriced = if total.unpriced_runs() > 0 {
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

/// Machine-readable Pool status, including the failure fields the table omits.
fn pool_list_json(
    snapshot: &crate::engine::WorkerPoolSnapshot,
    now: Timestamp,
    watchdog: StuckWindow,
) -> Result<()> {
    println!("{}", pool_list_json_string(snapshot, now, watchdog));
    Ok(())
}

/// The Pool status JSON document: `{"workers": [ ... ], "waiting": [ ... ]}`.
///
/// A top-level object rather than the bare Worker array: the human table prints
/// state that is not per-Worker (the `Waiting:` line), and a bare array has
/// nowhere to put it. The wrapper is the seam for that state and for the next
/// line the table grows, so the JSON can always carry what the table prints.
/// Encoded by hand so the emitted field order is fixed.
fn pool_list_json_string(
    snapshot: &crate::engine::WorkerPoolSnapshot,
    now: Timestamp,
    watchdog: StuckWindow,
) -> String {
    let mut out = String::from("{\n  \"workers\": [");
    for (index, worker) in snapshot.workers().iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
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
                snapshot
                    .cost()
                    .and_then(|cost| cost.worker(worker.worker_id().as_str()))
                    .map_or_else(
                        || "null".to_owned(),
                        |cost| format!("{:.6}", cost.cost_usd()),
                    ),
            ),
        ];
        out.push_str("\n    {\n");
        for (field, (key, value)) in fields.iter().enumerate() {
            if field > 0 {
                out.push_str(",\n");
            }
            out.push_str(&format!("      \"{key}\": {value}"));
        }
        out.push_str("\n    }");
    }
    if snapshot.workers().is_empty() {
        out.push_str("],\n");
    } else {
        out.push_str("\n  ],\n");
    }
    out.push_str("  \"waiting\": [");
    for (index, ticket) in snapshot.waiting().iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&json_string(ticket));
    }
    out.push_str("]\n}");
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

fn destroy_pool_command(repository: &Repository) -> Result<()> {
    if repository.worker_pool().snapshot().is_missing() {
        eprintln!("No pool to destroy");
        return Ok(());
    }
    repository
        .worker_pool()
        .destroy_with_progress(print_hook_output)?;
    eprintln!("Pool destroyed");
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
    let snapshot = repository.worker_pool().reconcile_runs();
    report_local_writeback(repository, &snapshot, tickets);
}

/// Reports the failures of one local Ticket writeback against `snapshot`.
fn report_local_writeback(
    repository: &Repository,
    snapshot: &WorkerPoolSnapshot,
    tickets: &TicketsConfig,
) {
    for failure in apply_local_writeback(repository, snapshot, tickets) {
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
    tickets_config: fn() -> Result<TicketsConfig>,
) -> Result<()> {
    let tickets = tickets_config()?;
    if tickets.source != TicketSource::Local {
        eprintln!("Ticket source is not local; nothing to reconcile");
        return Ok(());
    }
    let snapshot = repository.worker_pool().reconcile_runs();
    let failures = apply_local_writeback(repository, &snapshot, &tickets);
    if failures.is_empty() {
        eprintln!("Reconciled local Tickets");
        return Ok(());
    }
    for failure in &failures {
        eprintln!("local Ticket writeback failed: {failure}");
    }
    bail!("local Ticket reconcile failed")
}

fn dispatch_command(
    repository: &Repository,
    args: &DispatchArgs,
    open_workspaces: fn() -> Result<std::collections::BTreeSet<String>>,
    tickets_config: fn() -> Result<TicketsConfig>,
    spend_config: fn() -> Result<SpendConfig>,
) -> Result<()> {
    let model = selected_model(args.provider.as_deref(), args.model.as_deref())?;
    // The spend gate runs before anything is reserved or launched, including the
    // orchestration handoff below, so a blocked cap refuses the whole Dispatch.
    let spend_warning = spend_gate(repository, spend_config)?;
    if let Some(warning) = spend_warning {
        eprintln!("{warning}");
    }

    // A Ticket handed in directly is an input to Direct Dispatch, not a source
    // lookup: it is dispatched here, before any `[tickets]` config is read, so
    // an ad-hoc Dispatch works in a repository that has never been set up.
    if let Some(input) = &args.input {
        let ticket = AdHocTicket::read(input)?;
        return dispatch_adhoc_ticket(repository, args, &ticket, model, open_workspaces);
    }

    // A single Ticket with orchestration enabled fetches its own details before
    // reserving a placeholder, so hand off before building direct requests.
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
        let executable = std::env::current_exe()?;
        let mut command = std::process::Command::new(executable);
        command.arg("__orchestrate").arg(&args.tickets[0]);
        if let Some(provider) = args.provider.as_deref() {
            command.args(["--provider", provider]);
        }
        if let Some(model) = args.model.as_deref() {
            command.args(["--model", model]);
        }
        if args.force {
            command.arg("--force");
        }
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        render_dispatch_entries(
            &[DispatchEntry::Orchestrating {
                ticket: args.tickets[0].clone(),
                detail: "orchestrating in background".to_owned(),
            }],
            args.json,
        );
        return Ok(());
    }

    let exclusions = dispatch_exclusions(repository, args.force, open_workspaces)?;
    let tickets = tickets_config()?;
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
    )?);

    if args.all {
        let status = TicketStatus::parse(tickets.ready_status())?;
        let filter = ReadyTicketFilter::new(&args.label, status)?;
        eprintln!("Fetching tickets with label '{}'...", args.label);
        let ready = discovery.ready_tickets(&filter)?;
        for diagnostic in ready.diagnostics() {
            eprintln!("{}: {}", diagnostic.subject(), diagnostic.reason());
        }
        if ready.tickets().is_empty() {
            if args.json {
                render_dispatch_entries(&[], true);
            } else {
                eprintln!("No tickets found with label '{}'", args.label);
            }
            return Ok(());
        }
        let requests = ready
            .tickets()
            .iter()
            .map(|ticket| {
                let mut request = DirectDispatchRequest::new(ticket.clone(), args.mode);
                if let Some(details) = fetch_ticket_details(&discovery, ticket.id())? {
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
                // parallel branches off trunk. A Blocker with no branch to stack
                // on is stated rather than left silent, so a dependent that is
                // not fully stacked is never a surprise.
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
                        "Ticket {} is blocked by {blocker}, which has no branch; basing it on {base}",
                        ticket.id()
                    );
                }
                Ok(request)
            })
            .collect::<Result<Vec<_>>>()?;
        let result = dispatch_with_capacity_prompt(repository, &requests, &exclusions)?;
        render_dispatch_result(&result, requests.len(), true, args.json);
        write_back_local_tickets(repository, &tickets);
        return Ok(());
    }

    let requests = args
        .tickets
        .iter()
        .map(|ticket| {
            let id = TicketId::parse(ticket.clone())?;
            let mut request = DirectDispatchRequest::for_ticket_id(id.clone(), args.mode)?;
            if let Some(details) = fetch_ticket_details(&discovery, &id)? {
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
    render_dispatch_result(&result, requests.len(), false, args.json);
    write_back_local_tickets(repository, &tickets);
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
    render_dispatch_result(&result, requests.len(), false, args.json);
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

/// Evaluates the configured aggregate spend cap for one Dispatch.
///
/// `Ok(None)` when Dispatch is under the cap or no cap is configured; `Ok(Some)`
/// with the warning message when it is over the cap in `warn` mode; an error
/// naming the cap and its override when the cap is reached in `block` mode.
/// A cap that cannot be evaluated because the record store is unreadable is
/// also an error: failing closed is the only safe answer for a spend gate.
fn spend_gate(
    repository: &Repository,
    spend_config: fn() -> Result<SpendConfig>,
) -> Result<Option<String>> {
    let Some(cap) = spend_config()?.cap()? else {
        return Ok(None);
    };
    let snapshot = repository.pool_cost()?;
    match cap.evaluate(&snapshot) {
        crate::engine::SpendDecision::Allowed => Ok(None),
        crate::engine::SpendDecision::Warned(message) => Ok(Some(message)),
        crate::engine::SpendDecision::Refused(message) => Err(anyhow::anyhow!(message)),
    }
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
fn fetch_ticket_details<Q: TicketQuery>(
    discovery: &TicketDiscovery<Q>,
    id: &TicketId,
) -> Result<Option<TicketDetails>> {
    discovery.ticket_details(id).map(Some).with_context(|| {
        format!(
            "cannot fetch Ticket {id} to inline into the Dispatch prompt; ensure the pool runtime can reach Linear"
        )
    })
}

fn dispatch_with_capacity_prompt(
    repository: &Repository,
    requests: &[DirectDispatchRequest],
    exclusions: &DispatchExclusions,
) -> Result<crate::engine::DirectDispatchResult> {
    // The concurrency cap is jjfx's own setting, read here rather than passed
    // down: `cli::run` is already at the argument limit, and this is the only
    // place a whole batch is dispatched.
    let cap = crate::config::load()?.dispatch.cap()?;
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

fn growth_question(shortage: CapacityShortage) -> String {
    format!(
        "Pool has {} idle worker(s) but {} ticket(s) to dispatch. Grow pool by {}? [Y/n] ",
        shortage.available(),
        shortage.requested(),
        shortage.gap()
    )
}

fn confirm_growth(shortage: CapacityShortage) -> bool {
    eprint!("{}", growth_question(shortage));
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return false;
    }
    !input.trim().eq_ignore_ascii_case("n") && !input.trim().eq_ignore_ascii_case("no")
}

fn render_dispatch_result(
    result: &crate::engine::DirectDispatchResult,
    requested: usize,
    all: bool,
    json: bool,
) {
    let entries: Vec<DispatchEntry> = result
        .outcomes()
        .iter()
        .map(DispatchEntry::from_outcome)
        .collect();
    render_dispatch_entries(&entries, json);
    if json {
        return;
    }
    let fired = entries.iter().filter(|entry| entry.attempted()).count();
    if result.is_partial() {
        if fired == 0 && !all {
            eprintln!("No idle workers. Run: {PROGRAM} pool list");
        } else {
            eprintln!("No more idle workers. Dispatched {fired}/{requested} ticket(s).");
        }
    } else if all {
        eprintln!("Dispatched {fired} ticket(s)");
    }
    let held = result.waiting().len();
    if held > 0 {
        eprintln!("Held {held} ticket(s) back under the dispatch cap; re-run once a Run finishes.");
    }
}

/// The result for one Ticket in a Dispatch, the shared model both the prose and
/// the JSON renderings are built from.
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

    /// Returns the Ticket identifier.
    fn ticket(&self) -> &str {
        match self {
            Self::Dispatched { ticket, .. }
            | Self::Failed { ticket, .. }
            | Self::Waiting { ticket, .. }
            | Self::Orchestrating { ticket, .. } => ticket,
        }
    }

    /// Returns the stable machine-readable outcome name.
    const fn outcome(&self) -> &'static str {
        match self {
            Self::Dispatched { .. } => "dispatched",
            Self::Failed { .. } => "failed",
            Self::Waiting { .. } => "waiting",
            Self::Orchestrating { .. } => "orchestrating",
        }
    }

    /// Returns the Worker that took the Ticket, when one did.
    fn worker(&self) -> Option<&str> {
        match self {
            Self::Dispatched { worker, .. } => Some(worker),
            Self::Failed { worker, .. } => worker.as_deref(),
            Self::Waiting { .. } | Self::Orchestrating { .. } => None,
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
            | Self::Orchestrating { detail, .. } => detail,
        }
    }

    /// Whether Dispatch attempted this Ticket (launched or failed), as opposed
    /// to holding it back.
    const fn attempted(&self) -> bool {
        matches!(self, Self::Dispatched { .. } | Self::Failed { .. })
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
            DispatchEntry::Orchestrating { ticket, .. } => {
                out.push_str(&format!("Orchestrating {ticket} in background\n"));
                out.push_str(&format!(
                    "  Re-run '{PROGRAM} dispatch {ticket}' to check progress\n"
                ));
            }
        }
    }
    out
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
        let fields = [
            ("ticket", json_string(entry.ticket())),
            ("outcome", json_string(entry.outcome())),
            ("worker", json_option(entry.worker())),
            ("phase", json_option(entry.phase())),
            (
                "pid",
                entry
                    .pid()
                    .map_or_else(|| "null".to_owned(), |pid| pid.to_string()),
            ),
            ("detail", json_string(entry.detail())),
        ];
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

fn review_command(repository: &Repository, value: &str, mode: RunMode) -> Result<()> {
    let worker = normalize_worker(value)?;
    let config = crate::config::load()?.review;
    let Some(reviewer) = configured_reviewer(&config)? else {
        // No reviewer configured: exactly the pre-reviewer behaviour, one prompt
        // in the requested mode.
        let outcome = WorkerActions::new(repository.clone()).review(&worker, mode)?;
        render_review_outcome(&worker, &outcome);
        return Ok(());
    };
    let actions = WorkerActions::new(repository.clone());
    let request = actions.review_request(&worker)?;
    // A background Run returns before it has addressed anything, so only a
    // foreground review can iterate: it runs the full pass cap, while a
    // background review is a single pass. The cap is the bound either way.
    let passes = match mode {
        RunMode::Foreground => config.max_passes,
        RunMode::Background => 1,
    };
    let summary = ReviewLoop::new(passes).run(&*reviewer, &request, |_, outcome| {
        if let Some(reason) = outcome.completeness().reason() {
            eprintln!("Reviewer pass incomplete: {reason}");
        }
        let follow_up = actions
            .clone()
            .with_review_findings(outcome.findings().to_vec())
            .review(&worker, mode)?;
        render_review_outcome(&worker, &follow_up);
        Ok::<(), crate::engine::WorkerActionError>(())
    })?;
    eprintln!(
        "Review loop: {} pass(es), {} finding(s), stopped on {}.",
        summary.passes(),
        summary.findings(),
        summary.stop().as_str()
    );
    Ok(())
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

fn logs_command(repository: &Repository, value: &str) -> Result<()> {
    let worker = normalize_worker(value)?;
    let logs = WorkerActions::new(repository.clone()).logs(&worker)?;
    // Print the existing log first so the command never blocks silently: a
    // terminal Worker whose log does not parse would otherwise follow forever
    // with no output.
    print_log_tail(logs.path())?;
    if worker_is_terminal(repository, &worker) {
        return Ok(());
    }
    eprintln!("Following {} log for {worker}", logs.runtime());
    let mut last = None;
    loop {
        if let Some(activity) = logs.open().current_activity()? {
            let rendered = render_activity(&activity);
            if last.as_deref() != Some(rendered.as_str()) {
                println!("{rendered}");
                last = Some(rendered);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
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

/// Whether a Worker has reached a terminal state, so its log will not grow.
fn worker_is_terminal(repository: &Repository, worker: &WorkerId) -> bool {
    repository
        .worker_pool()
        .reconcile_runs()
        .worker(worker.as_str())
        .is_some_and(|worker| matches!(worker.status(), WorkerStatus::Done | WorkerStatus::Failed))
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

fn rebase_command(repository: &Repository, value: &str) -> Result<()> {
    let worker = normalize_worker(value)?;
    let outcome = WorkerActions::new(repository.clone()).rebase(&worker)?;
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

fn open_pr_command(repository: &Repository, value: &str) -> Result<()> {
    let worker = normalize_worker(value)?;
    let outcome = WorkerActions::new(repository.clone()).open_pull_request(&worker)?;
    eprintln!("Opened Pull Request for {}", outcome.branch());
    Ok(())
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

fn orchestrate_command(
    repository: &Repository,
    parent: &str,
    model: Option<AgentModel>,
    force: bool,
    json: bool,
    open_workspaces: fn() -> Result<std::collections::BTreeSet<String>>,
    tickets_config: fn() -> Result<TicketsConfig>,
) -> Result<()> {
    let id = TicketId::parse(parent.to_owned())?;
    // The runner re-queries occupancy per claim, so a Workspace a human opens or
    // closes while the group runs changes the next decision (ADR-0011).
    let exclusions = exclusions_provider(repository, force, open_workspaces);
    let tickets = tickets_config()?;
    let runtime = configured_runtime(repository);
    let discovery = TicketDiscovery::new(configured_ticket_query(
        repository.root(),
        &tickets,
        runtime,
    )?);
    // Fetch the Parent's details before `prepare` reserves a placeholder, so the
    // no-children direct fallback inlines the Ticket body.
    let details = fetch_ticket_details(&discovery, &id)?;
    let mut request = crate::engine::OrchestrationRequest::new(id.clone(), runtime);
    if let Some(details) = details {
        request = request.with_ticket_details(details);
    }
    let request = model.map_or(request.clone(), |selection| request.with_model(selection));
    let ticket = DirectDispatchRequest::for_ticket_id(id.clone(), RunMode::Background)?
        .ticket()
        .clone();
    let parent_ticket = crate::engine::ParentTicket::new(ticket.id().clone());
    let runner = repository
        .orchestration_runner()
        .with_exclusions(exclusions);
    let preparation = runner.prepare(&request, &parent_ticket, &discovery)?;
    if !json {
        render_orchestration_event(&OrchestrationEvent::Started {
            parent: id.clone(),
            resumed: preparation.resumed(),
        });
    }
    if let crate::engine::OrchestrationStart::Direct(success) = preparation.into_start() {
        render_dispatch_entries(&[DispatchEntry::from_success(&success)], json);
        return Ok(());
    }
    let options = crate::engine::OrchestrationOptions::new();
    let summary = runner.run(&request, &options, |event| {
        if !json {
            render_orchestration_event(&event);
        }
    })?;
    if json {
        let counts = summary.counts();
        render_dispatch_entries(
            &[DispatchEntry::Orchestrating {
                ticket: id.as_str().to_owned(),
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
    write_back_local_tickets(repository, &tickets);
    Ok(())
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
    'rebase:Rebase Worker' 'open-pr:Open Worker Pull Request'
    'resolve:Resolve a Workspace conflict'
    'absorb:Fold Worker edits into base commits'
    'undo:Undo the last jjfx mutation' 'ticket:Reconcile local Tickets' 'completion:Print shell completion' 'help:Show help'
  _arguments -C '1:command:->command' '*::arg:->args'
  case $state in
    command) _describe 'command' commands ;;
    args)
      case $words[1] in
        rm|remove|path) _arguments '1:workspace:__jjfx_workspaces' ;;
        send|s) _arguments '--fg[run in foreground]' '--bg[run in background]' '1:worker:__jjfx_non_busy_workers' '2:prompt:' ;;
        review|rev) _arguments '--fg[run in foreground]' '--bg[run in background]' '1:worker:__jjfx_non_busy_workers' ;;
        mount|m|reset|logs|log|rebase|rb|open-pr|pr|absorb|ab) _arguments '1:worker:__jjfx_workers' ;;
        resolve) _arguments '--tool[merge tool]:tool:' '1:worker:__jjfx_workers' ;;
        undo) _arguments '--force[discard later operations]' '1:worker:__jjfx_workers' ;;
        ticket) _arguments '1:subcommand:(reconcile)' ;;
        pool) _arguments '1:subcommand:(list resize rm reset profile destroy)' '2:runtime:(claude codex opencode)' '--provider[model provider]:provider:' '--model[model]:model:' ;;
        dispatch|d) _arguments '--fg[run in foreground]' '--bg[run in background]' '--all[dispatch all ready Tickets]' '--text[dispatch a one-off Ticket from text]:text:' '--from-file[dispatch a Ticket from a markdown file]:path:_files' '--id[Ticket id for ad-hoc input]:id:' '--no-orchestrate[skip orchestration]' '--json[print machine-readable results]' '--provider[model provider]:provider:' '--model[model]:model:' '--budget[maximum USD]:dollars:' '--label[label]:label:' '*:Ticket:' ;;
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

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
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
                label: "ready-for-agent".to_owned(),
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
                label: "ready-for-agent".to_owned(),
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

        let error = fetch_ticket_details(&discovery, &id)
            .expect_err("a runtime that cannot fetch the Ticket must abort Dispatch");
        let message = error.to_string();
        assert!(message.contains("AMBA-42"), "{message}");
        assert!(message.contains("Linear"), "{message}");
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
    /// Dispatch would have persisted it to `.jj/pool/waiting.json`.
    fn snapshot_with_waiting(waiting: &[&str]) -> crate::engine::WorkerPoolSnapshot {
        let temp = tempfile::tempdir().expect("temp repository");
        let pool = temp.path().join(".jj/pool");
        std::fs::create_dir_all(&pool).expect("pool directory");
        std::fs::write(
            temp.path().join(".jj/pool.json"),
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

    #[test]
    fn status_json_carries_the_waiting_set() {
        let held = snapshot_with_waiting(&["AMBA-42", "AMBA-43"]);
        let json = pool_list_json_string(&held, Timestamp::now(), StuckWindow::DEFAULT);
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(value["waiting"], serde_json::json!(["AMBA-42", "AMBA-43"]));
        assert_eq!(value["workers"][0]["worker"], "worker-01");
    }

    #[test]
    fn status_json_waiting_is_empty_without_a_capped_dispatch() {
        let idle = snapshot_with_waiting(&[]);
        let json = pool_list_json_string(&idle, Timestamp::now(), StuckWindow::DEFAULT);
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(value["waiting"], serde_json::json!([]));
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
            "Pool has 2 idle worker(s) but 5 ticket(s) to dispatch. Grow pool by 3? [Y/n] "
        );
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
                label: "needs-review".to_owned(),
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
                label: "ready-for-agent".to_owned(),
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
        let snapshot = repository.read_worker_pool_snapshot();

        let summary = spend_summary(&snapshot);
        assert!(summary.contains("$1.0000 total"), "{summary}");
        assert!(summary.contains("across 2 Run(s)"), "{summary}");
        assert_eq!(worker_spend(&snapshot, "worker-01"), "$0.2500");
        assert_eq!(worker_spend(&snapshot, "worker-03"), "-");
    }

    #[test]
    fn the_dispatch_spend_gate_allows_warns_and_blocks() {
        let temp = tempfile::tempdir().expect("temp repository");
        std::fs::create_dir(temp.path().join(".jj")).expect("repository marker");
        let repository = Repository::open(temp.path()).expect("repository");
        append_run(&repository, "worker-01", Some(6_000_000));

        // No cap configured: Dispatch is never gated.
        assert_eq!(
            spend_gate(&repository, || Ok(SpendConfig::default())).expect("no cap"),
            None
        );

        // Under the cap: allowed.
        let under = || {
            Ok(SpendConfig {
                cap_usd: Some(10.0),
                ..SpendConfig::default()
            })
        };
        assert_eq!(spend_gate(&repository, under).expect("under"), None);

        // Over the cap in warn mode: a message, and Dispatch proceeds.
        let warn = || {
            Ok(SpendConfig {
                cap_usd: Some(5.0),
                on_exceed: crate::engine::SpendCapMode::Warn,
                ..SpendConfig::default()
            })
        };
        let warning = spend_gate(&repository, warn)
            .expect("warn")
            .expect("a warning");
        assert!(warning.contains("dispatching anyway"), "{warning}");

        // Over the cap in block mode: an error naming the cap and the override.
        let block = || {
            Ok(SpendConfig {
                cap_usd: Some(5.0),
                ..SpendConfig::default()
            })
        };
        let error = spend_gate(&repository, block).expect_err("blocked");
        assert!(error.to_string().contains("spend.cap_usd"), "{error}");
        assert!(error.to_string().contains("spend.on_exceed"), "{error}");
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
                error: None,
                started_at: None,
                completed_at: Some(crate::engine::WireTimestamp::new(
                    jiff::Timestamp::now().to_string(),
                )),
            })
            .expect("append record");
    }
}
