//! The delivery contract agrees across the prompt, `jjfx help`, and the skill.
//!
//! The Dispatch prompt tells every Worker that jjfx delivers the change and not
//! to push or open the Pull Request itself. That claim is only true if a command
//! performs it, and the three surfaces an agent reads must name the same one -
//! `jjfx deliver` - while `rebase` and `open-pr` state what they do on their own,
//! so neither is mistaken for the delivery.

use std::ffi::OsStr;
use std::process::Command;

use jjfx::engine::{
    AgentRuntime, AgentRuntimeCapabilities, DeliveryContract, DispatchPromptBuilder,
    DispatchPromptContext, RepositoryIdentity, Ticket, TicketId, TicketStatus, TicketTitle,
};

/// The Dispatch prompt a Worker receives, as the agent runtime is invoked with
/// it.
fn dispatch_prompt() -> String {
    let ticket = Ticket::new(
        TicketId::parse("AMBA-42").expect("Ticket ID"),
        TicketTitle::parse("Ship typed discovery").expect("Ticket title"),
        TicketStatus::parse("Todo").expect("Ticket status"),
    );
    let delivery = DeliveryContract::new("owner@example.com", "adam").expect("delivery contract");
    let context = DispatchPromptContext::new(
        AgentRuntime::Claude,
        RepositoryIdentity::parse("owner/repo").expect("Repository identity"),
        ticket,
        delivery,
    );
    let invocation = DispatchPromptBuilder::new()
        .initial(context)
        .expect("initial Dispatch prompt");
    AgentRuntime::Claude
        .command(&invocation, AgentRuntimeCapabilities::default())
        .get_args()
        .map(OsStr::to_string_lossy)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The rendered `jjfx help <page>`.
fn help(page: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_jjfx"))
        .args(["help", page])
        .output()
        .expect("jjfx help should run");
    assert!(
        output.status.success(),
        "help {page} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The criterion: the prompt, `jjfx help`, and the skill agree on which command
/// performs the delivery, and on what `rebase` / `open-pr` do on their own.
#[test]
fn the_prompt_help_and_skill_name_the_same_delivery_command() {
    let prompt = dispatch_prompt();
    let deliver = help("deliver");
    let skill = include_str!("../skills/jjfx-dispatch/SKILL.md");
    let playbook = include_str!("../skills/jjfx-dispatch/PLAYBOOK.md");

    // The prompt promises jjfx delivers, and names the command that does it.
    assert!(prompt.contains("jjfx delivers the change"), "{prompt}");
    assert!(
        prompt.contains("jjfx deliver"),
        "the prompt must name the delivery verb: {prompt}"
    );

    // The help page is that command, and describes the whole path.
    assert!(deliver.contains("Usage: jjfx deliver"), "{deliver}");
    for phrase in ["weld", "check", "push", "Pull Request"] {
        assert!(
            deliver.contains(phrase),
            "`jjfx help deliver` omits `{phrase}`: {deliver}"
        );
    }

    // The skill an agent reads names the same command, and the playbook too.
    assert!(
        skill.contains("jjfx deliver"),
        "the skill must name the delivery verb"
    );
    assert!(
        playbook.contains("jjfx deliver"),
        "the playbook must name the delivery verb"
    );

    // `rebase` and `open-pr` state their own, narrower scope, and point at the
    // command that delivers.
    let rebase = help("rebase");
    assert!(
        rebase.contains("opens no Pull Request"),
        "rebase must say it opens no Pull Request: {rebase}"
    );
    assert!(
        rebase.contains("deliver"),
        "rebase must point at deliver: {rebase}"
    );
    let open_pr = help("open-pr");
    assert!(
        open_pr.contains("does not create one"),
        "open-pr must say it does not create a Pull Request: {open_pr}"
    );
    assert!(
        open_pr.contains("deliver"),
        "open-pr must point at deliver: {open_pr}"
    );
}
