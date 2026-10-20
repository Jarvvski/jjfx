//! The blocking Workspace Dispatch adapter behind the controller seam.
//!
//! This module owns the port (`WorkspaceDispatchAdapter`) and its sole
//! production implementation over the shared Repository-owned Pool module.
//! The seam itself - commands, events, the controller, and presentation DTOs -
//! stays in the parent module, so a reader of the seam never wades through
//! engine mapping and process work.

use super::*;

use crate::config::{SpendConfig, TicketsConfig};
use crate::engine::{DEFAULT_READY_LABEL, DispatchExclusions, UndoOutcome};
use crate::ticket_source::configured_ticket_query;

/// The blocking port behind the controller seam: one capability surface the
/// controller runs off the App task. Production fills it with
/// [`RealWorkspaceDispatch`]; tests substitute a recording adapter.
pub trait WorkspaceDispatchAdapter: Send + Sync + 'static {
    /// Reconcile and read the current immutable Pool presentation.
    fn refresh(&self) -> Result<WorkerPoolSnapshot, String>;
    /// Set exact Pool capacity.
    fn resize(&self, capacity: usize) -> Result<PoolMutationResult, String>;
    /// Destroy every Pool Worker and its compatible state.
    fn destroy(&self) -> Result<(), String>;
    /// Dispatch Ticket IDs through the shared Direct Dispatch coordinator.
    fn dispatch(
        &self,
        tickets: &[String],
        worker: Option<&str>,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError>;
    /// Dispatch after approving the exact capacity gap under the Worker Pool lock.
    fn dispatch_with_approved_growth(
        &self,
        tickets: &[String],
        worker: Option<&str>,
        additional: usize,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError>;
    /// Dispatch only the available subset under the Worker Pool lock.
    fn dispatch_use_available(
        &self,
        tickets: &[String],
        worker: Option<&str>,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError>;
    /// Discover Tickets ready for Dispatch through the selected Worker Pool's
    /// own routing label and ready status.
    fn discover_ready(&self) -> Result<ReadyTicketResult, String>;
    /// Prepare and run one Parent orchestration, emitting durable progress updates.
    fn orchestrate(
        &self,
        parent: &str,
        emit: &mut dyn FnMut(WorkspaceDispatchOrchestrationEvent),
    ) -> Result<(), String>;
    /// Launch a user prompt as a background Follow-up Run, optionally choosing
    /// the first idle Worker.
    fn send(&self, worker: Option<&str>, prompt: &str) -> Result<WorkerSessionOutcome, String>;
    /// Launch a Pull Request review as a background Follow-up Run.
    fn review(&self, worker: &str) -> Result<WorkerSessionOutcome, String>;
    /// Reset a Worker and return its independent Workspace restoration handle.
    fn reset(&self, worker: &str) -> Result<ResetAdapterResult, String>;
    /// Rebase and push one Worker's bookmark.
    fn rebase(&self, worker: &str) -> Result<WorkerCommandResult, String>;
    /// Open one Worker's Pull Request.
    fn open_pull_request(&self, worker: &str) -> Result<WorkerCommandResult, String>;
    /// Set or clear one Worker's cosmetic alias.
    fn set_alias(&self, worker: &str, alias: &str) -> Result<WorkerCommandResult, String>;
    /// Dismiss one Worker using the compatibility disposition.
    fn dismiss(&self, worker: &str) -> Result<WorkerCommandResult, String>;
    /// Answer one Worker's pending opencode permission request.
    fn resolve_permission(
        &self,
        worker: &str,
        decision: PermissionDecision,
    ) -> Result<WorkerCommandResult, String>;
    /// Undo the last jjfx-initiated mutation on one Worker.
    fn undo(&self, worker: &str) -> Result<WorkerCommandResult, String>;
    /// Reads one latest Worker log snapshot without choosing a rendering.
    fn worker_log(&self, worker: &str) -> Result<WorkerLogSnapshot, String>;
}

fn orchestration_notice(event: &crate::engine::OrchestrationEvent) -> Option<String> {
    match event {
        crate::engine::OrchestrationEvent::Started { .. }
        | crate::engine::OrchestrationEvent::Terminal(_) => None,
        crate::engine::OrchestrationEvent::Completed { ticket, worker, .. } => {
            Some(format!("{ticket} completed on {worker}"))
        }
        crate::engine::OrchestrationEvent::Retrying {
            ticket, attempt, ..
        } => Some(format!("retrying {ticket} (attempt {attempt})")),
        crate::engine::OrchestrationEvent::QuotaBlocked { ticket, detail, .. } => Some(format!(
            "{ticket} quota-blocked: {} (waiting; not retried)",
            detail
                .as_deref()
                .unwrap_or("provider quota or billing wall")
        )),
        crate::engine::OrchestrationEvent::Dispatched { ticket, worker } => {
            Some(format!("{ticket} dispatched to {worker}"))
        }
        crate::engine::OrchestrationEvent::WaitingForCapacity { ticket } => {
            Some(format!("waiting for Worker capacity: {ticket}"))
        }
        crate::engine::OrchestrationEvent::LaunchFailed { ticket, detail, .. } => {
            Some(format!("launch failed {ticket}: {detail}"))
        }
        crate::engine::OrchestrationEvent::BranchRevalidated {
            ticket, current, ..
        } => Some(format!("repaired {ticket} -> {current}")),
        crate::engine::OrchestrationEvent::Failed { ticket, detail, .. } => Some(format!(
            "{ticket} failed: {}",
            detail.as_deref().unwrap_or("unknown error")
        )),
    }
}

/// The production adapter over the shared Repository-owned Pool module.
#[derive(Clone)]
pub struct RealWorkspaceDispatch {
    repository_root: PathBuf,
    /// Whether new Workspaces are colocated with Git (jjfx's `colocate`).
    colocate: bool,
    /// Where Dispatch discovers Ready Tickets (`[tickets]`), independent of the
    /// Agent Runtime the Worker Pool runs.
    tickets: TicketsConfig,
    /// The optional aggregate spend cap (`[spend]`) that gates Dispatch.
    spend: SpendConfig,
    /// The repo-owned build check a finished Run is verified against before its
    /// Ticket reaches done (`[checks]`, ticket 02).
    checks: crate::config::ChecksConfig,
    /// The optional ceiling on how many Runs Dispatch may keep in flight.
    cap: Option<crate::engine::ConcurrencyCap>,
    /// The multiplexer jjfx drives, used to keep Workspaces that are open in a
    /// terminal out of Worker selection. `None` (tests) excludes nothing.
    terminal: Option<std::sync::Arc<dyn crate::terminal::Terminal>>,
    /// The Worker Pool every pool-scoped operation acts on, shared with the
    /// frontend that names it (ADR 0012).
    pool: PoolSelection,
    /// The Repository's declared pools and the routing each declares, so
    /// discovery goes through the same seam `dispatch` does.
    routing: crate::config::PoolRouting,
}

impl std::fmt::Debug for RealWorkspaceDispatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RealWorkspaceDispatch")
            .field("repository_root", &self.repository_root)
            .field("colocate", &self.colocate)
            .field("ticket_source", &self.tickets.source)
            .field("terminal", &self.terminal.is_some())
            .finish()
    }
}

/// The Workers excluded from Dispatch right now, or why they cannot be known.
///
/// Fails closed when the terminal cannot be queried: with no way to tell which
/// Workspaces are open, Dispatch is refused rather than risking a collision with
/// a human. A frontend with no terminal excludes nothing.
fn live_exclusions(
    repository: &crate::engine::Repository,
    terminal: Option<&std::sync::Arc<dyn crate::terminal::Terminal>>,
) -> Result<DispatchExclusions, String> {
    let Some(terminal) = terminal else {
        return Ok(DispatchExclusions::default());
    };
    let open = terminal.open_workspaces().map_err(|error| {
        format!(
            "cannot determine which Worker Workspaces are open ({error}); \
             refusing to dispatch"
        )
    })?;
    Ok(crate::engine::reconcile_occupancy(repository, &open))
}

enum DispatchStrategy {
    Complete,
    ApprovedGrowth(usize),
    Available,
}

impl RealWorkspaceDispatch {
    /// Creates an adapter rooted at a discovered jj workspace.
    pub fn new(repository_root: impl Into<PathBuf>) -> Self {
        Self {
            repository_root: repository_root.into(),
            colocate: true,
            tickets: TicketsConfig::default(),
            spend: SpendConfig::default(),
            checks: crate::config::ChecksConfig::default(),
            cap: None,
            terminal: None,
            pool: PoolSelection::new(crate::engine::PoolName::default()),
            routing: crate::config::PoolRouting::default(),
        }
    }

    /// Sets the Workspace colocation preference (builder form).
    pub fn with_colocate(mut self, colocate: bool) -> Self {
        self.colocate = colocate;
        self
    }

    /// Sets where Dispatch discovers Ready Tickets (builder form).
    pub fn with_tickets(mut self, tickets: TicketsConfig) -> Self {
        self.tickets = tickets;
        self
    }

    /// Sets the aggregate spend cap that gates Dispatch (builder form).
    pub fn with_spend(mut self, spend: SpendConfig) -> Self {
        self.spend = spend;
        self
    }

    /// Sets the repo-owned build check a finished Run is verified against before
    /// its Ticket reaches done (builder form).
    pub fn with_checks(mut self, checks: crate::config::ChecksConfig) -> Self {
        self.checks = checks;
        self
    }

    /// Sets the ceiling on how many Runs Dispatch may keep in flight.
    pub fn with_concurrency_cap(mut self, cap: Option<crate::engine::ConcurrencyCap>) -> Self {
        self.cap = cap;
        self
    }

    /// Supplies the multiplexer used to detect Workspaces open in a terminal.
    pub fn with_terminal(
        mut self,
        terminal: std::sync::Arc<dyn crate::terminal::Terminal>,
    ) -> Self {
        self.terminal = Some(terminal);
        self
    }

    /// Shares the Worker Pool every pool-scoped operation acts on (builder form).
    pub fn with_pool_selection(mut self, pool: PoolSelection) -> Self {
        self.pool = pool;
        self
    }

    /// Sets the Repository's declared pools and their routing (builder form).
    pub fn with_pool_routing(mut self, routing: crate::config::PoolRouting) -> Self {
        self.routing = routing;
        self
    }

    /// The Worker Pool this adapter's operations act on right now.
    fn selected_pool(&self) -> crate::engine::PoolName {
        self.pool.get()
    }

    /// The Workers excluded from Dispatch because their Workspace is open.
    ///
    /// Fails closed when the terminal cannot be queried: with no way to tell
    /// which Workspaces are open, Dispatch is refused rather than risking a
    /// collision with a human. A test adapter with no terminal excludes nothing.
    fn exclusions(
        &self,
        repository: &crate::engine::Repository,
    ) -> Result<DispatchExclusions, String> {
        live_exclusions(repository, self.terminal.as_ref())
    }

    /// A live exclusion source for the orchestration runner.
    ///
    /// The runner asks it per claim, so a Workspace a human opens (or closes)
    /// while the group runs changes the next decision instead of the run's
    /// opening snapshot (ADR-0011).
    fn exclusions_provider(&self) -> crate::engine::ExclusionsProvider {
        let root = self.repository_root.clone();
        let colocate = self.colocate;
        let terminal = self.terminal.clone();
        crate::engine::ExclusionsProvider::new(move || {
            let repository = crate::engine::Repository::open(&root)
                .map(|repository| repository.with_colocate(colocate))
                .map_err(|error| error.to_string())?;
            live_exclusions(&repository, terminal.as_ref())
        })
    }

    /// Opens the Repository narrowed to the selected Worker Pool, so Worker
    /// reservation, Dispatch Eligibility and the pool's size are all read from
    /// that lane's state rather than from an implicit `default` (ADR 0012).
    fn repository(&self) -> Result<crate::engine::Repository, String> {
        crate::engine::Repository::open(&self.repository_root)
            .map(|repository| {
                repository
                    .with_colocate(self.colocate)
                    .with_pool(self.selected_pool())
                    .with_checks(self.checks.clone())
            })
            .map_err(|error| error.to_string())
    }

    /// Evaluates every spend limit for one Dispatch.
    ///
    /// Both limits bind: the selected pool's own cap totals its records and the
    /// Repository ceiling totals every pool's, so Dispatch proceeds only when
    /// each has room. Returns the warning message when the tightest fired limit
    /// is in `warn` mode, `None` when every limit has room or none is
    /// configured, and an error - refusing Dispatch - when any limit blocks. A
    /// limit that cannot be evaluated because the record store is unreadable
    /// refuses too: failing closed is the only safe answer for a spend gate.
    fn spend_gate(&self, repository: &crate::engine::Repository) -> Result<Option<String>, String> {
        let (names, pool_cap) = self.declared_spend(repository);
        let mut limits = Vec::new();
        if let Some(cap) = self.spend.cap().map_err(|error| error.to_string())? {
            let spend = repository
                .repository_cost(&names)
                .map_err(|error| error.to_string())?;
            limits.push(crate::engine::SpendLimit::new(
                crate::engine::SpendScope::Repository,
                cap,
                spend,
            ));
        }
        if let Some(cap) = pool_cap {
            let spend = repository.pool_cost().map_err(|error| error.to_string())?;
            limits.push(crate::engine::SpendLimit::new(
                crate::engine::SpendScope::Pool(repository.selected_pool().clone()),
                cap,
                spend,
            ));
        }
        match crate::engine::evaluate_spend(limits) {
            crate::engine::SpendDecision::Allowed => Ok(None),
            crate::engine::SpendDecision::Warned(message) => Ok(Some(message)),
            crate::engine::SpendDecision::Refused(message) => Err(message),
        }
    }

    /// The pool names the Repository ceiling totals and the selected pool's cap.
    ///
    /// The ceiling totals every pool's records, so it needs the Repository's
    /// declared names, and the selected pool's cap is a repository-local
    /// declaration whose window and mode default to the configured ceiling. A
    /// config that cannot be read falls back to the implicit `default` pool with
    /// no cap - the floor every Repository holds - rather than refusing a
    /// Dispatch the frontend already validated at launch.
    fn declared_spend(
        &self,
        repository: &crate::engine::Repository,
    ) -> (
        Vec<crate::engine::PoolName>,
        Option<crate::engine::SpendCap>,
    ) {
        let default_only = || (vec![crate::engine::PoolName::default()], None);
        let Ok(user) = crate::config::load() else {
            return default_only();
        };
        let Ok(repo_config) = user.repo_config(repository.root()) else {
            return default_only();
        };
        let names = user
            .pool_names_for(repository.root())
            .unwrap_or_else(|_| vec![crate::engine::PoolName::default()]);
        let cap = repo_config
            .pools
            .get(repository.selected_pool().as_str())
            .and_then(|declaration| declaration.spend_cap(&self.spend).ok().flatten());
        (names, cap)
    }

    fn dispatch_with_strategy(
        &self,
        tickets: &[String],
        worker: Option<&str>,
        strategy: DispatchStrategy,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError> {
        let repository = self.repository().map_err(WorkspaceDispatchError::Failed)?;
        let warning = self
            .spend_gate(&repository)
            .map_err(WorkspaceDispatchError::Failed)?;
        let exclusions = self
            .exclusions(&repository)
            .map_err(WorkspaceDispatchError::Failed)?;
        let snapshot = repository.worker_pool().snapshot();
        let profile = snapshot
            .pool()
            .and_then(|pool| pool.profile())
            .cloned()
            .unwrap_or_else(|| crate::engine::AgentRuntimeProfile::new(AgentRuntime::Claude));
        let runtime = profile.runtime();
        let requests = tickets
            .iter()
            .map(|ticket| {
                let id = TicketId::parse(ticket.clone())
                    .map_err(|error| WorkspaceDispatchError::Failed(error.to_string()))?;
                let request = DirectDispatchRequest::for_ticket_id(id, RunMode::Background)
                    .map_err(|error| WorkspaceDispatchError::Failed(error.to_string()))?
                    .with_profile(profile.clone());
                Ok(match worker {
                    Some(worker) => request.to_worker(
                        WorkerId::parse(worker.to_owned())
                            .map_err(|error| WorkspaceDispatchError::Failed(error.to_string()))?,
                    ),
                    None => request,
                })
            })
            .collect::<Result<Vec<_>, WorkspaceDispatchError>>()?;
        let dispatcher = repository
            .direct_dispatch()
            .with_exclusions(exclusions)
            .with_concurrency_cap(self.cap);
        let result = match strategy {
            DispatchStrategy::Complete => dispatcher.dispatch(&requests),
            DispatchStrategy::ApprovedGrowth(additional) => {
                dispatcher.dispatch_with_approved_growth(&requests, additional)
            }
            DispatchStrategy::Available => dispatcher.dispatch_use_available(&requests),
        }
        .map_err(|error| match error {
            DirectDispatchError::WorkerPool(WorkerPoolError::CapacityShortage(shortage)) => {
                WorkspaceDispatchError::CapacityShortage(DispatchCapacityShortage {
                    requested: shortage.requested(),
                    available: shortage.available(),
                })
            }
            other => WorkspaceDispatchError::Failed(other.to_string()),
        })?;
        let outcomes = result
            .outcomes()
            .iter()
            .map(|outcome| match outcome {
                DirectDispatchOutcome::Succeeded(success) => match success.execution() {
                    DirectDispatchExecution::Background { pid } => DispatchOutcome::success(
                        success.ticket().id().to_string(),
                        success.ticket().title().as_str().to_owned(),
                        success.worker().to_string(),
                        *pid,
                    ),
                    DirectDispatchExecution::Session { session_id } => DispatchOutcome::session(
                        success.ticket().id().to_string(),
                        success.ticket().title().as_str().to_owned(),
                        success.worker().to_string(),
                        session_id.clone(),
                    ),
                    DirectDispatchExecution::Foreground(_) => DispatchOutcome::failure(
                        success.ticket().id().to_string(),
                        success.ticket().title().as_str().to_owned(),
                        Some(success.worker().to_string()),
                        DirectDispatchFailurePhase::Launch,
                        "foreground Dispatch is not supported by the jjfx controller".to_owned(),
                    ),
                },
                DirectDispatchOutcome::Failed(failure) => DispatchOutcome::failure(
                    failure.ticket().id().to_string(),
                    failure.ticket().title().as_str().to_owned(),
                    failure.worker().map(ToString::to_string),
                    failure.phase(),
                    failure.detail().to_owned(),
                ),
                DirectDispatchOutcome::Waiting(waiting) => DispatchOutcome::waiting(
                    waiting.ticket().id().to_string(),
                    waiting.ticket().title().as_str().to_owned(),
                    waiting.detail().to_owned(),
                ),
            })
            .collect();
        Ok(DispatchAdapterResult::new(DispatchResult {
            runtime,
            outcomes,
            partial: result.is_partial(),
            warning,
        }))
    }

    fn worker_action(
        &self,
        worker: Option<&str>,
        action: WorkerActionKind,
        prompt: Option<&str>,
    ) -> Result<WorkerSessionOutcome, String> {
        let repository = self.repository()?;
        let worker_id = worker
            .map(|worker| WorkerId::parse(worker.to_owned()))
            .transpose()
            .map_err(|error| error.to_string())?;
        let actions = match action {
            // A follow-up prompt must not be injected into a Workspace a human
            // is sitting in. `review` is exempt: it is how a waiting Worker is
            // told to address its pull request.
            WorkerActionKind::Send => {
                let exclusions = self.exclusions(&repository)?;
                crate::engine::WorkerActions::new(repository).with_exclusions(exclusions)
            }
            WorkerActionKind::Review => crate::engine::WorkerActions::new(repository),
        };
        let outcome = match action {
            WorkerActionKind::Send => {
                let prompt = prompt.ok_or_else(|| "Send prompt cannot be missing".to_owned())?;
                match worker_id.as_ref() {
                    Some(worker) => actions.send(worker, prompt, RunMode::Background),
                    None => actions.send_any(prompt, RunMode::Background),
                }
                .map_err(|error| error.to_string())?
            }
            WorkerActionKind::Review => actions
                .review(
                    worker_id
                        .as_ref()
                        .ok_or_else(|| "Review worker cannot be missing".to_owned())?,
                    RunMode::Background,
                )
                .map_err(|error| error.to_string())?,
        };
        let resolved_worker = outcome.worker().to_string();
        let runtime = outcome.runtime();
        let session = outcome.session().clone();
        let pid = match outcome.into_execution() {
            crate::engine::FollowUpExecution::Background(run) => {
                let pid = run.pid();
                std::thread::spawn(move || {
                    let _ = run.wait();
                });
                Some(pid)
            }
            // A daemon-delivered prompt drives no client process; the Worker's
            // liveness follows its session (ticket 37), so there is nothing to
            // reap or finalize here.
            crate::engine::FollowUpExecution::Session(_) => None,
            crate::engine::FollowUpExecution::Foreground(_) => {
                return Err("foreground Worker actions are not supported by jjfx".to_owned());
            }
        };
        Ok(WorkerSessionOutcome::new(
            resolved_worker,
            action,
            runtime,
            session,
            pid,
        ))
    }
}

impl WorkspaceDispatchAdapter for RealWorkspaceDispatch {
    fn refresh(&self) -> Result<WorkerPoolSnapshot, String> {
        let repository = self.repository()?;
        // Refresh the durable in-use marker from the live terminal so the Worker
        // Pool view reflects which Workspaces are open. Best effort: a failed query
        // leaves the marker as it was rather than failing the refresh.
        if let Some(terminal) = self.terminal.as_ref()
            && let Ok(open) = terminal.open_workspaces()
        {
            let _ = crate::engine::reconcile_occupancy(&repository, &open);
        }
        let snapshot = repository.worker_pool().reconcile_runs();
        // Claim a Ticket a Worker is running and resolve one whose Run finished.
        // Best effort: a failed writeback leaves the Ticket as it was and the
        // next refresh retries, so it must not fail the refresh itself.
        let _ = crate::ticket_source::apply_local_writeback(&repository, &snapshot, &self.tickets);
        Ok(snapshot)
    }

    fn resize(&self, capacity: usize) -> Result<PoolMutationResult, String> {
        let capacity = PoolCapacity::new(capacity).map_err(|error| error.to_string())?;
        let result = self
            .repository()?
            .worker_pool()
            .resize_to(capacity)
            .map_err(|error| error.to_string())?;
        Ok(PoolMutationResult {
            capacity: result.capacity().as_usize(),
            added_workers: result
                .added_workers()
                .iter()
                .map(ToString::to_string)
                .collect(),
            removed_workers: result
                .removed_workers()
                .iter()
                .map(ToString::to_string)
                .collect(),
        })
    }

    fn destroy(&self) -> Result<(), String> {
        self.repository()?
            .worker_pool()
            .destroy()
            .map_err(|error| error.to_string())
    }

    fn discover_ready(&self) -> Result<ReadyTicketResult, String> {
        let repository = self.repository()?;
        let runtime = repository
            .worker_pool()
            .snapshot()
            .pool()
            .and_then(|pool| pool.agent_runtime())
            .unwrap_or(AgentRuntime::Claude);
        // The ready label and status are the selected pool's, resolved through
        // the same seam `dispatch` uses: a lane that declares a routing label
        // discovers by it, and a lane that declares its own `ready_status`
        // draws work by that status. The *source's* fallback label is the
        // global one, never the pool's, so a Ticket that declares no `Labels:`
        // cannot match every lane at once.
        let pool = repository.selected_pool();
        let label = self.routing.ready_label(pool);
        let status =
            TicketStatus::parse(self.routing.ready_status(pool, self.tickets.ready_status()))
                .map_err(|error| error.to_string())?;
        let filter = ReadyTicketFilter::new(&label, status).map_err(|error| error.to_string())?;
        let discovery = TicketDiscovery::new(
            configured_ticket_query(
                repository.root(),
                &self.tickets,
                runtime,
                &crate::config::global_ready_label(None),
            )
            .map_err(|error| error.to_string())?,
        );
        let ready = discovery
            .ready_tickets(&filter)
            .map_err(|error| error.to_string())?;
        Ok(ReadyTicketResult::new(
            ready
                .tickets()
                .iter()
                .map(|ticket| ReadyTicket::new(ticket.id().to_string(), ticket.title().as_str()))
                .collect(),
            ready
                .diagnostics()
                .iter()
                .map(|diagnostic| format!("{}: {}", diagnostic.subject(), diagnostic.reason()))
                .collect(),
        ))
    }

    fn orchestrate(
        &self,
        parent: &str,
        emit: &mut dyn FnMut(WorkspaceDispatchOrchestrationEvent),
    ) -> Result<(), String> {
        let repository = self.repository()?;
        // A blocked spend cap refuses orchestration too: it dispatches Runs the
        // same way, so the gate belongs on both Dispatch paths.
        self.spend_gate(&repository)?;
        let id = TicketId::parse(parent.to_owned()).map_err(|error| error.to_string())?;
        let pool = repository.worker_pool().snapshot();
        let profile = pool.pool().and_then(|pool| pool.profile()).cloned();
        let runtime = profile.as_ref().map_or(
            AgentRuntime::Claude,
            crate::engine::AgentRuntimeProfile::runtime,
        );
        let mut request = crate::engine::OrchestrationRequest::new(id.clone(), runtime);
        if let Some(model) = profile
            .as_ref()
            .and_then(crate::engine::AgentRuntimeProfile::model)
        {
            request = request.with_model(model.clone());
        }
        // The concurrency cap bounds a wave as well as a batch, so a Parent's
        // Sub-issues obey the same `[dispatch] max_concurrent` ceiling a
        // `dispatch --all` batch does.
        request = request.with_concurrency_cap(self.cap);
        let ticket = DirectDispatchRequest::for_ticket_id(id.clone(), RunMode::Background)
            .map_err(|error| error.to_string())?
            .ticket()
            .clone();
        let parent_ticket = crate::engine::ParentTicket::new(ticket.id().clone());
        let discovery = TicketDiscovery::new(
            configured_ticket_query(
                repository.root(),
                &self.tickets,
                runtime,
                DEFAULT_READY_LABEL,
            )
            .map_err(|error| error.to_string())?,
        );
        let runner = repository
            .orchestration_runner()
            .with_exclusions(self.exclusions_provider());
        let preparation = runner
            .prepare(&request, &parent_ticket, &discovery)
            .map_err(|error| error.to_string())?;
        let resumed = preparation.resumed();
        match preparation.into_start() {
            // No free Worker is a capacity gap, not a failure: report the
            // Parent as waiting rather than as a Dispatch that broke.
            crate::engine::OrchestrationStart::Waiting { ticket, detail } => {
                emit(WorkspaceDispatchOrchestrationEvent::Waiting {
                    parent: ticket.to_string(),
                    detail,
                });
                return Ok(());
            }
            crate::engine::OrchestrationStart::Direct(success) => {
                emit(WorkspaceDispatchOrchestrationEvent::Started {
                    parent: id.to_string(),
                    resumed,
                });
                let (pid, session_id) = match success.execution() {
                    crate::engine::DirectDispatchExecution::Background { pid } => {
                        (Some(*pid), None)
                    }
                    crate::engine::DirectDispatchExecution::Session { session_id } => {
                        (None, Some(session_id.clone()))
                    }
                    crate::engine::DirectDispatchExecution::Foreground(_) => {
                        return Err(
                            "foreground Parent Dispatch is not supported by jjfx".to_owned()
                        );
                    }
                };
                emit(WorkspaceDispatchOrchestrationEvent::Direct {
                    parent: id.to_string(),
                    worker: success.worker().to_string(),
                    pid,
                    session_id,
                });
                return Ok(());
            }
            crate::engine::OrchestrationStart::Group => {}
        }
        emit(WorkspaceDispatchOrchestrationEvent::Started {
            parent: id.to_string(),
            resumed,
        });

        let mut projection_error = None;
        let options = crate::engine::OrchestrationOptions::new();
        runner
            .run(&request, &options, |event| {
                if matches!(event, crate::engine::OrchestrationEvent::Started { .. }) {
                    return;
                }
                let notice = orchestration_notice(&event);
                let progress = match runner.progress(&id) {
                    Ok(Some(progress)) => Ok(progress),
                    Ok(None) => Err(format!(
                        "Dispatch Group {id} disappeared during orchestration"
                    )),
                    Err(error) => Err(error.to_string()),
                };
                match progress {
                    Ok(progress) => {
                        emit(WorkspaceDispatchOrchestrationEvent::Progress { progress, notice })
                    }
                    Err(error) => projection_error = Some(error),
                }
                if let crate::engine::OrchestrationEvent::Terminal(summary) = event {
                    emit(WorkspaceDispatchOrchestrationEvent::Terminal {
                        parent: summary.parent().to_string(),
                        counts: summary.counts(),
                    });
                }
            })
            .map_err(|error| error.to_string())?;
        projection_error.map_or(Ok(()), Err)
    }

    fn dispatch(
        &self,
        tickets: &[String],
        worker: Option<&str>,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError> {
        self.dispatch_with_strategy(tickets, worker, DispatchStrategy::Complete)
    }

    fn dispatch_with_approved_growth(
        &self,
        tickets: &[String],
        worker: Option<&str>,
        additional: usize,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError> {
        self.dispatch_with_strategy(
            tickets,
            worker,
            DispatchStrategy::ApprovedGrowth(additional),
        )
    }

    fn dispatch_use_available(
        &self,
        tickets: &[String],
        worker: Option<&str>,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError> {
        self.dispatch_with_strategy(tickets, worker, DispatchStrategy::Available)
    }

    fn send(&self, worker: Option<&str>, prompt: &str) -> Result<WorkerSessionOutcome, String> {
        self.worker_action(worker, WorkerActionKind::Send, Some(prompt))
    }

    fn review(&self, worker: &str) -> Result<WorkerSessionOutcome, String> {
        self.worker_action(Some(worker), WorkerActionKind::Review, None)
    }

    fn reset(&self, worker: &str) -> Result<ResetAdapterResult, String> {
        let repository = self.repository()?;
        let worker_id = WorkerId::parse(worker.to_owned()).map_err(|error| error.to_string())?;
        let outcome = crate::engine::WorkerActions::new(repository)
            .reset(&worker_id)
            .map_err(|error| error.to_string())?;
        Ok(ResetAdapterResult::new(
            WorkerResetOutcome::new(worker, outcome.run()),
            outcome.into_restoration(),
        ))
    }

    fn rebase(&self, worker: &str) -> Result<WorkerCommandResult, String> {
        let repository = self.repository()?;
        let worker_id = WorkerId::parse(worker.to_owned()).map_err(|error| error.to_string())?;
        let outcome = crate::engine::WorkerActions::new(repository)
            .rebase(&worker_id)
            .map_err(|error| error.to_string())?;
        Ok(WorkerCommandResult::Rebased {
            worker: worker.to_owned(),
            branch: outcome.branch().to_owned(),
        })
    }

    fn open_pull_request(&self, worker: &str) -> Result<WorkerCommandResult, String> {
        let repository = self.repository()?;
        let worker_id = WorkerId::parse(worker.to_owned()).map_err(|error| error.to_string())?;
        let outcome = crate::engine::WorkerActions::new(repository)
            .open_pull_request(&worker_id)
            .map_err(|error| error.to_string())?;
        Ok(WorkerCommandResult::PullRequestOpened {
            worker: worker.to_owned(),
            branch: outcome.branch().to_owned(),
        })
    }

    fn set_alias(&self, worker: &str, alias: &str) -> Result<WorkerCommandResult, String> {
        let repository = self.repository()?;
        let worker_id = WorkerId::parse(worker.to_owned()).map_err(|error| error.to_string())?;
        let alias = match alias.trim() {
            "" => None,
            value => Some(value.to_owned()),
        };
        repository
            .worker_pool()
            .set_alias(worker_id, alias.clone().unwrap_or_default())
            .map_err(|error| error.to_string())?;
        Ok(WorkerCommandResult::AliasChanged {
            worker: worker.to_owned(),
            alias,
        })
    }

    fn dismiss(&self, worker: &str) -> Result<WorkerCommandResult, String> {
        let repository = self.repository()?;
        let worker_id = WorkerId::parse(worker.to_owned()).map_err(|error| error.to_string())?;
        let outcome = crate::engine::WorkerActions::new(repository)
            .dismiss(&worker_id)
            .map_err(|error| error.to_string())?;
        let disposition = match outcome {
            crate::engine::DismissOutcome::Removed { capacity } => {
                WorkerDismissDisposition::Removed { capacity }
            }
            crate::engine::DismissOutcome::Reset => WorkerDismissDisposition::Reset,
        };
        Ok(WorkerCommandResult::Dismissed {
            worker: worker.to_owned(),
            disposition,
        })
    }

    fn resolve_permission(
        &self,
        worker: &str,
        decision: PermissionDecision,
    ) -> Result<WorkerCommandResult, String> {
        let repository = self.repository()?;
        let worker_id = WorkerId::parse(worker.to_owned()).map_err(|error| error.to_string())?;
        let resolution = crate::engine::WorkerActions::new(repository)
            .resolve_permission(&worker_id, decision)
            .map_err(|error| error.to_string())?;
        Ok(WorkerCommandResult::PermissionResolved {
            worker: worker.to_owned(),
            action: resolution.request().action().to_owned(),
            decision: resolution.decision(),
        })
    }

    fn undo(&self, worker: &str) -> Result<WorkerCommandResult, String> {
        let repository = self.repository()?;
        let worker_id = WorkerId::parse(worker.to_owned()).map_err(|error| error.to_string())?;
        match repository
            .undo_mutation(worker_id.as_str(), false)
            .map_err(|error| error.to_string())?
        {
            UndoOutcome::Nothing => Err(format!("nothing recorded to undo for {worker}")),
            UndoOutcome::Restored {
                kind,
                discarded,
                later,
            } => Ok(WorkerCommandResult::MutationUndone {
                worker: worker.to_owned(),
                kind,
                discarded,
                later,
            }),
            UndoOutcome::Refused { kind, later } => Err(match later {
                Some(later) => format!(
                    "refusing to undo the last {kind} on {worker}: {later} operation(s) have happened since"
                ),
                None => format!(
                    "refusing to undo the last {kind} on {worker}: its recorded operation is no longer in the log"
                ),
            }),
        }
    }

    fn worker_log(&self, worker: &str) -> Result<WorkerLogSnapshot, String> {
        let repository = self.repository()?;
        let worker_id = WorkerId::parse(worker.to_owned()).map_err(|error| error.to_string())?;
        let snapshot = repository.worker_pool().snapshot();
        let state = snapshot
            .worker(worker)
            .ok_or_else(|| format!("Worker {worker} was not found"))?;
        let runtime = state.agent_runtime().unwrap_or(AgentRuntime::Claude);
        let logs = crate::engine::WorkerActions::new(repository)
            .logs(&worker_id)
            .map_err(|error| error.to_string())?;
        let log = logs.open();
        let activity = log.current_activity().map_err(|error| error.to_string())?;
        let result = match state.status() {
            WorkerStatus::Done | WorkerStatus::Failed => {
                log.final_result().map_err(|error| error.to_string())?
            }
            WorkerStatus::Idle | WorkerStatus::Busy => None,
        };
        Ok(WorkerLogSnapshot::new(
            worker.to_owned(),
            runtime,
            activity,
            result,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::TicketSource;
    use crate::engine::{Repository, RunRecord, RunRecordConclusion, SpendCapMode, WireTimestamp};

    /// Writes one local Ticket under `<repo>/<root>/<feature>/issues/<file>`.
    fn write_local_ticket(
        root: &std::path::Path,
        feature: &str,
        file: &str,
        status: &str,
        labels: Option<&str>,
    ) {
        let directory = root.join(".scratch").join(feature).join("issues");
        std::fs::create_dir_all(&directory).expect("issues directory");
        let labels = labels.map_or_else(String::new, |labels| format!("Labels: {labels}\n"));
        std::fs::write(
            directory.join(file),
            format!("# {file}\n\nStatus: {status}\n{labels}"),
        )
        .expect("Ticket file");
    }

    /// The Ticket ids a discovery result carries, in order.
    fn ready_ids(result: &ReadyTicketResult) -> Vec<&str> {
        result.tickets().iter().map(ReadyTicket::id).collect()
    }

    /// A repository with one recorded Run costing `cost_micro_usd`.
    fn repository_with_record(cost_micro_usd: u64) -> (tempfile::TempDir, Repository) {
        let temp = tempfile::tempdir().expect("temp repository");
        std::fs::create_dir(temp.path().join(".jj")).expect("repository marker");
        let repository = Repository::open(temp.path()).expect("repository");
        repository
            .state_store()
            .runs()
            .append(&RunRecord {
                run_id: "worker-01@1".to_owned(),
                worker: crate::engine::WorkerId::parse("worker-01").expect("worker id"),
                ticket: None,
                workspace: None,
                branch: None,
                runtime: None,
                provider: None,
                model: None,
                cost_micro_usd: Some(cost_micro_usd),
                usage: None,
                duration_ms: None,
                turns: None,
                conclusion: RunRecordConclusion::Succeeded,
                check: None,
                error: None,
                started_at: None,
                completed_at: Some(WireTimestamp::new(jiff::Timestamp::now().to_string())),
            })
            .expect("append record");
        (temp, repository)
    }

    #[test]
    fn no_configured_cap_never_gates_dispatch() {
        let (_temp, repository) = repository_with_record(6_000_000);
        let adapter = RealWorkspaceDispatch::new(repository.root());
        assert_eq!(adapter.spend_gate(&repository).expect("no cap"), None);
    }

    #[test]
    fn a_blocked_spend_cap_refuses_dispatch_with_the_override() {
        let (_temp, repository) = repository_with_record(6_000_000);
        let adapter = RealWorkspaceDispatch::new(repository.root()).with_spend(SpendConfig {
            cap_usd: Some(5.0),
            ..SpendConfig::default()
        });

        let message = adapter
            .spend_gate(&repository)
            .expect_err("a blocked cap refuses Dispatch");
        assert!(message.contains("spend.cap_usd"), "{message}");
        assert!(message.contains("spend.on_exceed"), "{message}");
    }

    #[test]
    fn a_warn_spend_cap_reports_the_overage_and_proceeds() {
        let (_temp, repository) = repository_with_record(6_000_000);
        let adapter = RealWorkspaceDispatch::new(repository.root()).with_spend(SpendConfig {
            cap_usd: Some(5.0),
            on_exceed: SpendCapMode::Warn,
            ..SpendConfig::default()
        });

        let warning = adapter
            .spend_gate(&repository)
            .expect("a warn cap lets Dispatch through")
            .expect("a warning");
        assert!(warning.contains("dispatching anyway"), "{warning}");
    }

    #[test]
    fn a_cap_under_its_limit_does_not_gate() {
        let (_temp, repository) = repository_with_record(1_000_000);
        let adapter = RealWorkspaceDispatch::new(repository.root()).with_spend(SpendConfig {
            cap_usd: Some(5.0),
            ..SpendConfig::default()
        });
        assert_eq!(adapter.spend_gate(&repository).expect("under"), None);
    }

    /// Discovery draws the selected lane's work: the lane's declared routing
    /// label decides which Tickets are drawn, and the lane's declared
    /// `ready_status` decides which are ready. A second lane drawing the same
    /// Tickets under different rules must not be able to reach them, so the two
    /// lanes cannot collapse into one.
    #[test]
    fn discovery_honours_the_selected_pools_routing_label_and_ready_status() {
        let temp = tempfile::tempdir().expect("temp repository");
        std::fs::create_dir(temp.path().join(".jj")).expect("repository marker");
        write_local_ticket(
            temp.path(),
            "bugs",
            "01-fix.md",
            "ready-for-agent",
            Some("bug"),
        );
        write_local_ticket(
            temp.path(),
            "bugs",
            "02-review.md",
            "ready-for-review",
            Some("bug"),
        );
        write_local_ticket(
            temp.path(),
            "features",
            "01-ship.md",
            "ready-for-agent",
            Some("feature"),
        );
        let config_dir = temp.path().join(".jjfx");
        std::fs::create_dir_all(&config_dir).expect("config directory");
        std::fs::write(
            config_dir.join("config.toml"),
            "[pools.bugs]\nlabel = \"bug\"\n\n[pools.review]\nready_status = \"ready-for-review\"\n",
        )
        .expect("repository config");

        let user: crate::config::Config =
            toml::from_str("[tickets]\nsource = \"local\"\n").expect("user config");
        let routing =
            crate::config::PoolRouting::load(&user, temp.path()).expect("routing resolves");
        let tickets = TicketsConfig {
            source: TicketSource::Local,
            ..TicketsConfig::default()
        };

        // The `bugs` lane routes by its declared label and keeps the global
        // ready status, so it draws only the Ticket that carries `bug` and is
        // ready for an agent.
        let bugs = RealWorkspaceDispatch::new(temp.path())
            .with_tickets(tickets.clone())
            .with_pool_routing(routing.clone())
            .with_pool_selection(PoolSelection::new(
                PoolName::parse("bugs").expect("pool name"),
            ));
        assert_eq!(
            ready_ids(&bugs.discover_ready().expect("discovery")),
            ["bugs--01-fix"],
            "the `bugs` lane draws its label in the global status"
        );

        // The `review` lane declares no label but its own ready status, so it
        // draws the Ticket that is ready for review - a different Ticket, from
        // the same tree.
        let review = RealWorkspaceDispatch::new(temp.path())
            .with_tickets(tickets)
            .with_pool_routing(routing)
            .with_pool_selection(PoolSelection::new(
                PoolName::parse("review").expect("pool name"),
            ));
        assert_eq!(
            ready_ids(&review.discover_ready().expect("discovery")),
            ["bugs--02-review"],
            "the `review` lane draws its declared ready status"
        );
    }
}
