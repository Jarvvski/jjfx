//! `jjfx help` at the surface a reader actually reaches: the built binary.
//!
//! Help is repo-independent, so these tests run it directly and read what it
//! prints. The bug they pin is that a two-word group read off the top-level list
//! (`jjfx ticket list`) could not itself be asked about: `jjfx help ticket`
//! answered "unknown command".

use std::process::{Command, Output};

fn help(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jjfx"))
        .arg("help")
        .args(args)
        .output()
        .expect("jjfx help should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The reported bug: the group itself answers with its subcommands instead of
/// "unknown command".
#[test]
fn help_for_a_multi_word_group_lists_its_subcommands() {
    let output = help(&["ticket"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("ticket list"), "{text}");
    assert!(text.contains("ticket reconcile"), "{text}");
    assert!(!text.contains("unknown command"), "{text}");
}

/// The behaviour that already worked must keep working.
#[test]
fn help_for_a_group_subcommand_prints_the_subcommand_page() {
    let output = help(&["ticket", "list"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("Usage: jjfx ticket list"), "{text}");
    assert!(text.contains("--json"), "{text}");
}

/// `pool` is a command and a group: its page renders, and it lists the
/// subcommands a reader would otherwise have to find in the top-level list.
#[test]
fn help_for_pool_prints_the_pool_page_and_its_subcommands() {
    let output = help(&["pool"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("Usage: jjfx pool"), "{text}");
    assert!(text.contains("pool create"), "{text}");
    assert!(text.contains("pool profile"), "{text}");
}

/// Another group with no command of its own resolves the same way.
#[test]
fn help_for_hooks_lists_its_subcommands() {
    let output = help(&["hooks"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("hooks install"), "{text}");
    assert!(text.contains("hooks status"), "{text}");
}

/// A genuinely unknown word is still a loud error naming it.
#[test]
fn help_for_an_unknown_group_fails_naming_it() {
    let output = help(&["definitely-not-a-group"]);
    assert!(!output.status.success(), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("unknown command: definitely-not-a-group"),
        "{}",
        stderr(&output)
    );
}

/// The `review` page must not claim a bounded loop the binary does not run.
///
/// `--fg` used to advertise "the full bounded review loop in the foreground",
/// but no command drives `ReviewLoop` (dogfood-2/04): jjfx has no reviewer of
/// its own, so a review sends one prompt and produces no findings. The page now
/// says so, where an operator reading `jjfx help review` sees it.
#[test]
fn help_for_review_does_not_claim_a_bounded_loop_it_does_not_run() {
    let output = help(&["review"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);

    assert!(
        !text.contains("full bounded review loop"),
        "the page still claims a loop no command drives: {text}"
    );
    assert!(
        text.contains("ReviewLoop"),
        "the page must name the loop whose caller is missing: {text}"
    );
    assert!(
        text.contains("no reviewer and no bounded loop"),
        "the page must state that review runs no bounded loop: {text}"
    );
    assert!(
        text.contains("harness, which is their only caller"),
        "the page must state that the loop's only caller is the harness: {text}"
    );
}
