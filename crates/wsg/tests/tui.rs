#![cfg(unix)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

#[test]
#[ignore = "interactive PTY test; run `mise run test-deep`"]
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
#[ignore = "interactive PTY test; run `mise run test-deep`"]
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
#[ignore = "interactive PTY test; run `mise run test-deep`"]
fn sigterm_in_the_shared_tui_restores_the_terminal() {
    assert_signal_restores_the_terminal("TERM");
}

#[test]
#[ignore = "interactive PTY test; run `mise run test-deep`"]
fn sigint_in_the_shared_tui_restores_the_terminal() {
    assert_signal_restores_the_terminal("INT");
}

#[test]
#[ignore = "interactive PTY test; run `mise run test-deep`"]
fn sighup_in_the_shared_tui_restores_the_terminal() {
    assert_signal_restores_the_terminal("HUP");
}

/// Every signal whose default action would strand the terminal must restore it.
/// One case per signal pins the registered list, not merely the mechanism.
fn assert_signal_restores_the_terminal(signal: &str) {
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("PTY should open");
    let mut command = CommandBuilder::new(std::env::current_exe().expect("test binary path"));
    command.args(["--exact", "signal_helper", "--nocapture"]);
    command.env("JJFX_SIGNAL_HELPER", "1");
    let mut child = pty
        .slave
        .spawn_command(command)
        .expect("signal helper should spawn in the PTY");
    drop(pty.slave);
    drop(pty.master.take_writer().expect("PTY writer should open"));
    let process_id = child
        .process_id()
        .expect("signal helper should have a process id");

    // Read incrementally so the signal lands *after* the alternate screen is
    // entered (signalling first would prove nothing), and drain until the helper
    // has restored and exited.
    let mut reader = pty
        .master
        .try_clone_reader()
        .expect("PTY reader should open");
    let (chunk_tx, chunk_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match std::io::Read::read(&mut reader, &mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if chunk_tx.send(buffer[..read].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut output = Vec::new();
    let mut signalled = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        match chunk_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => output.extend_from_slice(&chunk),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        let text = String::from_utf8_lossy(&output);
        if !signalled && text.contains("\u{1b}[?1049h") {
            signalled = true;
            let status = Command::new("kill")
                .args([&format!("-{signal}"), &process_id.to_string()])
                .status()
                .expect("kill should run");
            assert!(
                status.success(),
                "SIG{signal} should be delivered to the helper"
            );
        }
        if signalled && text.contains("\u{1b}[?1049l") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the helper never restored the terminal: {text}"
        );
    }

    let status = child.wait().expect("signal helper should exit");
    let output = String::from_utf8_lossy(&output).into_owned();
    assert!(
        output.contains("\u{1b}[?1049h"),
        "the helper should have entered the alternate screen: {output}"
    );
    assert!(
        output.contains("\u{1b}[?1049l"),
        "SIG{signal} should leave the alternate screen: {output}"
    );
    assert!(
        !status.success(),
        "SIG{signal} should terminate the helper, not exit cleanly: {status:?}"
    );
}

#[test]
fn signal_helper() {
    if std::env::var_os("JJFX_SIGNAL_HELPER").is_some() {
        jjfx::test_support::block_after_tui_init();
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
#[ignore = "interactive PTY test; run `mise run test-deep`"]
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
            let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
            let release = target.join("release/jjfx");
            if release.is_file() {
                release
            } else {
                target.join("debug/jjfx")
            }
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
