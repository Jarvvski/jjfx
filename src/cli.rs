use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
const PROGRAM: &str = env!("CARGO_PKG_NAME");

use crate::config::{MigrationsConfig, SpendConfig, TicketsConfig};
use crate::engine::{
    AgentModel, AgentRuntime, AgentRuntimeProfile, CapacityShortage, CleanDecision,
    DirectDispatchError, DirectDispatchExecution, DirectDispatchOutcome, DirectDispatchRequest,
    DispatchBudget, DispatchExclusions, FollowUpExecution, FollowUpOutcome, OrchestrationEvent,
    PoolCapacity, ReadyTicketFilter, Repository, ReviewLoop, RunActivity, RunActivityKind, RunMode,
    TerminalSettings, TicketDetails, TicketDiscovery, TicketId, TicketQuery, TicketStatus,
    WireAgent, WorkerActions, WorkerId, WorkerPoolError, WorkerStatus, WorkspaceAddOutcome,
    WorkspaceHookStream,
};
use crate::migrations::FindingKind;
use crate::reviewer_source::configured_reviewer;
use crate::ticket_source::{apply_local_writeback, configured_ticket_query};

pub const HELP: &str = concat!(
    env!("CARGO_PKG_NAME"),
    " - jj workspace manager\n\nUsage: ",
    env!("CARGO_PKG_NAME"),
    " [OPTIONS]\n\nUsage:\n  ",
    env!("CARGO_PKG_NAME"),
    " add <name> [-r <rev>]     Create workspace and print path (stdout)\n  ",
    env!("CARGO_PKG_NAME"),
    " rm [--force] <name>       Remove workspace\n  ",
    env!("CARGO_PKG_NAME"),
    " list                      List workspaces\n  ",
    env!("CARGO_PKG_NAME"),
    " clean                     Remove all non-default workspaces\n  ",
    env!("CARGO_PKG_NAME"),
    " root                      Print repo root\n  ",
    env!("CARGO_PKG_NAME"),
    " where                     Show repo and workspace paths\n  ",
    env!("CARGO_PKG_NAME"),
    " path <name>               Print workspace path\n  ",
    env!("CARGO_PKG_NAME"),
    " refresh                   Rebuild workspace cache\n  ",
    env!("CARGO_PKG_NAME"),
    " colocation <action>       Enable/disable Git colocation (enable|disable|status)\n\nPool:\n  ",
    env!("CARGO_PKG_NAME"),
    " pool <N>                  Set pool size (creates pool if needed, safe shrink)\n  ",
    env!("CARGO_PKG_NAME"),
    " pool list [--json]        Show pool status\n  ",
    env!("CARGO_PKG_NAME"),
    " pool rm <worker>          Remove a worker from the pool (must not be busy)\n  ",
    env!("CARGO_PKG_NAME"),
    " pool reset <worker>       Reset a worker to idle\n  ",
    env!("CARGO_PKG_NAME"),
    " pool profile <runtime>    Set runtime profile (--provider/--model)\n  ",
    env!("CARGO_PKG_NAME"),
    " pool destroy              Tear down all workers and remove pool\n\nDispatch and sessions:\n  ",
    env!("CARGO_PKG_NAME"),
    " dispatch <TICKET>...     Dispatch Tickets (d; --provider/--model/--force)\n  ",
    env!("CARGO_PKG_NAME"),
    " send <worker> <prompt>   Send a Follow-up (s; --force)\n  ",
    env!("CARGO_PKG_NAME"),
    " review <worker>          Address PR review comments (rev)\n  ",
    env!("CARGO_PKG_NAME"),
    " logs <worker>            Follow a Worker log (log)\n  ",
    env!("CARGO_PKG_NAME"),
    " mount <worker>           Mount a Worker in kitty (m)\n  ",
    env!("CARGO_PKG_NAME"),
    " rebase <worker>          Rebase and push a Worker (rb)\n  ",
    env!("CARGO_PKG_NAME"),
    " absorb <worker>          Fold a Worker's edits into their base commits (ab)\n  ",
    env!("CARGO_PKG_NAME"),
    " open-pr <worker>         Open a Worker's Pull Request (pr)\n\nCompletion:\n  ",
    env!("CARGO_PKG_NAME"),
    " completion [zsh]         Print zsh completion\n\nChecks:\n  ",
    env!("CARGO_PKG_NAME"),
    " check migrations          Flag migration-order inversions across workspaces\n\nObservability:\n  ",
    env!("CARGO_PKG_NAME"),
    " status [--json]           Alias for pool list\n  ",
    env!("CARGO_PKG_NAME"),
    " version                   Print the ",
    env!("CARGO_PKG_NAME"),
    " version\n",
);

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Default,
    Help,
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
        "help" | "-h" | "--help" => Ok(Command::Help),
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
        unknown => bail!("Unknown command: {unknown}. Run '{PROGRAM} help' for usage."),
    }
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
    let mut json = false;
    for argument in args {
        match argument.as_str() {
            "--json" => json = true,
            value => bail!("Unknown option for status: {value}"),
        }
    }
    Ok(Command::Status { json })
}

/// Parse `pool list`/`ls`/`status`, which accepts `--json`.
fn parse_pool_list(args: &[String]) -> Result<Command> {
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
    let usage = concat!(
        "Usage: ",
        env!("CARGO_PKG_NAME"),
        " pool profile <claude|codex|pi|opencode> [--provider PROVIDER] [--model MODEL]"
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
    let mut tickets = Vec::new();
    let mut all = false;
    let mut mode = None;
    let mut provider = None;
    let mut model = None;
    let mut budget = None;
    let mut label = "ready-for-agent".to_owned();
    let mut no_orchestrate = false;
    let mut force = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--fg" | "--bg" => parse_mode(&args[index], &mut mode)?,
            "--all" => all = true,
            "--no-orchestrate" => no_orchestrate = true,
            "--force" => force = true,
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
    if tickets.is_empty() && !all {
        bail!(concat!(
            "Usage: ",
            env!("CARGO_PKG_NAME"),
            " dispatch <TICKET>... [--fg|--bg] [--provider PROVIDER] [--model MODEL]"
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
    }))
}

fn parse_send(args: &[String]) -> Result<Command> {
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
        Command::Help => print!("{HELP}"),
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
            provider.as_deref(),
            model.as_deref(),
            force,
            open_workspaces,
            tickets,
        )?,
        Command::Pool(command) => pool_command(&repository(colocate)?, command)?,
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
    let workspaces = repository.workspaces();
    let prefixed = format!("worker-{name}");
    let path = workspaces
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
        });
    println!("{}", path.display());
    Ok(())
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
            print!("{HELP}");
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
    let runtime = AgentRuntime::from_configured(Some(&configured)).map_err(|value| {
        anyhow::anyhow!("invalid Agent Runtime {value:?} (expected claude, codex, pi, or opencode)")
    })?;
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
    let snapshot = repository.worker_pool().reconcile_runs();
    let pool = snapshot
        .pool()
        .ok_or_else(|| anyhow::anyhow!("No pool. Run: {PROGRAM} pool create --size N"))?;
    if json {
        return pool_list_json(&snapshot);
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
    println!(
        "Pool: {} idle, {} busy, {} done, {} failed ({} total)",
        counts[0],
        counts[1],
        counts[2],
        counts[3],
        pool.size()
    );
    println!("{}", spend_summary(&snapshot));
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
///
/// Encoded by hand so the emitted field order is fixed.
fn pool_list_json(snapshot: &crate::engine::WorkerPoolSnapshot) -> Result<()> {
    let mut out = String::from("[\n");
    for (index, worker) in snapshot.workers().iter().enumerate() {
        if index > 0 {
            out.push_str(",\n");
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
    println!("{out}");
    Ok(())
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
    let duration = completed.duration_since(started);
    let seconds = duration.as_secs_f64().trunc() as i64;
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
/// a Dispatch that already succeeded, so a failure is reported rather than raised.
fn write_back_local_tickets(repository: &Repository, tickets: &TicketsConfig) {
    let snapshot = repository.worker_pool().reconcile_runs();
    for failure in apply_local_writeback(repository, &snapshot, tickets) {
        eprintln!("local Ticket writeback failed: {failure}");
    }
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

    // A single Ticket with orchestration enabled fetches its own details before
    // reserving a placeholder, so hand off before building direct requests.
    if !args.all && !args.no_orchestrate && args.tickets.len() == 1 {
        if args.mode == RunMode::Foreground {
            return orchestrate_command(
                repository,
                &args.tickets[0],
                args.provider.as_deref(),
                args.model.as_deref(),
                args.force,
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
        eprintln!("Orchestrating {} in background", args.tickets[0]);
        eprintln!(
            "  Re-run '{PROGRAM} dispatch {}' to check progress",
            args.tickets[0]
        );
        return Ok(());
    }

    let exclusions = dispatch_exclusions(repository, args.force, open_workspaces)?;
    let tickets = tickets_config()?;
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
            eprintln!("No tickets found with label '{}'", args.label);
            return Ok(());
        }
        let requests = ready
            .tickets()
            .iter()
            .map(|ticket| {
                let mut request = DirectDispatchRequest::new(ticket.clone(), args.mode);
                if let Some(details) = fetch_ticket_details(runtime, &discovery, ticket.id())? {
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
        render_dispatch_result(&result, requests.len(), true);
        write_back_local_tickets(repository, &tickets);
        return Ok(());
    }

    let requests = args
        .tickets
        .iter()
        .map(|ticket| {
            let id = TicketId::parse(ticket.clone())?;
            let mut request = DirectDispatchRequest::for_ticket_id(id.clone(), args.mode)?;
            if let Some(details) = fetch_ticket_details(runtime, &discovery, &id)? {
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
    render_dispatch_result(&result, requests.len(), false);
    write_back_local_tickets(repository, &tickets);
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
///
/// Pi is skipped: its Direct Dispatch preflight already guarantees the pinned
/// Linear tools, so the Worker fetches the Ticket itself.
fn fetch_ticket_details<Q: TicketQuery>(
    runtime: AgentRuntime,
    discovery: &TicketDiscovery<Q>,
    id: &TicketId,
) -> Result<Option<TicketDetails>> {
    if runtime == AgentRuntime::Pi {
        return Ok(None);
    }
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
    let dispatcher = repository
        .direct_dispatch()
        .with_exclusions(exclusions.clone());
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
) {
    for outcome in result.outcomes() {
        match outcome {
            DirectDispatchOutcome::Succeeded(success) => match success.execution() {
                DirectDispatchExecution::Background { pid } => eprintln!(
                    "  {} (PID {}) -> {}",
                    success.worker(),
                    pid,
                    success.ticket().id()
                ),
                DirectDispatchExecution::Foreground(completed) => {
                    eprintln!(
                        "  {} completed {:?}",
                        success.worker(),
                        completed.result().conclusion()
                    )
                }
            },
            DirectDispatchOutcome::Failed(failure) => eprintln!(
                "  {} failed at {:?}: {}",
                failure.ticket().id(),
                failure.phase(),
                failure.detail()
            ),
        }
    }
    if result.is_partial() {
        let dispatched = result.outcomes().len();
        if dispatched == 0 && !all {
            eprintln!("No idle workers. Run: {PROGRAM} pool list");
        } else {
            eprintln!("No more idle workers. Dispatched {dispatched}/{requested} ticket(s).");
        }
    } else if all {
        eprintln!("Dispatched {} ticket(s)", result.outcomes().len());
    }
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
    provider: Option<&str>,
    model: Option<&str>,
    force: bool,
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
    let details = fetch_ticket_details(runtime, &discovery, &id)?;
    let mut request = crate::engine::OrchestrationRequest::new(id.clone(), runtime);
    if let Some(details) = details {
        request = request.with_ticket_details(details);
    }
    let request = selected_model(provider, model)?
        .map_or(request.clone(), |selection| request.with_model(selection));
    let ticket = DirectDispatchRequest::for_ticket_id(id.clone(), RunMode::Background)?
        .ticket()
        .clone();
    let parent_ticket = crate::engine::ParentTicket::new(ticket.id().clone());
    let runner = repository
        .orchestration_runner()
        .with_exclusions(exclusions);
    let preparation = runner.prepare(&request, &parent_ticket, &discovery)?;
    render_orchestration_event(&OrchestrationEvent::Started {
        parent: id.clone(),
        resumed: preparation.resumed(),
    });
    if let crate::engine::OrchestrationStart::Direct(success) = preparation.into_start() {
        if let DirectDispatchExecution::Background { pid } = success.execution() {
            eprintln!(
                "  {} (PID {}) -> {}",
                success.worker(),
                pid,
                success.ticket().id()
            );
        }
        return Ok(());
    }
    let options = crate::engine::OrchestrationOptions::new();
    let summary = runner.run(&request, &options, |event| {
        render_orchestration_event(&event);
    })?;
    render_orchestration_event(&OrchestrationEvent::Terminal(summary));
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
    'absorb:Fold Worker edits into base commits'
    'completion:Print shell completion' 'help:Show help'
  )
  _arguments -C '1:command:->command' '*::arg:->args'
  case $state in
    command) _describe 'command' commands ;;
    args)
      case $words[1] in
        rm|remove|path) _arguments '1:workspace:__jjfx_workspaces' ;;
        send|s) _arguments '--fg[run in foreground]' '--bg[run in background]' '1:worker:__jjfx_non_busy_workers' '2:prompt:' ;;
        review|rev) _arguments '--fg[run in foreground]' '--bg[run in background]' '1:worker:__jjfx_non_busy_workers' ;;
        mount|m|reset|logs|log|rebase|rb|open-pr|pr|absorb|ab) _arguments '1:worker:__jjfx_workers' ;;
        pool) _arguments '1:subcommand:(list resize rm reset profile destroy)' '2:runtime:(claude codex pi opencode)' '--provider[model provider]:provider:' '--model[model]:model:' ;;
        dispatch|d) _arguments '--fg[run in foreground]' '--bg[run in background]' '--all[dispatch all ready Tickets]' '--no-orchestrate[skip orchestration]' '--provider[model provider]:provider:' '--model[model]:model:' '--budget[maximum USD]:dollars:' '--label[label]:label:' '*:Ticket:' ;;
      esac
      ;;
  esac
}
compdef _jjfx jjfx
"#;

fn default_command(launch: fn(PathBuf) -> Result<()>) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        print!("{HELP}");
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
            })
        );
    }

    #[test]
    fn parser_rejects_unknown_dispatch_options_and_budgeted_orchestration() {
        assert!(parse(&args(&["dispatch", "AMBA-42", "--wat"])).is_err());
        assert!(parse(&args(&["dispatch", "AMBA-42", "--budget", "12"])).is_err());
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
    fn fetch_ticket_details_aborts_with_context_and_skips_pi() {
        let discovery = TicketDiscovery::new(FailingQuery);
        let id = TicketId::parse("AMBA-42").expect("Ticket ID");

        let error = fetch_ticket_details(AgentRuntime::Claude, &discovery, &id)
            .expect_err("a runtime that cannot fetch the Ticket must abort Dispatch");
        let message = error.to_string();
        assert!(message.contains("AMBA-42"), "{message}");
        assert!(message.contains("Linear"), "{message}");

        // Pi is skipped: its Direct Dispatch preflight owns tool validation.
        assert_eq!(
            fetch_ticket_details(AgentRuntime::Pi, &discovery, &id).expect("Pi is skipped"),
            None
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
        assert!(HELP.contains("completion [zsh]"));
        assert!(!HELP.contains("__complete"));
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
