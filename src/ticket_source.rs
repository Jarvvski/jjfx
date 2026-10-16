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
    AgentRuntime, AgentRuntimeQuery, LocalMarkdownQuery, LocalTickets, Repository, TicketQuery,
    TicketQueryError, WorkerPoolSnapshot, WorkerStatus,
};

/// Builds the [`TicketQuery`] selected by `[tickets] source`, independent of the
/// Agent Runtime running the pool.
///
/// `ready_label` is the global label a local Ticket that declares no `Labels:` of
/// its own is discovered by. It is the Dispatch's global value, never a pool's
/// routing label: the fallback must not make an unlabelled Ticket match every
/// lane a Repository holds.
///
/// Fails for a source this build cannot construct, so the setup error surfaces
/// before any Worker reservation or Pool mutation.
pub(crate) fn configured_ticket_query(
    workspace: &Path,
    tickets: &TicketsConfig,
    runtime: AgentRuntime,
    ready_label: &str,
) -> Result<Box<dyn TicketQuery>, TicketQueryError> {
    match tickets.source {
        TicketSource::Linear => Ok(Box::new(AgentRuntimeQuery::new(runtime, workspace))),
        TicketSource::Local => Ok(Box::new(
            LocalMarkdownQuery::new(
                workspace.join(&tickets.local.root),
                tickets.local.ready_status.clone(),
                tickets.local.resolved_status.clone(),
            )
            .with_ready_label(ready_label),
        )),
    }
}

/// Writes local Tickets back for the reconciled Worker Pool: a Worker running a
/// local Ticket claims it, and a Ticket whose Run finished is resolved.
///
/// The completion signal is the durable Run Record store, not live Worker state:
/// `pool reset` clears a Worker's Ticket association, so a finished Worker that
/// was reset would otherwise never resolve its Ticket. Live `done` Workers are
/// folded in as well, so a Run whose record was never written - a runtime that
/// finalizes lazily - still resolves while its Worker is intact. A no-op for
/// every other source. The failures are returned rather than raised: the
/// writeback is a side effect of a command that already succeeded, and the caller
/// decides how loudly to report it.
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
    let in_flight = snapshot
        .workers()
        .iter()
        .filter(|worker| worker.status() == WorkerStatus::Busy)
        .filter_map(|worker| worker.ticket())
        .collect::<Vec<_>>();
    let mut completed = snapshot
        .workers()
        .iter()
        .filter(|worker| worker.status() == WorkerStatus::Done)
        .filter_map(|worker| worker.ticket().map(str::to_owned))
        .collect::<Vec<_>>();
    let mut failures = Vec::new();
    match repository.state_store().runs().completed_tickets() {
        Ok(recorded) => {
            for ticket in recorded {
                if !completed.contains(&ticket) {
                    completed.push(ticket);
                }
            }
        }
        Err(error) => failures.push(format!("cannot read Run Records: {error}")),
    }
    let completed = completed.iter().map(String::as_str).collect::<Vec<_>>();
    failures.extend(
        local
            .write_back(&in_flight, &completed)
            .into_iter()
            .map(|error| error.to_string()),
    );
    failures
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::engine::{
        DEFAULT_READY_LABEL, ParentTicket, ReadyTicketFilter, RepositoryIdentity, RunRecord,
        RunRecordConclusion, TicketDiscovery, TicketId, TicketQueryRequest, TicketStatus, WorkerId,
    };
    use tempfile::TempDir;

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
    fn the_configured_linear_source_is_constructible_for_every_runtime() {
        let workspace = Path::new("/tmp/jjfx-ticket-source");
        for runtime in [
            AgentRuntime::Claude,
            AgentRuntime::Codex,
            AgentRuntime::OpenCode,
        ] {
            assert!(
                configured_ticket_query(workspace, &linear(), runtime, DEFAULT_READY_LABEL).is_ok(),
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
        ] {
            assert!(
                configured_ticket_query(workspace, &local(), runtime, DEFAULT_READY_LABEL).is_ok(),
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
                configured_ticket_query(workspace, &default, runtime, DEFAULT_READY_LABEL).is_ok(),
                "{runtime}"
            );
        }
    }

    fn write_local_ticket(root: &Path, feature: &str, file: &str, content: &str) {
        let directory = root.join(".scratch").join(feature).join("issues");
        fs::create_dir_all(&directory).expect("issues directory");
        fs::write(directory.join(file), content).expect("Ticket file");
    }

    fn completed_record(ticket: &str) -> RunRecord {
        RunRecord {
            run_id: "worker-1@1".to_owned(),
            worker: WorkerId::parse("worker-1").expect("worker id"),
            ticket: Some(ticket.to_owned()),
            workspace: None,
            branch: None,
            runtime: None,
            provider: None,
            model: None,
            cost_micro_usd: None,
            usage: None,
            duration_ms: None,
            turns: None,
            conclusion: RunRecordConclusion::Succeeded,
            error: None,
            started_at: None,
            completed_at: None,
        }
    }

    /// The chain the deadlock broke: a finished Run resolves its local Ticket and
    /// makes its dependent ready, driven by the durable Run Record alone.
    ///
    /// The snapshot has no live Worker - exactly the state after `pool reset` -
    /// so this proves the completion signal survives reset rather than depending
    /// on live Worker state.
    #[test]
    fn a_finished_run_resolves_its_ticket_and_readies_its_dependent() {
        let temporary = TempDir::new().expect("temporary repository");
        fs::create_dir(temporary.path().join(".jj")).expect("repository marker");
        let repository = Repository::open(temporary.path()).expect("repository");
        write_local_ticket(
            temporary.path(),
            "widgets",
            "01-foundation.md",
            "# Foundation\n\nStatus: claimed\n",
        );
        write_local_ticket(
            temporary.path(),
            "widgets",
            "02-dependent.md",
            "# Dependent\n\nStatus: ready-for-agent\nBlocked by: 01\n",
        );
        repository
            .state_store()
            .runs()
            .append(&completed_record("widgets--01-foundation"))
            .expect("append");
        let tickets = local();
        let snapshot = repository.worker_pool().snapshot();

        let failures = apply_local_writeback(&repository, &snapshot, &tickets);

        assert!(failures.is_empty(), "{failures:?}");
        let foundation = fs::read_to_string(
            temporary
                .path()
                .join(".scratch/widgets/issues/01-foundation.md"),
        )
        .expect("foundation Ticket");
        assert!(foundation.contains("Status: resolved"), "{foundation}");
        let discovery = TicketDiscovery::new(
            configured_ticket_query(
                temporary.path(),
                &tickets,
                AgentRuntime::Claude,
                DEFAULT_READY_LABEL,
            )
                .expect("query"),
        );
        let filter = ReadyTicketFilter::new(
            "ready-for-agent",
            TicketStatus::parse("ready-for-agent").expect("ready status"),
        )
        .expect("Ready Ticket filter");
        let ready = discovery.ready_tickets(&filter).expect("ready Tickets");
        assert_eq!(ready.tickets().len(), 1, "{:?}", ready.diagnostics());
        assert_eq!(ready.tickets()[0].id().as_str(), "widgets--02-dependent");
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
        discovery
            .ticket_details(&id)
            .expect("Ticket details")
            .expect("the Ticket has details");

        assert_eq!(
            *requests.lock().expect("recorded requests"),
            ["ready", "graph", "details"]
        );
    }
}
