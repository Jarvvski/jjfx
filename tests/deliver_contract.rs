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

/// The criterion `dogfood-3` finding 02 states: the dispatch prompt, `jjfx help`
/// and the skill agree on what an agent does when its Pull Request conflicts
/// after a sibling merges.
///
/// The answer is the change's **bookmark**, because a Worker is a reusable slot
/// and the change it delivered is not: the same change is re-welded by naming
/// the bookmark, and the conflict names the Workspace to resolve in.
#[test]
fn the_prompt_help_and_skill_agree_on_re_welding_a_conflicted_change() {
    let prompt = dispatch_prompt();
    let rebase = help("rebase");
    let deliver = help("deliver");
    let skill = include_str!("../skills/jjfx-dispatch/SKILL.md");
    let playbook = include_str!("../skills/jjfx-dispatch/PLAYBOOK.md");
    let configuration = include_str!("../docs/configuration.md");

    // The prompt tells the Worker the durable name is the bookmark, and names
    // the command that re-welds by it.
    assert!(prompt.contains("durable name"), "{prompt}");
    assert!(
        prompt.contains("conflicts after a sibling merged"),
        "the prompt must say what to do when a sibling merges first: {prompt}"
    );
    assert!(
        prompt.contains("jjfx rebase <your bookmark>"),
        "the prompt must name the re-weld command: {prompt}"
    );

    // Both verbs accept the bookmark, and say so.
    for (page, text) in [("rebase", &rebase), ("deliver", &deliver)] {
        assert!(
            text.contains("<worker|branch>"),
            "`jjfx help {page}` must accept a bookmark: {text}"
        );
        assert!(
            text.contains("bookmark is the durable name"),
            "`jjfx help {page}` must say which name survives: {text}"
        );
        assert!(
            text.contains("sibling merged"),
            "`jjfx help {page}` must name the case: {text}"
        );
    }
    assert!(
        rebase.contains("jjfx resolve <workspace>"),
        "the rebase page must name the resolve that follows a conflict: {rebase}"
    );
    assert!(
        deliver.contains("jjfx resolve <workspace>"),
        "the deliver page must name the resolve that follows a conflict: {deliver}"
    );

    // The skill an agent reads says the same, and so does the playbook.
    for (name, text) in [("skill", skill), ("playbook", playbook)] {
        assert!(
            text.contains("jjfx rebase adam/tally--04-json-output"),
            "the {name} must show the re-weld by bookmark: {text}"
        );
        assert!(
            text.contains("durable name"),
            "the {name} must say which name survives: {text}"
        );
    }

    // The configuration page documents the rule the verbs implement.
    assert!(
        configuration.contains("### Naming the change: a Worker or its bookmark"),
        "docs/configuration.md must document the two names"
    );
    assert!(
        configuration.contains("branch-adam-tally--04-json-output"),
        "docs/configuration.md must name the Workspace jjfx creates"
    );
}
