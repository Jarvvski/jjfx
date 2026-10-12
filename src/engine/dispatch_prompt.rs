//! Typed, provider-neutral prompt construction for Workspace Dispatch.

use thiserror::Error;

use crate::engine::{
    AgentModel, AgentRuntime, AgentRuntimeInvocation, DispatchDependencyContext,
    RepositoryIdentity, Ticket, TicketDetails,
};

/// Delivery obligations supplied by a Dispatch caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryContract {
    assignee: String,
    branch_prefix: String,
    pull_request_command: String,
}

impl DeliveryContract {
    /// Creates the assignee, bookmark namespace, and Pull Request command contract.
    pub fn new(
        assignee: impl Into<String>,
        branch_prefix: impl Into<String>,
        pull_request_command: impl Into<String>,
    ) -> Result<Self, DispatchPromptError> {
        Ok(Self {
            assignee: required(assignee, "assignee")?,
            branch_prefix: required(branch_prefix, "branch prefix")?,
            pull_request_command: required(pull_request_command, "Pull Request command")?,
        })
    }
}

/// Provider-owned spending behavior or an explicit supported maximum.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DispatchBudget {
    /// Leave token and spending behavior to the selected Agent Runtime.
    #[default]
    ProviderManaged,
    /// Stop a supporting Agent Runtime after this many US dollars.
    MaximumUsd(u32),
}

impl DispatchBudget {
    /// Creates a positive maximum-spend override.
    pub fn maximum_usd(dollars: u32) -> Result<Self, DispatchPromptError> {
        if dollars == 0 {
            return Err(DispatchPromptError::InvalidBudget);
        }
        Ok(Self::MaximumUsd(dollars))
    }
}

/// Typed inputs for one initial Direct Dispatch prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchPromptContext {
    runtime: AgentRuntime,
    repository: RepositoryIdentity,
    ticket: Ticket,
    delivery: DeliveryContract,
    model: Option<AgentModel>,
    budget: DispatchBudget,
    dependency_context: Option<DispatchDependencyContext>,
    ticket_details: Option<TicketDetails>,
}

impl DispatchPromptContext {
    /// Creates prompt inputs using provider-owned model and budget behavior.
    pub fn new(
        runtime: AgentRuntime,
        repository: RepositoryIdentity,
        ticket: Ticket,
        delivery: DeliveryContract,
    ) -> Self {
        Self {
            runtime,
            repository,
            ticket,
            delivery,
            model: None,
            budget: DispatchBudget::ProviderManaged,
            dependency_context: None,
            ticket_details: None,
        }
    }

    /// Supplies the fetched Ticket title and description to inline in the prompt.
    ///
    /// When present, the Worker no longer needs tracker tools just to read the
    /// Ticket, and status updates become best-effort.
    pub fn with_ticket_details(mut self, details: TicketDetails) -> Self {
        self.ticket_details = Some(details);
        self
    }

    /// Supplies a caller-selected model override.
    pub fn with_model(mut self, model: impl Into<AgentModel>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Supplies a caller-selected spending override.
    pub fn with_budget(mut self, budget: DispatchBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Supplies stacked-branch obligations derived from Ticket Dependencies.
    pub fn with_dependency_context(mut self, context: DispatchDependencyContext) -> Self {
        self.dependency_context = Some(context);
        self
    }
}

/// Builds complete Agent Runtime invocations without exposing provider flags or prose layout.
#[derive(Debug, Default, Clone, Copy)]
pub struct DispatchPromptBuilder;

impl DispatchPromptBuilder {
    /// Creates a Dispatch prompt builder.
    pub const fn new() -> Self {
        Self
    }

    /// Builds the initial invocation for one Ticket.
    pub fn initial(
        &self,
        context: DispatchPromptContext,
    ) -> Result<AgentRuntimeInvocation, DispatchPromptError> {
        let ticket_lower = context.ticket.id().as_str().to_ascii_lowercase();
        let mut system_prompt = format!(
            "You are an autonomous implementation agent in a jj (Jujutsu VCS) workspace.\n\nCRITICAL RULES:\n- Use jj commands, NEVER git commands.\n- The gh CLI requires: gh -R {} pr create ...\n- Branch naming: {}/{}-<short-description> (lowercase, hyphens, max 4 words from the Ticket title).\n- To push your work: jj git push --named <branch>=@\n- Use Linear tools to update status only when they are available; never block on their absence.\n- Do NOT ask questions. Make reasonable decisions and proceed.\n- If you encounter ambiguity, document your assumptions in the PR description.\n- Do NOT add any AI attribution to PRs, commits, or comments.",
            context.repository.as_str(),
            context.delivery.branch_prefix,
            ticket_lower,
        );
        if let Some(dependency) = context
            .dependency_context
            .as_ref()
            .filter(|dependency| !dependency.description().trim().is_empty())
        {
            system_prompt.push_str(&format!(
                "\n\nSTACKED BRANCH: Your Workspace is based on prerequisite work:\n{}\n\nCRITICAL: Do NOT rebase onto main. Your changes build on the prerequisite branch or branches.",
                dependency.description()
            ));
        }
        // When the Ticket body was fetched at dispatch time, inline it and make
        // every tracker interaction best-effort so a runtime without Linear
        // tools can still implement and deliver.
        let mut worker_prompt = format!(
            "Implement Linear Ticket {}: {}.\n\n",
            context.ticket.id(),
            context.ticket.title().as_str(),
        );
        if let Some(details) = context.ticket_details.as_ref() {
            worker_prompt.push_str("TICKET DETAILS:\n");
            if details.description().trim().is_empty() {
                worker_prompt.push_str("(the tracker returned no description)\n");
            } else {
                worker_prompt.push_str(details.description().trim());
                worker_prompt.push('\n');
            }
            worker_prompt.push_str(
                "\n1. The Ticket title and description are provided above; do not spend time re-fetching them.\n",
            );
            worker_prompt.push_str(&format!(
                "2. If Linear delivery tools are available, claim the Ticket by moving it to In Progress and assigning {}; otherwise proceed without them.\n",
                context.delivery.assignee,
            ));
        } else {
            worker_prompt.push_str(
                "1. Fetch the Ticket through Linear tools and verify its acceptance criteria.\n",
            );
            worker_prompt.push_str(&format!(
                "2. Claim it by moving it to In Progress and assigning {}.\n",
                context.delivery.assignee,
            ));
        }
        worker_prompt.push_str(&format!(
            "3. Derive a bookmark beginning {}/{ticket_lower}.\n4. Read AGENTS.md, CLAUDE.md, or equivalent repository instructions and the relevant source.\n5. Implement with the repository's TDD workflow, repeating red-green cycles until the acceptance criteria are met.\n6. Run the full lint, type-check, build, and test suite and fix every failure.\n7. Describe the change with jj describe.\n8. Push with jj git push.\n9. Create the Pull Request with: {}\n",
            context.delivery.branch_prefix,
            context.delivery.pull_request_command,
        ));
        if context.ticket_details.is_some() {
            worker_prompt.push_str(&format!(
                "10. If Linear delivery tools are available, move {} to Reviewable and add a Linear comment summarizing the implementation, PR URL, and assumptions; otherwise note the summary in the Pull Request.\n",
                context.ticket.id(),
            ));
        } else {
            worker_prompt.push_str(&format!(
                "10. Move {} to Reviewable and add a Linear comment summarizing the implementation, PR URL, and assumptions.\n",
                context.ticket.id(),
            ));
        }
        let mut invocation =
            AgentRuntimeInvocation::new(worker_prompt).with_system_prompt(system_prompt);
        invocation = invocation.with_ticket_delivery_profile();
        if let Some(model) = context
            .model
            .filter(|model| !model.model().trim().is_empty())
        {
            invocation = invocation.with_model(model);
        }
        if let DispatchBudget::MaximumUsd(dollars) = context.budget {
            if context.runtime != AgentRuntime::Claude {
                return Err(DispatchPromptError::UnsupportedBudget {
                    runtime: context.runtime,
                });
            }
            invocation = invocation.with_max_budget_usd(dollars);
        }
        Ok(invocation)
    }
}

fn required(value: impl Into<String>, field: &'static str) -> Result<String, DispatchPromptError> {
    let value = value.into().trim().to_owned();
    if value.is_empty() {
        return Err(DispatchPromptError::MissingDeliveryField { field });
    }
    Ok(value)
}

/// Invalid or unsupported Dispatch prompt inputs.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DispatchPromptError {
    /// A required delivery obligation was blank.
    #[error("Dispatch delivery contract requires {field}")]
    MissingDeliveryField {
        /// Missing delivery field.
        field: &'static str,
    },
    /// A maximum budget must be positive.
    #[error("Dispatch maximum budget must be greater than zero")]
    InvalidBudget,
    /// The selected runtime has no supported spending override.
    #[error("{runtime} does not support a Dispatch spending override")]
    UnsupportedBudget {
        /// Runtime selected for the invocation.
        runtime: AgentRuntime,
    },
}
