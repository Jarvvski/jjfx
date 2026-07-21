use anyhow::{Context, bail};
use wsg_core::{MigrationCapabilities, Repository};

const HELP: &str = "wsg - Workspace Dispatch command line interface\n\nUsage: wsg [OPTIONS]\n\nOptions:\n  -h, --help       Print this help message\n  -V, --version    Print version information\n";

fn main() {
    if let Err(error) = run() {
        eprintln!("wsg: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--help") | Some("-h") => {
            print!("{HELP}");
            return Ok(());
        }
        Some("--version") | Some("-V") => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(argument) => bail!("unrecognized argument: {argument}"),
        None => {}
    }

    let repository = Repository::open(".").context("opening the current repository")?;
    match repository.migration_capabilities() {
        MigrationCapabilities::NotImplemented => println!(
            "Workspace Dispatch migration capabilities are not implemented for {}",
            repository.root().display()
        ),
    }
    Ok(())
}
