//! Selecting the configured Ticket source for the shared CLI and TUI paths.
//!
//! The source is a `[tickets]` config decision, not a function of the Agent
//! Runtime: a runtime with no native tracker can still pull work through a
//! configured source, and a new tracker needs no runtime-shaped path. The
//! selected source is a plain [`TicketQuery`], so `TicketDiscovery` and the
//! orchestration runner stay generic over it.

use std::path::Path;

use crate::config::{TicketSource, TicketsConfig};
use crate::engine::{
    AgentRuntime, AgentRuntimeQuery, LocalMarkdownQuery, LocalTickets, PI_DISCOVERY_HELPER_ENV,
    PiDiscoveryHelper, Repository, TicketQuery, TicketQueryError, WorkerPoolSnapshot,
};

/// Builds the [`TicketQuery`] selected by `[tickets] source`, independent of the
/// Agent Runtime running the pool.
///
/// Fails for a source this build cannot construct, so the setup error surfaces
/// before any Worker reservation or Pool mutation.
pub(crate) fn configured_ticket_query(
    workspace: &Path,
    tickets: &TicketsConfig,
    runtime: AgentRuntime,
) -> Result<Box<dyn TicketQuery>, TicketQueryError> {
    match tickets.source {
        TicketSource::Linear => Ok(Box::new(linear_query(workspace, runtime))),
        TicketSource::Local => Ok(Box::new(LocalMarkdownQuery::new(
            workspace.join(&tickets.local.root),
            tickets.local.ready_status.clone(),
            tickets.local.resolved_status.clone(),
        ))),
    }
}

/// The `source = "linear"` query: the Agent Runtime's native Linear access,
/// unchanged from the runtime-shaped path it replaces.
fn linear_query(workspace: &Path, runtime: AgentRuntime) -> AgentRuntimeQuery {
    let query = AgentRuntimeQuery::new(runtime, workspace);
    if runtime != AgentRuntime::Pi {
        return query;
    }
    match std::env::var_os(PI_DISCOVERY_HELPER_ENV).filter(|executable| !executable.is_empty()) {
        Some(executable) => query.with_pi_helper(PiDiscoveryHelper::new(executable)),
        None => query,
    }
}

/// Writes local Tickets back for the reconciled Worker Pool: a Worker running a
/// local Ticket claims it, and a Worker whose Run finished resolves it.
///
/// A no-op for every other source. The failures are returned rather than raised:
/// the writeback is a side effect of a Dispatch that already succeeded, and the
/// caller decides how loudly to report it.
pub(crate) fn apply_local_writeback(
    repository: &Repository,
    snapshot: &WorkerPoolSnapshot,
    tickets: &TicketsConfig,
) -> Vec<String> {
    if tickets.source != TicketSource::Local {
        return Vec::new();
    }
    let local = LocalTickets::new(
        repository.root().join(&tickets.local.root),
        tickets.local.ready_status.clone(),
        tickets.local.resolved_status.clone(),
    );
    let workers = snapshot
        .workers()
        .iter()
        .filter_map(|worker| worker.ticket().map(|ticket| (ticket, worker.status())))
        .collect::<Vec<_>>();
    local
        .write_back(&workers)
        .into_iter()
        .map(|error| error.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::engine::{
        ParentTicket, ReadyTicketFilter, RepositoryIdentity, TicketDiscovery, TicketId,
        TicketQueryRequest, TicketStatus,
    };

    fn linear() -> TicketsConfig {
        TicketsConfig {
            source: TicketSource::Linear,
            ..TicketsConfig::default()
        }
    }

    fn local() -> TicketsConfig {
        TicketsConfig {
            source: TicketSource::Local,
            ..TicketsConfig::default()
        }
    }

    #[test]
    fn the_linear_source_is_byte_for_byte_the_agent_runtime_query() {
        let workspace = Path::new("/tmp/jjfx-ticket-source");
        // The runtime-shaped query the linear source replaced, unchanged: the
        // source decision moved to config, not the query itself.
        for runtime in [
            AgentRuntime::Claude,
            AgentRuntime::Codex,
            AgentRuntime::OpenCode,
        ] {
            assert_eq!(
                linear_query(workspace, runtime),
                AgentRuntimeQuery::new(runtime, workspace),
                "{runtime}"
            );
        }
    }

    #[test]
    fn the_configured_linear_source_is_constructible_for_every_runtime() {
        let workspace = Path::new("/tmp/jjfx-ticket-source");
        for runtime in [
            AgentRuntime::Claude,
            AgentRuntime::Codex,
            AgentRuntime::OpenCode,
            AgentRuntime::Pi,
        ] {
            assert!(
                configured_ticket_query(workspace, &linear(), runtime).is_ok(),
                "{runtime}"
            );
        }
    }

    #[test]
    fn source_selection_is_independent_of_the_runtime() {
        let workspace = Path::new("/tmp/jjfx-ticket-source");
        // The chosen implementation follows config alone: `local` is selected
        // for every runtime, not just a runtime-shaped subset.
        for runtime in [
            AgentRuntime::Claude,
            AgentRuntime::Codex,
            AgentRuntime::OpenCode,
            AgentRuntime::Pi,
        ] {
            assert!(
                configured_ticket_query(workspace, &local(), runtime).is_ok(),
                "{runtime}"
            );
        }
    }

    #[test]
    fn an_absent_tickets_section_selects_the_linear_source() {
        // Backwards compatible: no `[tickets]` still means the Agent Runtime's
        // native Linear discovery.
        let default = TicketsConfig::default();
        assert_eq!(default.source, TicketSource::Linear);
        let workspace = Path::new("/tmp/jjfx-ticket-source");
        for runtime in [
            AgentRuntime::Claude,
            AgentRuntime::Codex,
            AgentRuntime::OpenCode,
        ] {
            assert!(
                configured_ticket_query(workspace, &default, runtime).is_ok(),
                "{runtime}"
            );
        }
    }

    /// A non-runtime `TicketQuery` stands in for a configured source: it proves
    /// discovery, the dependency graph, and ticket details all route through
    /// whichever source is selected, with no Agent Runtime involved.
    struct FakeSource {
        requests: Arc<Mutex<Vec<&'static str>>>,
    }

    impl TicketQuery for FakeSource {
        fn query(&self, request: &TicketQueryRequest) -> Result<String, TicketQueryError> {
            let (kind, response) = match request {
                TicketQueryRequest::ReadyTickets { .. } => (
                    "ready",
                    r#"{"tickets":[{"id":"LOCAL-1","title":"T","status":"Todo","labels":["ready-for-agent"]}]}"#,
                ),
                TicketQueryRequest::DependencyGraph { .. } => ("graph", r#"{"sub_issues":[]}"#),
                TicketQueryRequest::TicketDetails { .. } => {
                    ("details", r#"{"title":"T","description":"body"}"#)
                }
            };
            self.requests.lock().expect("recorded requests").push(kind);
            Ok(response.to_owned())
        }
    }

    #[test]
    fn every_operation_routes_through_the_selected_source() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let source: Box<dyn TicketQuery> = Box::new(FakeSource {
            requests: Arc::clone(&requests),
        });
        let discovery = TicketDiscovery::new(source);
        let filter = ReadyTicketFilter::new(
            "ready-for-agent",
            TicketStatus::parse("Todo").expect("expected status"),
        )
        .expect("Ready Ticket filter");
        let parent = ParentTicket::new(TicketId::parse("LOCAL-1").expect("Parent Ticket ID"));
        let repository = RepositoryIdentity::parse("owner/repo").expect("Repository identity");
        let id = TicketId::parse("LOCAL-1").expect("Ticket ID");

        let ready = discovery
            .ready_tickets(&filter)
            .expect("Ready Ticket discovery");
        assert_eq!(ready.tickets().len(), 1);
        discovery
            .dependency_graph(&parent, &repository)
            .expect("dependency graph");
        discovery.ticket_details(&id).expect("Ticket details");

        assert_eq!(
            *requests.lock().expect("recorded requests"),
            ["ready", "graph", "details"]
        );
    }
}
