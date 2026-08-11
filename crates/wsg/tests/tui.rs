#![cfg(unix)]

use std::io::Write;
use std::time::Duration;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

#[test]
fn interactive_wsg_without_arguments_enters_the_shared_tui() {
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
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_wsg"));
    command.cwd(repository.path());
    command.env("HOME", repository.path());
    command.env("XDG_CONFIG_HOME", repository.path().join("config"));
    let mut child = pty
        .slave
        .spawn_command(command)
        .expect("wsg should spawn in the PTY");
    drop(pty.slave);

    let mut writer = pty.master.take_writer().expect("PTY writer should open");
    writer
        .write_all(b"q")
        .expect("quit should be delivered to wsg");
    drop(writer);

    let output = read_pty_output(pty.master.as_ref());
    let status = child.wait().expect("wsg should exit after q");
    assert!(status.success(), "wsg exited with {status:?}: {output}");
    assert!(
        output.contains("\u{1b}[?1049h"),
        "interactive wsg should enter the alternate screen: {output}"
    );
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
