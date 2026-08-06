use std::io::{IsTerminal, Read};
use std::str::FromStr;

use anyhow::{Result, bail};
use jiff::Timestamp;
use wsg_core::{
    AgentRuntime, AgentRuntimeQuery, CapacityShortage, CleanDecision, DirectDispatchError,
    DirectDispatchExecution, DirectDispatchOutcome, DirectDispatchRequest, DispatchBudget,
    FollowUpExecution, OrchestrationEvent, PoolCapacity, ReadyTicketFilter, Repository,
    RunActivity, RunActivityKind, RunMode, TicketDiscovery, TicketId, TicketStatus, WorkerActions,
    WorkerId, WorkerPoolError, WorkerStatus, WorkspaceAddOutcome,
};

pub const HELP: &str = r#"wsg - jj workspace manager

Usage: wsg [OPTIONS]

Usage:
  wsg add <name> [-r <rev>]     Create workspace and print path (stdout)
  wsg rm [--force] <name>       Remove workspace
  wsg list                      List workspaces
  wsg clean                     Remove all non-default workspaces
  wsg root                      Print repo root
  wsg where                     Show repo and workspace paths
  wsg path <name>               Print workspace path
  wsg refresh                   Rebuild workspace cache

Pool:
  wsg pool <N>                  Set pool size (creates pool if needed, safe shrink)
  wsg pool list                 Show pool status
  wsg pool rm <worker>          Remove a worker from the pool (must not be busy)
  wsg pool reset <worker>       Reset a worker to idle
  wsg pool destroy              Tear down all workers and remove pool

Dispatch and sessions:
  wsg dispatch <TICKET>...     Dispatch Tickets (d)
  wsg send <worker> <prompt>   Send a Follow-up (s)
  wsg review <worker>          Address PR review comments (rev)
  wsg logs <worker>            Follow a Worker log (log)
  wsg mount <worker>           Mount a Worker in kitty (m)
  wsg rebase <worker>          Rebase and push a Worker (rb)
  wsg open-pr <worker>         Open a Worker's Pull Request (pr)

Completion:
  wsg completion [zsh]         Print zsh completion

Observability:
  wsg status                    Alias for pool list
  wsg version                   Print the wsg version
"#;

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
    Status,
    Dispatch(DispatchArgs),
    Send {
        worker: String,
        prompt: String,
        mode: RunMode,
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
    OpenPullRequest {
        worker: String,
    },
    Completion {
        shell: String,
    },
    InternalComplete {
        mode: String,
    },
    InternalOrchestrate {
        parent: String,
        model: Option<String>,
    },
    Pool(PoolCommand),
}

#[derive(Debug, PartialEq, Eq)]
pub struct DispatchArgs {
    pub tickets: Vec<String>,
    pub all: bool,
    pub mode: RunMode,
    pub model: Option<String>,
    pub budget: Option<u32>,
    pub label: String,
    pub no_orchestrate: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PoolCommand {
    Resize { size: String },
    List,
    Remove { worker: String },
    Reset { worker: String },
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
        "path" => {
            parse_single("Usage: wsg path <name>", &args[1..]).map(|name| Command::Path { name })
        }
        "refresh" | "sync" => Ok(Command::Refresh),
        "status" => ensure_no_extra("wsg status", &args[1..]).map(|_| Command::Status),
        "reset" => parse_single_exact("Usage: wsg reset <worker>", &args[1..])
            .map(|worker| Command::Pool(PoolCommand::Reset { worker })),
        "dispatch" | "d" => parse_dispatch(&args[1..]),
        "send" | "s" => parse_send(&args[1..]),
        "review" | "rev" => parse_review(&args[1..]),
        "logs" | "log" => parse_worker_command("Usage: wsg logs <worker>", &args[1..])
            .map(|worker| Command::Logs { worker }),
        "mount" | "m" => parse_worker_command("Usage: wsg mount <worker>", &args[1..])
            .map(|worker| Command::Mount { worker }),
        "rebase" | "rb" => parse_worker_command("Usage: wsg rebase <worker>", &args[1..])
            .map(|worker| Command::Rebase { worker }),
        "open-pr" | "pr" => parse_worker_command("Usage: wsg open-pr <worker>", &args[1..])
            .map(|worker| Command::OpenPullRequest { worker }),
        "completion" => parse_completion(&args[1..]),
        "__complete" => parse_worker_command("Usage: wsg __complete <mode>", &args[1..])
            .map(|mode| Command::InternalComplete { mode }),
        "__orchestrate" => parse_orchestrate(&args[1..]),
        "pool" => parse_pool(&args[1..]),
        unknown => bail!("Unknown command: {unknown}. Run 'wsg help' for usage."),
    }
}

fn parse_add(args: &[String]) -> Result<Command> {
    let mut name = None;
    let mut revision = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-r" | "--revision" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| anyhow::anyhow!("Usage: wsg add <name> [-r <rev>]"))?;
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
    let name = name.ok_or_else(|| anyhow::anyhow!("Usage: wsg add <name> [-r <rev>]"))?;
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
        bail!("Usage: wsg rm [--force] <name> [name...]");
    }
    Ok(Command::Remove { force, names })
}

fn parse_pool(args: &[String]) -> Result<Command> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        return Ok(PoolCommand::List.into());
    };
    match subcommand {
        "list" | "ls" | "status" => Ok(PoolCommand::List.into()),
        "destroy" => Ok(PoolCommand::Destroy.into()),
        "help" => Ok(PoolCommand::Help.into()),
        "rm" | "remove" => parse_single("Usage: wsg pool rm <worker>", &args[1..])
            .map(|worker| PoolCommand::Remove { worker }.into()),
        "reset" => parse_single("Usage: wsg pool reset <worker>", &args[1..])
            .map(|worker| PoolCommand::Reset { worker }.into()),
        "create" | "c" | "resize" | "r" => parse_pool_size(&args[1..]),
        value if value.parse::<i64>().is_ok() => parse_pool_size(args),
        unknown => bail!("Unknown pool command: {unknown}"),
    }
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
    let size = size.ok_or_else(|| anyhow::anyhow!("Usage: wsg pool resize <N>"))?;
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

fn ensure_no_extra(usage: &str, args: &[String]) -> Result<()> {
    if args.is_empty() {
        Ok(())
    } else {
        bail!("{usage}");
    }
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
    let mut model = None;
    let mut budget = None;
    let mut label = "ready-for-agent".to_owned();
    let mut no_orchestrate = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--fg" | "--bg" => parse_mode(&args[index], &mut mode)?,
            "--all" => all = true,
            "--no-orchestrate" => no_orchestrate = true,
            "--model" | "--label" | "--budget" => {
                let option = args[index].as_str();
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| anyhow::anyhow!("missing value for {option}"))?;
                if value.starts_with('-') {
                    bail!("missing value for {option}");
                }
                match option {
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
        bail!("Usage: wsg dispatch <TICKET>... [--fg|--bg] [--model MODEL]");
    }
    if budget.is_some() && tickets.len() == 1 && !no_orchestrate {
        bail!("--budget for one Ticket requires --no-orchestrate");
    }
    Ok(Command::Dispatch(DispatchArgs {
        tickets,
        all,
        mode: mode.unwrap_or(RunMode::Background),
        model,
        budget,
        label,
        no_orchestrate,
    }))
}

fn parse_send(args: &[String]) -> Result<Command> {
    let mut values = Vec::new();
    let mut mode = None;
    for argument in args {
        match argument.as_str() {
            "--fg" | "--bg" => parse_mode(argument, &mut mode)?,
            value if value.starts_with('-') => bail!("unknown option for send: {value}"),
            value => values.push(value.to_owned()),
        }
    }
    if values.len() != 2 {
        bail!("Usage: wsg send <worker> <prompt> [--fg|--bg]");
    }
    Ok(Command::Send {
        worker: values.remove(0),
        prompt: values.remove(0),
        mode: mode.unwrap_or(RunMode::Background),
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
            _ => bail!("Usage: wsg review <worker> [--fg|--bg]"),
        }
    }
    Ok(Command::Review {
        worker: worker.ok_or_else(|| anyhow::anyhow!("Usage: wsg review <worker> [--fg|--bg]"))?,
        mode: mode.unwrap_or(RunMode::Background),
    })
}

fn parse_completion(args: &[String]) -> Result<Command> {
    if args.len() > 1 {
        bail!("Usage: wsg completion [zsh]");
    }
    Ok(Command::Completion {
        shell: args.first().cloned().unwrap_or_else(|| "zsh".to_owned()),
    })
}

fn parse_orchestrate(args: &[String]) -> Result<Command> {
    let Some(parent) = args.first() else {
        bail!("Usage: wsg __orchestrate <PARENT-TICKET> [--model MODEL]");
    };
    let mut model = None;
    let mut index = 1;
    while index < args.len() {
        if args[index] != "--model" {
            bail!("unknown option for __orchestrate: {}", args[index]);
        }
        model = Some(
            args.get(index + 1)
                .ok_or_else(|| anyhow::anyhow!("missing value for --model"))?
                .clone(),
        );
        index += 2;
    }
    Ok(Command::InternalOrchestrate {
        parent: parent.clone(),
        model,
    })
}

impl From<PoolCommand> for Command {
    fn from(command: PoolCommand) -> Self {
        Self::Pool(command)
    }
}

pub fn run(args: Vec<String>) -> Result<()> {
    match parse(&args)? {
        Command::Help => print!("{HELP}"),
        Command::Version => println!("wsg {}", env!("CARGO_PKG_VERSION")),
        Command::Default => default_command()?,
        Command::Root => root_command(&repository()?)?,
        Command::Where => where_command(&repository()?)?,
        Command::Path { name } => path_command(&repository()?, &name)?,
        Command::Refresh => refresh_command(&repository()?)?,
        Command::Add { name, revision } => add_command(&repository()?, &name, revision.as_deref())?,
        Command::List => list_command(&repository()?)?,
        Command::Remove { force, names } => remove_command(&repository()?, force, &names)?,
        Command::Clean => clean_command(&repository()?)?,
        Command::Status => pool_list_command(&repository()?)?,
        Command::Dispatch(args) => dispatch_command(&repository()?, &args)?,
        Command::Send {
            worker,
            prompt,
            mode,
        } => send_command(&repository()?, &worker, &prompt, mode)?,
        Command::Review { worker, mode } => review_command(&repository()?, &worker, mode)?,
        Command::Logs { worker } => logs_command(&repository()?, &worker)?,
        Command::Mount { worker } => mount_command(&repository()?, &worker)?,
        Command::Rebase { worker } => rebase_command(&repository()?, &worker)?,
        Command::OpenPullRequest { worker } => open_pr_command(&repository()?, &worker)?,
        Command::Completion { shell } => completion_command(&shell)?,
        Command::InternalComplete { mode } => internal_complete_command(&mode)?,
        Command::InternalOrchestrate { parent, model } => {
            orchestrate_command(&repository()?, &parent, model.as_deref())?
        }
        Command::Pool(command) => pool_command(&repository()?, command)?,
    }
    Ok(())
}

fn repository() -> Result<Repository> {
    Repository::open(".").map_err(|_| anyhow::anyhow!("Not in a jj repo"))
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

fn path_command(repository: &Repository, name: &str) -> Result<()> {
    println!("{}", repository.workspaces().path(name).display());
    Ok(())
}

fn refresh_command(repository: &Repository) -> Result<()> {
    repository.workspaces().refresh()?;
    eprintln!("Cache refreshed");
    Ok(())
}

fn add_command(repository: &Repository, name: &str, revision: Option<&str>) -> Result<()> {
    match repository.workspaces().add(name, revision)? {
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
        let missing = if entry.name() != "default" && entry.is_missing() {
            " (missing)"
        } else {
            ""
        };
        println!("  {} ➜ {}{}", entry.name(), entry.path().display(), missing);
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
        match workspaces.remove(name, force) {
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
    workspaces.clean(&plan, decision)?;
    Ok(())
}

fn pool_command(repository: &Repository, command: PoolCommand) -> Result<()> {
    match command {
        PoolCommand::Resize { size } => resize_pool_command(repository, &size),
        PoolCommand::List => pool_list_command(repository),
        PoolCommand::Remove { worker } => remove_worker_command(repository, &worker),
        PoolCommand::Reset { worker } => reset_worker_command(repository, &worker),
        PoolCommand::Destroy => destroy_pool_command(repository),
        PoolCommand::Help => {
            print!("{HELP}");
            Ok(())
        }
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
    let resize = repository.worker_pool().resize_to(capacity)?;
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

fn pool_list_command(repository: &Repository) -> Result<()> {
    let snapshot = repository.worker_pool().reconcile_runs();
    let pool = snapshot
        .pool()
        .ok_or_else(|| anyhow::anyhow!("No pool. Run: wsg pool create --size N"))?;
    for diagnostic in snapshot.diagnostics() {
        eprintln!("{}", diagnostic.message());
    }
    println!(
        "{:<10} {:<12} {:<10} {:<14} ELAPSED",
        "WORKER", "NAME", "STATUS", "TICKET"
    );
    println!(
        "{:<10} {:<12} {:<10} {:<14} -------",
        "------", "----", "------", "------"
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
        let short = worker
            .worker_id()
            .as_str()
            .strip_prefix("worker-")
            .unwrap_or(worker.worker_id().as_str());
        let name = if worker.alias().is_empty() {
            "-"
        } else {
            worker.alias()
        };
        let ticket = worker.ticket().unwrap_or("-");
        let elapsed = elapsed_display(worker.started_at(), worker.completed_at());
        println!(
            "{short:<10} {name:<12} {:<10} {ticket:<14} {elapsed}",
            worker.status().as_str()
        );
    }
    println!();
    println!(
        "Pool: {} idle, {} busy, {} done, {} failed ({} total)",
        counts[0],
        counts[1],
        counts[2],
        counts[3],
        pool.size()
    );
    Ok(())
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
    let resize = repository.worker_pool().remove(worker.clone())?;
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
    repository.worker_pool().destroy()?;
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

fn dispatch_command(repository: &Repository, args: &DispatchArgs) -> Result<()> {
    if args.all {
        let runtime = configured_runtime(repository);
        let status = TicketStatus::parse("Todo")?;
        let filter = ReadyTicketFilter::new(&args.label, status)?;
        let discovery = TicketDiscovery::new(AgentRuntimeQuery::new(runtime, repository.root()));
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
                if let Some(model) = args.model.as_deref() {
                    request = request.with_model(model);
                }
                if let Some(dollars) = args.budget {
                    request = request.with_budget(DispatchBudget::maximum_usd(dollars)?);
                }
                Ok(request)
            })
            .collect::<Result<Vec<_>>>()?;
        let result = dispatch_with_capacity_prompt(repository, &requests)?;
        render_dispatch_result(&result, requests.len(), true);
        return Ok(());
    }

    let requests = args
        .tickets
        .iter()
        .map(|ticket| {
            let id = TicketId::parse(ticket.clone())?;
            let mut request = DirectDispatchRequest::for_ticket_id(id, args.mode)?;
            if let Some(model) = args.model.as_deref() {
                request = request.with_model(model);
            }
            if let Some(dollars) = args.budget {
                request = request.with_budget(DispatchBudget::maximum_usd(dollars)?);
            }
            Ok(request)
        })
        .collect::<Result<Vec<_>>>()?;
    let result = if args.no_orchestrate || requests.len() != 1 {
        dispatch_with_capacity_prompt(repository, &requests)?
    } else {
        eprintln!("Orchestrating {} in background", args.tickets[0]);
        dispatch_with_capacity_prompt(repository, &requests)?
    };
    render_dispatch_result(&result, requests.len(), false);
    Ok(())
}

fn dispatch_with_capacity_prompt(
    repository: &Repository,
    requests: &[DirectDispatchRequest],
) -> Result<wsg_core::DirectDispatchResult> {
    let dispatcher = repository.direct_dispatch();
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
        "Pool has {} idle worker(s) but {} ticket(s) to dispatch. Resize pool to {}? [Y/n] ",
        shortage.available(),
        shortage.requested(),
        shortage.available() + shortage.gap()
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

fn render_dispatch_result(result: &wsg_core::DirectDispatchResult, requested: usize, all: bool) {
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
            eprintln!("No idle workers. Run: wsg pool list");
        } else {
            eprintln!("No more idle workers. Dispatched {dispatched}/{requested} ticket(s).");
        }
    } else if all {
        eprintln!("Dispatched {} ticket(s)", result.outcomes().len());
    }
}

fn send_command(repository: &Repository, value: &str, prompt: &str, mode: RunMode) -> Result<()> {
    let worker = normalize_worker(value)?;
    eprintln!("Sending to {worker}...");
    let outcome = WorkerActions::new(repository.clone()).send(&worker, prompt, mode)?;
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
    let outcome = WorkerActions::new(repository.clone()).review(&worker, mode)?;
    render_session(outcome.session());
    if let FollowUpExecution::Background(run) = outcome.execution() {
        eprintln!("  {worker} (PID {}) -> review", run.pid());
    }
    Ok(())
}

fn render_session(session: &wsg_core::AgentSessionResolution) {
    match session {
        wsg_core::AgentSessionResolution::Resumed { session_id } => {
            eprintln!("Resumed session {session_id}");
        }
        wsg_core::AgentSessionResolution::Fresh { reason } => {
            eprintln!("Starting fresh session ({reason})");
        }
    }
}

fn truncate_prompt(prompt: &str) -> String {
    prompt.chars().take(60).collect()
}

fn logs_command(repository: &Repository, value: &str) -> Result<()> {
    let worker = normalize_worker(value)?;
    let logs = WorkerActions::new(repository.clone()).logs(&worker)?;
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

fn mount_command(repository: &Repository, value: &str) -> Result<()> {
    let worker = normalize_worker(value)?;
    let outcome = WorkerActions::new(repository.clone()).mount(&worker)?;
    render_session(outcome.session());
    eprintln!("Mounted {worker} in kitty tab {}", outcome.tab_id());
    Ok(())
}

fn rebase_command(repository: &Repository, value: &str) -> Result<()> {
    let worker = normalize_worker(value)?;
    let outcome = WorkerActions::new(repository.clone()).rebase(&worker)?;
    eprintln!("Rebased {}", outcome.branch());
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

fn orchestrate_command(repository: &Repository, parent: &str, model: Option<&str>) -> Result<()> {
    let id = TicketId::parse(parent.to_owned())?;
    let request = wsg_core::OrchestrationRequest::new(id.clone(), configured_runtime(repository));
    let request = model.map_or(request.clone(), |value| request.with_model(value));
    let options = wsg_core::OrchestrationOptions::new();
    let summary = repository
        .orchestration_runner()
        .run(&request, &options, |event| {
            render_orchestration_event(&event);
        })?;
    render_orchestration_event(&OrchestrationEvent::Terminal(summary));
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
                "Orchestration complete: {} done, {} failed, {} skipped",
                counts.done(),
                counts.failed(),
                counts.skipped()
            );
        }
    }
}

const ZSH_COMPLETION: &str = r#"#compdef wsg
_wsg() {
  local -a commands
  commands=(add rm list clean root where path refresh pool dispatch send review mount reset status logs completion rebase open-pr help)
  _describe 'command' commands
}
compdef _wsg wsg
"#;

fn default_command() -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        print!("{HELP}");
        return Ok(());
    }
    let repository = Repository::open(".").map_err(|_| anyhow::anyhow!("Not in a jj repo"))?;
    if repository.worker_pool().snapshot().pool().is_none() {
        print!("{HELP}");
        return Ok(());
    }
    pool_list_command(&repository)
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
                model: Some("opus".to_owned()),
                budget: Some(12),
                label: "ready-for-agent".to_owned(),
                no_orchestrate: true,
            })
        );
    }

    #[test]
    fn parser_rejects_unknown_dispatch_options_and_budgeted_orchestration() {
        assert!(parse(&args(&["dispatch", "AMBA-42", "--wat"])).is_err());
        assert!(parse(&args(&["dispatch", "AMBA-42", "--budget", "12"])).is_err());
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
            "Pool has 2 idle worker(s) but 5 ticket(s) to dispatch. Resize pool to 5? [Y/n] "
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
                model: None,
                budget: None,
                label: "needs-review".to_owned(),
                no_orchestrate: false,
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
                model: None,
                budget: None,
                label: "ready-for-agent".to_owned(),
                no_orchestrate: true,
            })
        );
        assert_eq!(
            parse(&args(&["rb", "worker-1"])).unwrap(),
            Command::Rebase {
                worker: "worker-1".to_owned(),
            }
        );
    }
}
