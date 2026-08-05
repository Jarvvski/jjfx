use std::io::Read;
use std::str::FromStr;

use anyhow::{Result, bail};
use jiff::Timestamp;
use wsg_core::{
    CleanDecision, PoolCapacity, Repository, WorkerActions, WorkerId, WorkerStatus,
    WorkspaceAddOutcome,
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
    Pool(PoolCommand),
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
        "status" => Ok(Command::Status),
        "reset" => parse_single("Usage: wsg reset <worker>", &args[1..])
            .map(|worker| Command::Pool(PoolCommand::Reset { worker })),
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

fn default_command() -> Result<()> {
    let repository = Repository::open(".").map_err(|_| anyhow::anyhow!("Not in a jj repo"))?;
    pool_list_command(&repository)
}
