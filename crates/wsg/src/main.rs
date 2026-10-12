// The compatibility binary compiles jjfx's CLI router directly (the `#[path]`
// include below) so `env!("CARGO_PKG_NAME")` reads "wsg" in its help and usage
// text. That router resolves the engine through `crate::engine`, so re-export
// jjfx's engine module at this crate's root to give those paths a target.
pub use jjfx::engine;

#[path = "../../../src/cli.rs"]
mod cli;

fn main() {
    let result = cli::run(
        std::env::args().skip(1).collect(),
        jjfx::launch,
        jjfx::terminal_settings,
        jjfx::colocate_setting,
    );
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
