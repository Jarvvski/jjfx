//! Dispatch eligibility: which Worker Workspaces a frontend has ruled out of a
//! Dispatch decision.
//!
//! Eligibility is derived, not persisted as a lifecycle: a Worker is eligible
//! when it is a Pool member, its Worker Status is idle, and it is not occupied
//! (open in a terminal). A frontend computes the occupied set from live terminal
//! state and hands it to the Dispatch coordinator as [`DispatchExclusions`]; the
//! engine enforces it while holding the Pool lock. The engine never inspects the
//! terminal itself.

use std::collections::BTreeSet;

use crate::engine::{Repository, WorkerId};

/// Worker Workspaces excluded from Dispatch selection for one operation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DispatchExclusions {
    workers: BTreeSet<WorkerId>,
}

impl DispatchExclusions {
    /// Creates an exclusion set from the given Workers.
    pub fn new(workers: impl IntoIterator<Item = WorkerId>) -> Self {
        Self {
            workers: workers.into_iter().collect(),
        }
    }

    /// Returns whether `worker` is excluded from selection.
    pub fn contains(&self, worker: &WorkerId) -> bool {
        self.workers.contains(worker)
    }

    /// Returns whether nothing is excluded.
    pub fn is_empty(&self) -> bool {
        self.workers.is_empty()
    }
}

/// The Workers excluded from Dispatch because their Workspace is open in a
/// terminal.
///
/// `open` is the set of workspace names with an open terminal tab, as reported
/// by the frontend's terminal seam. Only Pool members are ever excluded. The
/// reconciled set is written back to the durable in-use marker (best effort) so
/// a later `--force` run and the TUI can see what jjfx last observed. The engine
/// never queries the terminal itself.
pub fn reconcile_occupancy(repository: &Repository, open: &BTreeSet<String>) -> DispatchExclusions {
    let snapshot = repository.worker_pool().snapshot();
    let occupied: BTreeSet<WorkerId> = snapshot
        .pool()
        .map(|pool| {
            pool.workers()
                .iter()
                .map(|worker| worker.worker_id().clone())
                .filter(|worker| open.contains(worker.as_str()))
                .collect()
        })
        .unwrap_or_default();
    let _ = repository.state_store().in_use().replace(occupied.clone());
    DispatchExclusions::new(occupied)
}
