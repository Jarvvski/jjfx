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
    AgentRuntime, AgentRuntimeQuery, LocalMarkdownQuery, LocalTickets, Repository,
    RunRecordConclusion, TicketQuery, TicketQueryError, WorkerPoolSnapshot, WorkerStatus,
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
/// local Ticket claims it, and a Ticket whose Run finished **and whose change
/// passed the repository's check** is resolved.
///
/// The completion signal is the durable Run Record store, not live Worker state:
/// `pool reset` clears a Worker's Ticket association, so a finished Worker that
/// was reset would otherwise never resolve its Ticket. The record carries the
/// check the repository's `[checks]` command produced against the Run's change,
/// so a Ticket is done only when that check ran and passed - the agent's own
/// terminal report is not enough (ticket 02). A Run that finished with a failed
/// check, one that could not run, or none configured leaves the Ticket not-done
/// and returns the reason, rather than silently claiming verification. A no-op
/// for every other source. The failures are returned rather than raised: the
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
    let mut completed = Vec::new();
    let mut failures = Vec::new();
    match repository.state_store().runs().latest_ticket_records() {
        Ok(records) => {
            for (ticket, record) in records {
                // A Run that did not finish successfully is not done, and needs
                // no explanation here: the Worker's own status already carries it.
                if record.conclusion != RunRecordConclusion::Succeeded {
                    continue;
                }
                // A live Worker still owns the Ticket, so it is in flight, not
                // finished - a claim wins over a stale verified record.
                if in_flight.contains(&ticket.as_str()) {
                    continue;
                }
                match &record.check {
                    // Only a check that ran and passed makes the Ticket done.
                    Some(check) if check.passed() => completed.push(ticket),
                    // Everything else - a failed check, one that could not run,
                    // none configured, or a record written before verification
                    // existed - leaves the Ticket demonstrably not-done and names
                    // what happened, so a finished Run is never mistaken for
                    // finished work.
                    Some(check) => failures.push(format!(
                        "Ticket {ticket} is not verified: {}",
                        check.reason().unwrap_or_default()
                    )),
                    None => failures.push(format!(
                        "Ticket {ticket} is not verified: its Run recorded no check"
                    )),
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
        DEFAULT_READY_LABEL, ParentTicket, ReadyTicketFilter, RepositoryIdentity, RunCheck,
        RunRecord, RunRecordConclusion, TicketDiscovery, TicketId, TicketQueryRequest,
        TicketStatus, WorkerId,
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
            check: Some(RunCheck::Passed),
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

    /// A finished Run whose change did not pass the repository's check is not
    /// done: the Ticket stays as it was and the writeback names what failed, so a
    /// failed check is never mistaken for finished work (ticket 02).
    #[test]
    fn a_finished_run_whose_check_failed_does_not_resolve_its_ticket() {
        let temporary = TempDir::new().expect("temporary repository");
        fs::create_dir(temporary.path().join(".jj")).expect("repository marker");
        let repository = Repository::open(temporary.path()).expect("repository");
        write_local_ticket(
            temporary.path(),
            "widgets",
            "01-foundation.md",
            "# Foundation\n\nStatus: claimed\n",
        );
        let mut record = completed_record("widgets--01-foundation");
        record.check = Some(RunCheck::Failed {
            command: "cargo check".to_owned(),
            detail: "error: could not compile".to_owned(),
        });
        repository
            .state_store()
            .runs()
            .append(&record)
            .expect("append");
        let tickets = local();
        let snapshot = repository.worker_pool().snapshot();

        let failures = apply_local_writeback(&repository, &snapshot, &tickets);

        let foundation = fs::read_to_string(
            temporary
                .path()
                .join(".scratch/widgets/issues/01-foundation.md"),
        )
        .expect("foundation Ticket");
        assert!(
            foundation.contains("Status: claimed"),
            "a failed check leaves the Ticket not-done: {foundation}"
        );
        assert!(
            failures.iter().any(|failure| {
                failure.contains("not verified")
                    && failure.contains("cargo check")
                    && failure.contains("could not compile")
            }),
            "{failures:?}"
        );
    }

    /// A repository with no check configured does not silently claim
    /// verification: the Ticket is not resolved and the writeback says so.
    #[test]
    fn a_repository_with_no_check_configured_does_not_resolve_and_says_so() {
        let temporary = TempDir::new().expect("temporary repository");
        fs::create_dir(temporary.path().join(".jj")).expect("repository marker");
        let repository = Repository::open(temporary.path()).expect("repository");
        write_local_ticket(
            temporary.path(),
            "widgets",
            "01-foundation.md",
            "# Foundation\n\nStatus: claimed\n",
        );
        let mut record = completed_record("widgets--01-foundation");
        record.check = Some(RunCheck::NotConfigured);
        repository
            .state_store()
            .runs()
            .append(&record)
            .expect("append");
        let tickets = local();
        let snapshot = repository.worker_pool().snapshot();

        let failures = apply_local_writeback(&repository, &snapshot, &tickets);

        let foundation = fs::read_to_string(
            temporary
                .path()
                .join(".scratch/widgets/issues/01-foundation.md"),
        )
        .expect("foundation Ticket");
        assert!(foundation.contains("Status: claimed"), "{foundation}");
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("no check is configured")),
            "{failures:?}"
        );
    }

    /// A record written before jjfx recorded verification does not read as done
    /// either: no check having run is never a pass.
    #[test]
    fn a_record_without_a_check_does_not_resolve_its_ticket() {
        let temporary = TempDir::new().expect("temporary repository");
        fs::create_dir(temporary.path().join(".jj")).expect("repository marker");
        let repository = Repository::open(temporary.path()).expect("repository");
        write_local_ticket(
            temporary.path(),
            "widgets",
            "01-foundation.md",
            "# Foundation\n\nStatus: claimed\n",
        );
        let mut record = completed_record("widgets--01-foundation");
        record.check = None;
        repository
            .state_store()
            .runs()
            .append(&record)
            .expect("append");
        let tickets = local();
        let snapshot = repository.worker_pool().snapshot();

        let failures = apply_local_writeback(&repository, &snapshot, &tickets);

        let foundation = fs::read_to_string(
            temporary
                .path()
                .join(".scratch/widgets/issues/01-foundation.md"),
        )
        .expect("foundation Ticket");
        assert!(foundation.contains("Status: claimed"), "{foundation}");
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("recorded no check")),
            "{failures:?}"
        );
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
