#![cfg(unix)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

#[test]
fn interactive_wsg_without_arguments_enters_the_shared_tui() {
    let output = run_interactive(env!("CARGO_BIN_EXE_wsg"), "wsg", &[]);
    assert!(
        output.contains("\u{1b}[?1049h") && output.contains("\u{1b}[?1049l"),
        "interactive wsg should enter and leave the alternate screen: {output}"
    );
    assert!(
        output.contains("jjfx -"),
        "interactive wsg should render the shared jjfx TUI: {output}"
    );
}

#[test]
fn panic_in_the_shared_tui_restores_the_terminal() {
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("PTY should open");
    let mut command = CommandBuilder::new(std::env::current_exe().expect("test binary path"));
    command.args(["--exact", "panic_helper", "--nocapture"]);
    command.env("JJFX_PANIC_HELPER", "1");
    let mut child = pty
        .slave
        .spawn_command(command)
        .expect("panic helper should spawn in the PTY");
    drop(pty.slave);
    drop(pty.master.take_writer().expect("PTY writer should open"));

    let output = read_pty_output(pty.master.as_ref());
    let status = child.wait().expect("panic helper should exit");
    assert!(!status.success(), "panic helper should fail: {output}");
    assert!(
        output.contains("\u{1b}[?1049h") && output.contains("\u{1b}[?1049l"),
        "panic should enter and leave the alternate screen: {output}"
    );
}

#[test]
fn panic_helper() {
    if std::env::var_os("JJFX_PANIC_HELPER").is_some() {
        jjfx::test_support::panic_after_tui_init();
    }
}

#[test]
fn jjfx_without_arguments_prints_cli_help() {
    let output = Command::new(jjfx_binary())
        .output()
        .expect("jjfx should run without arguments");
    assert!(
        output.status.success(),
        "jjfx help should succeed: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Usage: jjfx [OPTIONS]"),
        "unexpected help: {stdout}"
    );
    assert!(!stdout.contains("\u{1b}[?1049h"));
}

#[test]
fn interactive_jjfx_enters_the_same_tui() {
    let binary = jjfx_binary();
    let output = run_interactive(&binary, "jjfx", &["tui"]);
    assert!(
        output.contains("\u{1b}[?1049h") && output.contains("\u{1b}[?1049l"),
        "interactive jjfx should enter and leave the alternate screen: {output}"
    );
    assert!(
        output.contains("jjfx -"),
        "interactive jjfx should render the shared TUI: {output}"
    );
}

fn jjfx_binary() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_jjfx")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/debug/jjfx")
                .to_path_buf()
        })
}

fn run_interactive(binary: impl AsRef<Path>, name: &str, args: &[&str]) -> String {
    let repository = tempfile::tempdir().expect("temporary repository should be created");
    std::fs::create_dir(repository.path().join(".jj"))
        .expect("repository marker should be created");

    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("PTY should open");
    let mut command = CommandBuilder::new(binary.as_ref());
    command.args(args);
    command.cwd(repository.path());
    command.env("HOME", repository.path());
    command.env("XDG_CONFIG_HOME", repository.path().join("config"));
    let mut child = pty
        .slave
        .spawn_command(command)
        .unwrap_or_else(|error| panic!("{name} should spawn in the PTY: {error}"));
    drop(pty.slave);

    let mut writer = pty.master.take_writer().expect("PTY writer should open");
    writer
        .write_all(b"q")
        .unwrap_or_else(|error| panic!("quit should be delivered to {name}: {error}"));
    drop(writer);

    let output = read_pty_output(pty.master.as_ref());
    let status = child
        .wait()
        .unwrap_or_else(|error| panic!("{name} should exit after q: {error}"));
    assert!(status.success(), "{name} exited with {status:?}: {output}");
    output
}

fn read_pty_output(master: &dyn portable_pty::MasterPty) -> String {
    let mut reader = master.try_clone_reader().expect("PTY reader should open");
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = std::io::Read::read_to_end(&mut reader, &mut output);
        let _ = sender.send(String::from_utf8_lossy(&output).into_owned());
    });
    receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("PTY output should close")
}
