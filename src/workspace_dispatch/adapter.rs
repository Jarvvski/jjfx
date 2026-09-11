//! The blocking Workspace Dispatch adapter behind the controller seam.
//!
//! This module owns the port (`WorkspaceDispatchAdapter`) and its sole
//! production implementation over the shared Repository-owned Pool module.
//! The seam itself - commands, events, the controller, and presentation DTOs -
//! stays in the parent module, so a reader of the seam never wades through
//! wsg-core mapping and process work.

use super::*;

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
    /// Dispatch after approving the exact capacity gap under the Pool lock.
    fn dispatch_with_approved_growth(
        &self,
        tickets: &[String],
        worker: Option<&str>,
        additional: usize,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError>;
    /// Dispatch only the available subset under the Pool lock.
    fn dispatch_use_available(
        &self,
        tickets: &[String],
        worker: Option<&str>,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError>;
    /// Discover Tickets ready for Dispatch through the configured Agent Runtime.
    fn discover_ready(&self, label: &str) -> Result<ReadyTicketResult, String>;
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
    /// Reads one latest Worker log snapshot without choosing a rendering.
    fn worker_log(&self, worker: &str) -> Result<WorkerLogSnapshot, String>;
}

fn orchestration_notice(event: &wsg_core::OrchestrationEvent) -> Option<String> {
    match event {
        wsg_core::OrchestrationEvent::Started { .. }
        | wsg_core::OrchestrationEvent::Terminal(_) => None,
        wsg_core::OrchestrationEvent::Completed { ticket, worker, .. } => {
            Some(format!("{ticket} completed on {worker}"))
        }
        wsg_core::OrchestrationEvent::Retrying {
            ticket, attempt, ..
        } => Some(format!("retrying {ticket} (attempt {attempt})")),
        wsg_core::OrchestrationEvent::Dispatched { ticket, worker } => {
            Some(format!("{ticket} dispatched to {worker}"))
        }
        wsg_core::OrchestrationEvent::WaitingForCapacity { ticket } => {
            Some(format!("waiting for Worker capacity: {ticket}"))
        }
        wsg_core::OrchestrationEvent::LaunchFailed { ticket, detail, .. } => {
            Some(format!("launch failed {ticket}: {detail}"))
        }
        wsg_core::OrchestrationEvent::BranchRevalidated {
            ticket, current, ..
        } => Some(format!("repaired {ticket} -> {current}")),
        wsg_core::OrchestrationEvent::Failed { ticket, detail, .. } => Some(format!(
            "{ticket} failed: {}",
            detail.as_deref().unwrap_or("unknown error")
        )),
    }
}

/// The production adapter over the shared Repository-owned Pool module.
#[derive(Debug, Clone)]
pub struct RealWorkspaceDispatch {
    repository_root: PathBuf,
}

fn configured_ticket_query(
    repository: &wsg_core::Repository,
    runtime: AgentRuntime,
) -> AgentRuntimeQuery {
    let query = AgentRuntimeQuery::new(runtime, repository.root());
    if runtime != AgentRuntime::Pi {
        return query;
    }
    match std::env::var_os(PI_DISCOVERY_HELPER_ENV).filter(|executable| !executable.is_empty()) {
        Some(executable) => query.with_pi_helper(PiDiscoveryHelper::new(executable)),
        None => query,
    }
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
        }
    }

    fn repository(&self) -> Result<wsg_core::Repository, String> {
        wsg_core::Repository::open(&self.repository_root).map_err(|error| error.to_string())
    }

    fn dispatch_with_strategy(
        &self,
        tickets: &[String],
        worker: Option<&str>,
        strategy: DispatchStrategy,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError> {
        let repository = self.repository().map_err(WorkspaceDispatchError::Failed)?;
        let snapshot = repository.worker_pool().snapshot();
        let profile = snapshot
            .pool()
            .and_then(|pool| pool.profile())
            .cloned()
            .unwrap_or_else(|| wsg_core::AgentRuntimeProfile::new(AgentRuntime::Claude));
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
        let dispatcher = repository.direct_dispatch();
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
            })
            .collect();
        Ok(DispatchAdapterResult::new(DispatchResult {
            runtime,
            outcomes,
            partial: result.is_partial(),
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
        let actions = wsg_core::WorkerActions::new(repository);
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
        let wsg_core::FollowUpExecution::Background(run) = outcome.into_execution() else {
            return Err("foreground Worker actions are not supported by jjfx".to_owned());
        };
        let pid = run.pid();
        std::thread::spawn(move || {
            let _ = run.wait();
        });
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
        Ok(self.repository()?.worker_pool().reconcile_runs())
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

    fn discover_ready(&self, label: &str) -> Result<ReadyTicketResult, String> {
        let repository = self.repository()?;
        let runtime = repository
            .worker_pool()
            .snapshot()
            .pool()
            .and_then(|pool| pool.agent_runtime())
            .unwrap_or(AgentRuntime::Claude);
        let status = TicketStatus::parse("Todo").map_err(|error| error.to_string())?;
        let filter = ReadyTicketFilter::new(label, status).map_err(|error| error.to_string())?;
        let discovery = TicketDiscovery::new(configured_ticket_query(&repository, runtime));
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
        let id = TicketId::parse(parent.to_owned()).map_err(|error| error.to_string())?;
        let pool = repository.worker_pool().snapshot();
        let profile = pool.pool().and_then(|pool| pool.profile()).cloned();
        let runtime = profile
            .as_ref()
            .map_or(AgentRuntime::Claude, wsg_core::AgentRuntimeProfile::runtime);
        let mut request = wsg_core::OrchestrationRequest::new(id.clone(), runtime);
        if let Some(model) = profile
            .as_ref()
            .and_then(wsg_core::AgentRuntimeProfile::model)
        {
            request = request.with_model(model.clone());
        }
        let ticket = DirectDispatchRequest::for_ticket_id(id.clone(), RunMode::Background)
            .map_err(|error| error.to_string())?
            .ticket()
            .clone();
        let parent_ticket = wsg_core::ParentTicket::new(ticket.id().clone());
        let discovery = TicketDiscovery::new(configured_ticket_query(&repository, runtime));
        let runner = repository.orchestration_runner();
        let preparation = runner
            .prepare(&request, &parent_ticket, &discovery)
            .map_err(|error| error.to_string())?;
        emit(WorkspaceDispatchOrchestrationEvent::Started {
            parent: id.to_string(),
            resumed: preparation.resumed(),
        });
        match preparation.into_start() {
            wsg_core::OrchestrationStart::Direct(success) => {
                let wsg_core::DirectDispatchExecution::Background { pid } = success.execution()
                else {
                    return Err("foreground Parent Dispatch is not supported by jjfx".to_owned());
                };
                emit(WorkspaceDispatchOrchestrationEvent::Direct {
                    parent: id.to_string(),
                    worker: success.worker().to_string(),
                    pid: *pid,
                });
                return Ok(());
            }
            wsg_core::OrchestrationStart::Group => {}
        }

        let mut projection_error = None;
        let options = wsg_core::OrchestrationOptions::new();
        runner
            .run(&request, &options, |event| {
                if matches!(event, wsg_core::OrchestrationEvent::Started { .. }) {
                    return;
                }
                let notice = orchestration_notice(&event);
                let progress = match repository.state_store().dispatch_group(id.clone()).load() {
                    Ok(wsg_core::Loaded::Present(versioned)) => {
                        DispatchGroupProgress::from_state(versioned.value)
                    }
                    Ok(wsg_core::Loaded::Missing) => Err(format!(
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
                if let wsg_core::OrchestrationEvent::Terminal(summary) = event {
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
        let outcome = wsg_core::WorkerActions::new(repository)
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
        let outcome = wsg_core::WorkerActions::new(repository)
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
        let outcome = wsg_core::WorkerActions::new(repository)
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
        let outcome = wsg_core::WorkerActions::new(repository)
            .dismiss(&worker_id)
            .map_err(|error| error.to_string())?;
        let disposition = match outcome {
            wsg_core::DismissOutcome::Removed { capacity } => {
                WorkerDismissDisposition::Removed { capacity }
            }
            wsg_core::DismissOutcome::Reset => WorkerDismissDisposition::Reset,
        };
        Ok(WorkerCommandResult::Dismissed {
            worker: worker.to_owned(),
            disposition,
        })
    }

    fn worker_log(&self, worker: &str) -> Result<WorkerLogSnapshot, String> {
        let repository = self.repository()?;
        let worker_id = WorkerId::parse(worker.to_owned()).map_err(|error| error.to_string())?;
        let snapshot = repository.worker_pool().snapshot();
        let state = snapshot
            .worker(worker)
            .ok_or_else(|| format!("Worker {worker} was not found"))?;
        let runtime = state.agent_runtime().unwrap_or(AgentRuntime::Claude);
        let logs = wsg_core::WorkerActions::new(repository)
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
