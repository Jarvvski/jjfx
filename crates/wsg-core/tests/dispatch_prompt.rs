use std::ffi::OsStr;

use wsg_core::{
    AgentRuntime, AgentRuntimeCapabilities, AgentRuntimeInvocation, DeliveryContract,
    DispatchBudget, DispatchPromptBuilder, DispatchPromptContext, RepositoryIdentity, Ticket,
    TicketId, TicketStatus, TicketTitle,
};

#[test]
fn fresh_and_resumed_agent_sessions_receive_the_same_delegation_contract() {
    for runtime in [AgentRuntime::Claude, AgentRuntime::Codex] {
        let fresh = DispatchPromptBuilder::new()
            .initial(dispatch_context(runtime))
            .expect("fresh Dispatch prompt");
        let resumed = AgentRuntimeInvocation::new("continue the Ticket")
            .with_session_id("session-42");

        for invocation in [fresh, resumed] {
            let command = runtime.command(&invocation, AgentRuntimeCapabilities::default());
            let prompt = command
                .get_args()
                .map(OsStr::to_string_lossy)
                .collect::<Vec<_>>()
                .join("\n");

            assert!(prompt.contains("Delegated work is read-only"));
            assert!(prompt.contains("not to edit tracked files or run jj commands"));
            assert!(prompt.contains("Do not use detached sessions, nested delegation"));
            assert!(prompt.contains("Await all delegated work before finishing"));
            assert!(prompt.contains("The main agent alone owns tracked edits"));
        }
    }
}

#[test]
fn provider_managed_model_and_budget_add_no_command_overrides() {
    let context = dispatch_context(AgentRuntime::Codex);

    let invocation = DispatchPromptBuilder::new()
        .initial(context)
        .expect("provider-managed Dispatch prompt");
    let command = AgentRuntime::Codex.command(&invocation, AgentRuntimeCapabilities::default());
    let args = command
        .get_args()
        .map(OsStr::to_string_lossy)
        .collect::<Vec<_>>();

    assert!(!args.iter().any(|argument| argument == "--model"));
    assert!(!args.iter().any(|argument| argument == "--max-budget-usd"));
}

#[test]
fn unsupported_budget_override_is_rejected_before_invocation() {
    let context = dispatch_context(AgentRuntime::Codex)
        .with_budget(DispatchBudget::maximum_usd(12).expect("Dispatch budget"));

    let error = DispatchPromptBuilder::new()
        .initial(context)
        .expect_err("Codex has no supported spending override");

    assert_eq!(
        error.to_string(),
        "codex does not support a Dispatch spending override"
    );
}

#[test]
fn initial_dispatch_prompt_pins_delivery_obligations_and_supported_overrides() {
    let context = dispatch_context(AgentRuntime::Claude)
    .with_model("opus")
    .with_budget(DispatchBudget::maximum_usd(12).expect("Dispatch budget"));

    let invocation = DispatchPromptBuilder::new()
        .initial(context)
        .expect("initial Dispatch prompt");
    let command = AgentRuntime::Claude.command(&invocation, AgentRuntimeCapabilities::default());
    let args = command
        .get_args()
        .map(OsStr::to_string_lossy)
        .collect::<Vec<_>>();
    let prompt = args.join("\n");

    assert!(prompt.contains("owner/repo"));
    assert!(prompt.contains("AMBA-42"));
    assert!(prompt.contains("Ship typed discovery"));
    assert!(prompt.contains("owner@example.com"));
    assert!(prompt.contains("adam/amba-42"));
    assert!(prompt.contains("AGENTS.md"));
    assert!(prompt.contains("TDD"));
    assert!(prompt.contains("jj describe"));
    assert!(prompt.contains("jj git push"));
    assert!(prompt.contains("gh -R owner/repo pr create --fill"));
    assert!(prompt.contains("Reviewable"));
    assert!(prompt.contains("Do NOT add any AI attribution"));
    assert!(args.windows(2).any(|pair| pair == ["--model", "opus"]));
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--max-budget-usd", "12"])
    );
}

fn dispatch_context(runtime: AgentRuntime) -> DispatchPromptContext {
    let ticket = Ticket::new(
        TicketId::parse("AMBA-42").expect("Ticket ID"),
        TicketTitle::parse("Ship typed discovery").expect("Ticket title"),
        TicketStatus::parse("Todo").expect("Ticket status"),
    );
    let delivery = DeliveryContract::new(
        "owner@example.com",
        "adam",
        "gh -R owner/repo pr create --fill",
    )
    .expect("delivery contract");
    DispatchPromptContext::new(
        runtime,
        RepositoryIdentity::parse("owner/repo").expect("Repository identity"),
        ticket,
        delivery,
    )
}
