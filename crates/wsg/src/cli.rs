use anyhow::{Result, bail};
use wsg_core::{MigrationCapabilities, Repository};

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
        command => bail!("command is not implemented yet: {command:?}"),
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

fn default_command() -> Result<()> {
    let repository =
        Repository::open(".").map_err(|error| anyhow::anyhow!("Not in a jj repo: {error}"))?;
    match repository.migration_capabilities() {
        MigrationCapabilities::ReadOnlyWorkerPool => println!(
            "Workspace Dispatch read-only Worker Pool snapshots available for {}",
            repository.root().display()
        ),
        MigrationCapabilities::NotImplemented => println!(
            "Workspace Dispatch migration capabilities are not implemented for {}",
            repository.root().display()
        ),
    }
    Ok(())
}
