#[path = "../../../src/cli.rs"]
mod cli;

fn main() {
    let result = cli::run(
        std::env::args().skip(1).collect(),
        jjfx::launch,
        jjfx::terminal_settings,
    );
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
