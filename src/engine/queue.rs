//! The durable work queue: what jjfx has queued, claimed, run and settled.
//!
//! A Dispatch's held-back set (`waiting.json`) describes one Dispatch and is
//! rewritten by the next; a Dispatch Group tracks one parent Ticket's
//! Sub-issues. Neither is a queue: neither records what was claimed, what is in
//! flight, or what was abandoned, so nothing can be resumed and nothing replayed.
//!
//! This module is that record. Each unit of work is a *step*, identified by what
//! it is rather than when it ran, and each step's item is written to the pool's
//! own state directory before it is claimed and again as it ends. The record
//! therefore outlives the process that wrote it and is never recomputed from
//! live Worker state - which is what lets an interrupted Run be resumed rather
//! than lost (ticket 07) and a repeated step be recognised rather than silently
//! re-run (ticket 09).
//!
//! Writes are best-effort at the Dispatch seam, exactly as the waiting set is:
//! a Run that has already launched must not be misreported, or have its Worker
//! released, because a state write failed. Every transition is idempotent and
//! keyed by step identity, so a retry is safe.

use super::pool::current_timestamp;
use crate::engine::{
    InFlightRecord, QueueAdmission, QueueItem, QueueItemState, QueueSettlement,
    QueueStateRepository, QueuedStep, Repository, StateError, StepKey, WireTimestamp, WorkerStatus,
};

/// One step reconciliation resolved from the durable queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueResolution {
    /// The step resolved.
    pub step: StepKey,
    /// The state it was moved to.
    pub state: QueueItemState,
    /// Why, in the terms the record now carries.
    pub detail: String,
}

/// A deep interface to one Worker Pool's durable work queue.
#[derive(Debug, Clone)]
pub struct WorkQueue {
    repository: Repository,
}

impl Repository {
    /// Opens the durable work queue for the pool this handle acts on.
    pub fn work_queue(&self) -> WorkQueue {
        WorkQueue::new(self.clone())
    }
}

impl WorkQueue {
    fn new(repository: Repository) -> Self {
        Self { repository }
    }

    fn store(&self) -> QueueStateRepository {
        self.repository.state_store().queue()
    }

    /// Offers `steps` to the queue, reporting what happened to each.
    ///
    /// A step already recorded is left as it is, so a completed step offered
    /// again is reported rather than queued a second time.
    pub fn enqueue(&self, steps: &[QueuedStep]) -> Result<Vec<QueueAdmission>, StateError> {
        self.store().enqueue(steps)
    }

    /// Every queued item, in queue order.
    pub fn items(&self) -> Result<Vec<QueueItem>, StateError> {
        self.store().items()
    }

    /// The open items: queued, claimed or in flight.
    pub fn open(&self) -> Result<Vec<QueueItem>, StateError> {
        Ok(self
            .items()?
            .into_iter()
            .filter(|item| item.state.is_open())
            .collect())
    }

    /// One item by its step identity.
    pub fn item(&self, step: &StepKey) -> Result<Option<QueueItem>, StateError> {
        self.store().item(step)
    }

    /// Records that a Worker holds `step`, with what a resumption needs.
    ///
    /// This is the record ticket 07 reads: it is written before the Run starts,
    /// so a jjfx killed between the claim and the launch still knows what was in
    /// flight and on which Worker.
    ///
    /// A step keyed by identity means a second Dispatch of the same Ticket is
    /// the *same* step, so this is also where a later attempt supersedes an
    /// earlier one: a step that settled `failed` or `abandoned` is re-opened
    /// rather than left terminal, so the new Run's settlement replaces the stale
    /// outcome instead of the queue reporting the first failure forever
    /// (dogfood-2 ticket 06). [`QueueItemState::is_resumable`] is the one rule
    /// for what a new attempt may re-open, so a Dispatch cannot redo completed
    /// work, and a live `in_flight` Run is not clobbered by a second claim.
    pub fn claim(
        &self,
        step: &StepKey,
        in_flight: InFlightRecord,
        at: WireTimestamp,
    ) -> Result<bool, StateError> {
        self.store().transition(step, |item| {
            if !item.state.is_resumable() {
                return false;
            }
            let reopened = item.state.is_terminal();
            let mut changed = item.state != QueueItemState::Claimed
                || item.claimed_by != in_flight.worker
                || item.in_flight.as_ref() != Some(&in_flight);
            item.state = QueueItemState::Claimed;
            item.claimed_by = in_flight.worker.clone();
            if reopened {
                // The earlier attempt is superseded: its claim, start, end and
                // reason described work that is no longer this step's latest
                // outcome. The Run Records keep that attempt; the queue tracks
                // the new one, so "what happened to this?" answers with what
                // actually happened last.
                item.claimed_at = Some(at);
                item.started_at = None;
                item.finished_at = None;
                item.detail = None;
                changed = true;
            } else {
                item.claimed_at.get_or_insert(at);
            }
            item.in_flight = Some(in_flight);
            changed
        })
    }

    /// Records that the claimed step's Run has started.
    ///
    /// The session or the process is what only the launch knows, so this
    /// completes the in-flight record the claim left behind.
    pub fn begin(
        &self,
        step: &StepKey,
        session: Option<String>,
        pid: Option<i64>,
        at: WireTimestamp,
    ) -> Result<bool, StateError> {
        self.store().transition(step, |item| {
            if item.state.is_terminal() {
                return false;
            }
            let mut changed = item.state != QueueItemState::InFlight;
            item.state = QueueItemState::InFlight;
            if item.started_at.is_none() {
                item.started_at = Some(at);
                changed = true;
            }
            let record = item.in_flight.get_or_insert_with(InFlightRecord::default);
            if record.session != session {
                record.session = session;
                changed = true;
            }
            if record.pid != pid {
                record.pid = pid;
                changed = true;
            }
            changed
        })
    }

    /// Records how `step` ended, if it has not already ended.
    ///
    /// The first writer to observe the end owns it, exactly as the Run Record
    /// store does, so a Run finalised by its supervisor is not re-settled by a
    /// concurrent reconciliation.
    pub fn settle(
        &self,
        step: &StepKey,
        settlement: QueueSettlement,
        at: WireTimestamp,
    ) -> Result<bool, StateError> {
        self.store().transition(step, |item| {
            if item.state.is_terminal() {
                return false;
            }
            item.state = settlement.state();
            item.finished_at = Some(at);
            let detail = settlement.detail();
            item.detail = (!detail.is_empty()).then(|| detail.to_owned());
            true
        })
    }

    /// Records why an open step has not started, without changing its state.
    ///
    /// A step is queued without a reason and only learns one when a Dispatch
    /// cannot fire it, so the reason is written separately from the admission.
    pub fn note(&self, step: &StepKey, detail: impl Into<String>) -> Result<bool, StateError> {
        let detail = detail.into();
        self.store().transition(step, |item| {
            if item.state.is_terminal() {
                return false;
            }
            let changed = item.detail.as_deref() != Some(detail.as_str());
            item.detail = Some(detail);
            changed
        })
    }

    /// Records that a settled step was replayed rather than run again.
    ///
    /// A replay does not move a step: it is still the work that was already done,
    /// so this annotates the existing record's `detail` instead of transitioning
    /// it. That is how an operator tells a pass that replayed a recorded decision
    /// from one that ran and found nothing - a silent replay and an empty pass
    /// would otherwise read identically (ticket 12). A step the queue does not
    /// know is reported unchanged rather than invented.
    pub fn record_replay(
        &self,
        step: &StepKey,
        detail: impl Into<String>,
    ) -> Result<bool, StateError> {
        let detail = detail.into();
        self.store().transition(step, |item| {
            let changed = item.detail.as_deref() != Some(detail.as_str());
            item.detail = Some(detail);
            changed
        })
    }

    /// Returns a claimed or in-flight step to the queue.
    ///
    /// A launch that never happened leaves the work outstanding rather than
    /// failing it, so the step is queued again with the reason it did not run.
    pub fn requeue(&self, step: &StepKey, detail: Option<String>) -> Result<bool, StateError> {
        self.store().transition(step, |item| {
            if item.state.is_terminal() {
                return false;
            }
            item.state = QueueItemState::Queued;
            item.claimed_at = None;
            item.claimed_by = None;
            item.started_at = None;
            item.in_flight = None;
            item.detail = detail;
            true
        })
    }

    /// Re-opens `step` for another attempt, reporting whether it was resumable.
    ///
    /// A resumption is explicit - an operator asked for it - and it re-opens the
    /// record rather than leaving a terminal state in place, so the claim and
    /// the settlement of the new Run apply to this step. A step that is already
    /// done, or one whose Run is still in flight, is refused: the first would
    /// redo completed work, the second would race a Run jjfx believes is live.
    ///
    /// The state it was resumed from, and why it was in that state, are carried
    /// into `detail`, so "what happened to this?" answers while the new attempt
    /// runs; once that attempt settles, `detail` is its own outcome again and the
    /// Run Records keep each attempt separately.
    pub fn resume(&self, step: &StepKey) -> Result<bool, StateError> {
        self.store().transition(step, |item| {
            if !item.state.is_resumable() {
                return false;
            }
            let previous = item.state;
            let reason = match item.detail.take().filter(|detail| !detail.is_empty()) {
                Some(detail) => format!("resumed after {previous}: {detail}"),
                None => format!("resumed after {previous}"),
            };
            item.state = QueueItemState::Queued;
            item.claimed_at = None;
            item.claimed_by = None;
            item.started_at = None;
            item.finished_at = None;
            item.in_flight = None;
            item.detail = Some(reason);
            true
        })
    }

    /// Resolves every claimed or in-flight step against live Worker state.
    ///
    /// The queue is read, not inferred: the pass iterates the durable items, so
    /// a step whose Worker has left the Pool, or has moved on to other work, is
    /// still resolved - and the resolution is written back, so "what happened to
    /// this?" has an answer even when nothing live remembers the step.
    pub fn reconcile(&self) -> Result<Vec<QueueResolution>, StateError> {
        let live = self.repository.worker_pool().snapshot();
        let mut resolutions = Vec::new();
        for item in self.open()? {
            if item.state == QueueItemState::Queued {
                continue;
            }
            let worker = item
                .in_flight
                .as_ref()
                .and_then(|record| record.worker.clone())
                .or_else(|| item.claimed_by.clone());
            let settlement = match worker {
                None => QueueSettlement::Abandoned {
                    detail: format!("step {} was claimed with no Worker recorded", item.subject),
                },
                Some(worker) => match live.worker(worker.as_str()) {
                    None => QueueSettlement::Abandoned {
                        detail: format!("Worker {worker} is no longer in the Worker Pool"),
                    },
                    Some(observed) if observed.ticket() != Some(item.subject.as_str()) => {
                        QueueSettlement::Abandoned {
                            detail: format!(
                                "Worker {worker} no longer holds Ticket {}",
                                item.subject
                            ),
                        }
                    }
                    Some(observed) => match observed.status() {
                        // Still running: leave it in flight.
                        WorkerStatus::Busy => continue,
                        WorkerStatus::Done => QueueSettlement::Done,
                        WorkerStatus::Failed => QueueSettlement::Failed {
                            detail: observed
                                .error()
                                .filter(|error| !error.is_empty())
                                .unwrap_or("the Run failed")
                                .to_owned(),
                        },
                        WorkerStatus::Idle => QueueSettlement::Abandoned {
                            detail: format!(
                                "Worker {worker} is idle without a Run for Ticket {}",
                                item.subject
                            ),
                        },
                    },
                },
            };
            let detail = settlement.detail().to_owned();
            let state = settlement.state();
            let step = item.step;
            let at = current_timestamp()
                .map_err(|error| StateError::new("timestamp", "Work Queue", error))?;
            if self.settle(&step, settlement, at)? {
                resolutions.push(QueueResolution {
                    step,
                    state,
                    detail,
                });
            }
        }
        Ok(resolutions)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::engine::state::StateStore;
    use crate::engine::{AgentRuntime, AgentRuntimeProfile, WorkerId};

    fn queue() -> (tempfile::TempDir, WorkQueue) {
        let temp = tempfile::tempdir().expect("temp repository");
        fs::create_dir(temp.path().join(".jj")).expect("repository marker");
        let repository = Repository::open(temp.path()).expect("repository");
        let queue = repository.work_queue();
        (temp, queue)
    }

    fn step(ticket: &str) -> QueuedStep {
        QueuedStep::dispatch(ticket, WireTimestamp::new("2026-10-19T00:00:00Z"), None)
    }

    fn review_step(ticket: &str, dimension: &str, pass: u32) -> QueuedStep {
        QueuedStep::review(
            ticket,
            dimension,
            pass,
            WireTimestamp::new("2026-10-19T00:00:00Z"),
            None,
        )
    }

    fn at(seconds: u32) -> WireTimestamp {
        WireTimestamp::new(format!("2026-10-19T00:00:{seconds:02}Z"))
    }

    fn record(worker: &str) -> InFlightRecord {
        InFlightRecord::for_run(
            WorkerId::parse(worker).expect("Worker id"),
            format!("/tmp/{worker}"),
            "eng-1",
            &AgentRuntimeProfile::new(AgentRuntime::Claude),
        )
    }

    #[test]
    fn a_missing_queue_reads_as_nothing_queued() {
        let (_temp, queue) = queue();
        assert!(queue.items().expect("items").is_empty());
        assert!(queue.open().expect("open").is_empty());
    }

    #[test]
    fn queueing_is_durable_and_ordered() {
        let (temp, queue) = queue();
        let admissions = queue
            .enqueue(&[step("ENG-1"), step("ENG-2")])
            .expect("enqueue");
        assert_eq!(
            admissions,
            [
                QueueAdmission::Queued { sequence: 0 },
                QueueAdmission::Queued { sequence: 1 }
            ]
        );

        // A new handle over the same state - the process that wrote it is gone.
        let reopened = Repository::open(temp.path())
            .expect("repository")
            .work_queue();
        let items = reopened.items().expect("items");
        assert_eq!(
            items
                .iter()
                .map(|item| (item.step.as_str(), item.sequence))
                .collect::<Vec<_>>(),
            [("dispatch:ENG-1", 0), ("dispatch:ENG-2", 1)]
        );
        assert!(
            items
                .iter()
                .all(|item| item.state == QueueItemState::Queued)
        );
    }

    /// Ticket 09: the same step offered twice is recognised, not queued again.
    #[test]
    fn re_offering_a_settled_step_is_reported_rather_than_requeued() {
        let (_temp, queue) = queue();
        queue.enqueue(&[step("ENG-1")]).expect("enqueue");
        let key = StepKey::dispatch("ENG-1");
        queue
            .settle(&key, QueueSettlement::Done, at(1))
            .expect("settle");

        let admissions = queue.enqueue(&[step("ENG-1")]).expect("re-enqueue");

        assert_eq!(
            admissions,
            [QueueAdmission::Known {
                state: QueueItemState::Done
            }]
        );
        assert_eq!(queue.items().expect("items").len(), 1);
    }

    /// Ticket 09: the recurring unit is a review dimension and pass, so the same
    /// dimension reviewed again on a later round is the same step - a repeat is
    /// recognised rather than run a second time.
    #[test]
    fn re_offering_a_settled_review_step_is_reported_rather_than_requeued() {
        let (_temp, queue) = queue();
        let offered = review_step("ENG-1", "security", 1);
        queue
            .enqueue(std::slice::from_ref(&offered))
            .expect("enqueue");
        let key = StepKey::review("ENG-1", "security", 1);
        queue
            .settle(&key, QueueSettlement::Done, at(1))
            .expect("settle");

        let admissions = queue.enqueue(&[offered]).expect("re-enqueue");

        assert_eq!(
            admissions,
            [QueueAdmission::Known {
                state: QueueItemState::Done
            }]
        );
        assert_eq!(queue.items().expect("items").len(), 1);
    }

    /// A review step and the Dispatch of the same Ticket are different work, and
    /// a later pass of one dimension is different again.
    #[test]
    fn a_review_step_is_distinct_from_a_dispatch_and_from_another_pass() {
        let (_temp, queue) = queue();
        queue
            .enqueue(&[
                step("ENG-1"),
                review_step("ENG-1", "security", 1),
                review_step("ENG-1", "security", 2),
            ])
            .expect("enqueue");

        let items = queue.items().expect("items");
        assert_eq!(items.len(), 3);
        assert_eq!(
            items
                .iter()
                .map(|item| item.step.as_str())
                .collect::<Vec<_>>(),
            [
                "dispatch:ENG-1",
                "review:ENG-1:security:1",
                "review:ENG-1:security:2"
            ]
        );
        assert_eq!(items[1].kind.as_str(), "review");
        assert_eq!(items[1].subject, "ENG-1 security pass 1");
    }

    #[test]
    fn a_claim_records_what_a_resumption_needs_before_the_run_starts() {
        let (_temp, queue) = queue();
        queue.enqueue(&[step("ENG-1")]).expect("enqueue");
        let key = StepKey::dispatch("ENG-1");

        queue
            .claim(&key, record("worker-01"), at(1))
            .expect("claim");
        let item = queue.item(&key).expect("item").expect("recorded");

        assert_eq!(item.state, QueueItemState::Claimed);
        assert_eq!(
            item.claimed_by.as_ref().map(WorkerId::as_str),
            Some("worker-01")
        );
        assert_eq!(item.claimed_at, Some(at(1)));
        let in_flight = item.in_flight.expect("in-flight record");
        assert_eq!(
            in_flight.worker.as_ref().map(WorkerId::as_str),
            Some("worker-01")
        );
        assert_eq!(in_flight.workspace.as_deref(), Some("/tmp/worker-01"));
        assert_eq!(in_flight.runtime.as_deref(), Some("claude"));
        assert_eq!(in_flight.session, None);
    }

    #[test]
    fn beginning_a_claimed_step_completes_the_in_flight_record() {
        let (_temp, queue) = queue();
        queue.enqueue(&[step("ENG-1")]).expect("enqueue");
        let key = StepKey::dispatch("ENG-1");
        queue
            .claim(&key, record("worker-01"), at(1))
            .expect("claim");

        queue
            .begin(&key, Some("ses_abc".to_owned()), None, at(2))
            .expect("begin");
        let item = queue.item(&key).expect("item").expect("recorded");

        assert_eq!(item.state, QueueItemState::InFlight);
        assert_eq!(item.started_at, Some(at(2)));
        assert_eq!(
            item.in_flight.expect("record").session.as_deref(),
            Some("ses_abc")
        );
    }

    #[test]
    fn settling_is_terminal_and_first_writer_wins() {
        let (_temp, queue) = queue();
        queue.enqueue(&[step("ENG-1")]).expect("enqueue");
        let key = StepKey::dispatch("ENG-1");
        queue
            .claim(&key, record("worker-01"), at(1))
            .expect("claim");

        assert!(
            queue
                .settle(
                    &key,
                    QueueSettlement::Failed {
                        detail: "boom".to_owned()
                    },
                    at(2),
                )
                .expect("settle")
        );
        assert!(
            !queue
                .settle(&key, QueueSettlement::Done, at(3))
                .expect("re-settle")
        );

        let item = queue.item(&key).expect("item").expect("recorded");
        assert_eq!(item.state, QueueItemState::Failed);
        assert_eq!(item.finished_at, Some(at(2)));
        assert_eq!(item.detail.as_deref(), Some("boom"));
    }

    #[test]
    fn requeueing_returns_an_unstarted_step_to_the_queue() {
        let (_temp, queue) = queue();
        queue.enqueue(&[step("ENG-1")]).expect("enqueue");
        let key = StepKey::dispatch("ENG-1");
        queue
            .claim(&key, record("worker-01"), at(1))
            .expect("claim");

        queue
            .requeue(&key, Some("released before launch".to_owned()))
            .expect("requeue");
        let item = queue.item(&key).expect("item").expect("recorded");

        assert_eq!(item.state, QueueItemState::Queued);
        assert_eq!(item.claimed_by, None);
        assert_eq!(item.in_flight, None);
        assert_eq!(item.detail.as_deref(), Some("released before launch"));
    }

    /// Ticket 12: a replayed step is still done, so the replay annotates the
    /// record rather than moving it - which is how `status --json` tells a
    /// replayed pass from a pass that ran and found nothing.
    #[test]
    fn a_replay_is_recorded_on_the_settled_step() {
        let (_temp, queue) = queue();
        let offered = review_step("ENG-1", "security", 1);
        queue
            .enqueue(std::slice::from_ref(&offered))
            .expect("enqueue");
        let key = StepKey::review("ENG-1", "security", 1);
        queue
            .settle(&key, QueueSettlement::Done, at(1))
            .expect("settle");

        assert!(
            queue
                .record_replay(&key, "replayed from the recorded decision")
                .expect("record the replay")
        );

        let item = queue.item(&key).expect("item").expect("recorded");
        assert_eq!(
            item.state,
            QueueItemState::Done,
            "a replay does not move the step"
        );
        assert_eq!(
            item.detail.as_deref(),
            Some("replayed from the recorded decision")
        );
        // Recording the same replay again is a no-op, and a step the queue never
        // held is not invented.
        assert!(
            !queue
                .record_replay(&key, "replayed from the recorded decision")
                .expect("re-record the replay")
        );
        assert!(
            !queue
                .record_replay(&StepKey::review("ENG-9", "security", 1), "replayed")
                .expect("record a replay for an unrecorded step")
        );
    }

    /// Ticket 07: a resumption reads the queue's own per-step state to tell what
    /// is already done, so completed work is left alone and a Run reconciliation
    /// has just called live is not raced.
    #[test]
    fn the_resumable_rule_is_the_queue_state_rather_than_a_second_progress_notion() {
        assert!(QueueItemState::Queued.is_resumable());
        assert!(QueueItemState::Claimed.is_resumable());
        assert!(!QueueItemState::InFlight.is_resumable());
        assert!(!QueueItemState::Done.is_resumable());
        assert!(QueueItemState::Failed.is_resumable());
        assert!(QueueItemState::Abandoned.is_resumable());
    }

    /// Re-opening is what makes a settled step runnable again: a claim is
    /// refused for a terminal step, so a resumption must clear it first.
    #[test]
    fn resuming_re_opens_unfinished_work_and_refuses_completed_or_running_work() {
        let (_temp, queue) = queue();
        queue
            .enqueue(&[step("ENG-1"), step("ENG-2"), step("ENG-3")])
            .expect("enqueue");
        queue
            .settle(&StepKey::dispatch("ENG-1"), QueueSettlement::Done, at(1))
            .expect("settle");
        queue
            .claim(&StepKey::dispatch("ENG-2"), record("worker-01"), at(1))
            .expect("claim");
        queue
            .begin(
                &StepKey::dispatch("ENG-2"),
                Some("ses_abc".to_owned()),
                None,
                at(2),
            )
            .expect("begin");
        queue
            .claim(&StepKey::dispatch("ENG-3"), record("worker-02"), at(1))
            .expect("claim");
        queue
            .settle(
                &StepKey::dispatch("ENG-3"),
                QueueSettlement::Abandoned {
                    detail: "Worker worker-02 is no longer in the Worker Pool".to_owned(),
                },
                at(2),
            )
            .expect("settle");

        assert!(
            !queue
                .resume(&StepKey::dispatch("ENG-1"))
                .expect("resume a completed step")
        );
        assert!(
            !queue
                .resume(&StepKey::dispatch("ENG-2"))
                .expect("resume a running step")
        );
        assert!(
            !queue
                .resume(&StepKey::dispatch("ENG-9"))
                .expect("resume an unrecorded step")
        );
        assert!(
            queue
                .resume(&StepKey::dispatch("ENG-3"))
                .expect("resume an abandoned step")
        );

        // The re-opened step is runnable again and carries why it was resumed.
        let item = queue
            .item(&StepKey::dispatch("ENG-3"))
            .expect("item")
            .expect("recorded");
        assert_eq!(item.state, QueueItemState::Queued);
        assert_eq!(item.claimed_at, None);
        assert_eq!(item.claimed_by, None);
        assert_eq!(item.started_at, None);
        assert_eq!(item.finished_at, None);
        assert_eq!(item.in_flight, None);
        assert_eq!(
            item.detail.as_deref(),
            Some("resumed after abandoned: Worker worker-02 is no longer in the Worker Pool")
        );
        assert!(
            queue
                .claim(&StepKey::dispatch("ENG-3"), record("worker-02"), at(3))
                .expect("claim the re-opened step"),
            "a re-opened step must be claimable, or the resumption runs nothing"
        );

        // The completed and the running steps are exactly as they were.
        let done = queue
            .item(&StepKey::dispatch("ENG-1"))
            .expect("item")
            .expect("recorded");
        assert_eq!(done.state, QueueItemState::Done);
        assert_eq!(done.finished_at, Some(at(1)));
        let running = queue
            .item(&StepKey::dispatch("ENG-2"))
            .expect("item")
            .expect("recorded");
        assert_eq!(running.state, QueueItemState::InFlight);
        assert_eq!(
            running
                .in_flight
                .and_then(|record| record.session)
                .as_deref(),
            Some("ses_abc")
        );
    }

    /// Dogfood-2 ticket 06: a step keyed by identity means a second Dispatch of
    /// the same Ticket is the same step. A later claim re-opens a step that
    /// settled `failed`, so the new attempt's settlement supersedes the earlier
    /// failure rather than the queue reporting the first outcome forever.
    #[test]
    fn a_later_claim_supersedes_an_earlier_failure() {
        let (_temp, queue) = queue();
        queue.enqueue(&[step("ENG-1")]).expect("enqueue");
        let key = StepKey::dispatch("ENG-1");
        queue
            .claim(&key, record("worker-01"), at(1))
            .expect("claim");
        queue
            .begin(&key, Some("ses_first".to_owned()), None, at(2))
            .expect("begin");
        queue
            .settle(
                &key,
                QueueSettlement::Failed {
                    detail: "cannot anchor the session".to_owned(),
                },
                at(3),
            )
            .expect("settle the first attempt failed");

        // A second Dispatch of the same Ticket claims the same step, re-opening
        // the record so the new Run's claim and settlement apply to it.
        assert!(
            queue
                .claim(&key, record("worker-02"), at(4))
                .expect("re-claim the failed step"),
            "a failed step must be re-claimable by a later Dispatch"
        );
        let claimed = queue.item(&key).expect("item").expect("recorded");
        assert_eq!(claimed.state, QueueItemState::Claimed);
        assert_eq!(
            claimed.claimed_by.as_ref().map(WorkerId::as_str),
            Some("worker-02")
        );
        assert_eq!(claimed.claimed_at, Some(at(4)), "the new attempt's claim");
        assert_eq!(claimed.started_at, None, "the new attempt has not started");
        assert_eq!(
            claimed.finished_at, None,
            "the earlier failure's end no longer applies"
        );
        assert_eq!(
            claimed.detail, None,
            "the earlier failure's reason no longer applies"
        );

        queue
            .begin(&key, Some("ses_second".to_owned()), None, at(5))
            .expect("begin the retry");
        queue
            .settle(&key, QueueSettlement::Done, at(6))
            .expect("settle the retry succeeded");

        let done = queue.item(&key).expect("item").expect("recorded");
        assert_eq!(
            done.state,
            QueueItemState::Done,
            "the queue must reflect the latest outcome, not the first failure"
        );
        assert_eq!(done.finished_at, Some(at(6)));
        assert_eq!(done.detail, None, "a successful step carries no failure");
        // The queue no longer reads failed, so a resumption leaves it alone.
        assert!(
            !queue.resume(&key).expect("resume a completed step"),
            "completed work must not be re-opened"
        );
    }

    /// The re-open is bounded by the same rule a resumption uses: a completed
    /// step is not redone, and a live Run is not clobbered by a second claim.
    #[test]
    fn a_claim_refuses_completed_and_running_steps() {
        let (_temp, queue) = queue();
        queue
            .enqueue(&[step("ENG-1"), step("ENG-2")])
            .expect("enqueue");

        let completed = StepKey::dispatch("ENG-1");
        queue
            .settle(&completed, QueueSettlement::Done, at(1))
            .expect("settle");
        assert!(
            !queue
                .claim(&completed, record("worker-01"), at(2))
                .expect("claim a completed step"),
            "a completed step must not be re-claimed"
        );
        let item = queue.item(&completed).expect("item").expect("recorded");
        assert_eq!(item.state, QueueItemState::Done);
        assert_eq!(item.finished_at, Some(at(1)), "completion is left intact");

        let running = StepKey::dispatch("ENG-2");
        queue
            .claim(&running, record("worker-01"), at(1))
            .expect("claim");
        queue
            .begin(&running, Some("ses_live".to_owned()), None, at(2))
            .expect("begin");
        assert!(
            !queue
                .claim(&running, record("worker-02"), at(3))
                .expect("claim a running step"),
            "a live Run must not be re-claimed by a second Dispatch"
        );
        let item = queue.item(&running).expect("item").expect("recorded");
        assert_eq!(item.state, QueueItemState::InFlight);
        assert_eq!(
            item.claimed_by.as_ref().map(WorkerId::as_str),
            Some("worker-01"),
            "the live Run's claim must be left intact"
        );
        assert_eq!(
            item.in_flight.and_then(|record| record.session).as_deref(),
            Some("ses_live"),
            "the live Run's record must be left intact"
        );
    }

    /// A step the queue never held is not invented by a transition for it.
    #[test]
    fn a_transition_for_an_unrecorded_step_changes_nothing() {
        let (_temp, queue) = queue();
        let key = StepKey::dispatch("ENG-9");

        assert!(
            !queue
                .settle(&key, QueueSettlement::Done, at(1))
                .expect("settle")
        );
        assert!(queue.items().expect("items").is_empty());
    }

    #[test]
    fn an_unreadable_queue_is_an_error_rather_than_an_empty_one() {
        let (temp, queue) = queue();
        let path = temp.path().join(".jj/pools/default/queue.json");
        fs::create_dir_all(path.parent().expect("parent")).expect("state directory");
        fs::write(&path, b"{ not json").expect("write queue");

        let error = queue
            .items()
            .expect_err("a malformed queue must be reported");

        assert!(error.to_string().contains("Work Queue"), "{error}");
    }

    /// The queue is a sidecar document, not a jj artefact: writing it must not
    /// add an operation to the repository's operation log.
    #[test]
    fn queueing_does_not_touch_the_jj_operation_log() {
        let temp = tempfile::tempdir().expect("temp repository");
        let init = jj(temp.path(), &["git", "init", "--no-colocate"]);
        assert!(
            init.status.success(),
            "jj init: {}",
            String::from_utf8_lossy(&init.stderr)
        );
        let repository = Repository::open(temp.path()).expect("repository");
        let before = jj_operation_count(temp.path());

        repository
            .work_queue()
            .enqueue(&[step("ENG-1")])
            .expect("enqueue");

        assert_eq!(jj_operation_count(temp.path()), before);
    }

    /// Run one `jj` command against a throwaway repository.
    ///
    /// The identity is passed as `--config` rather than set with `jj config set
    /// --repo`: jj stores repo config under a directory derived from
    /// `XDG_CONFIG_HOME`, so a `--repo` write can land where a later command
    /// never looks, and a developer's own user config would hide that.
    fn jj(root: &std::path::Path, args: &[&str]) -> std::process::Output {
        std::process::Command::new("jj")
            .args([
                "--config",
                "user.name=Test User",
                "--config",
                "user.email=test@example.com",
                "--config",
                "signing.behavior=drop",
            ])
            .args(args)
            .current_dir(root)
            .output()
            .expect("jj op log")
    }

    fn jj_operation_count(root: &std::path::Path) -> usize {
        let output = jj(root, &["op", "log", "--no-graph", "-T", "id ++ \"\\n\""]);
        assert!(
            output.status.success(),
            "jj op log: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count()
    }

    #[test]
    fn the_state_store_opens_the_same_queue_as_the_repository() {
        let (temp, queue) = queue();
        queue.enqueue(&[step("ENG-1")]).expect("enqueue");
        let store: StateStore = Repository::open(temp.path())
            .expect("repository")
            .state_store();
        assert_eq!(store.queue().items().expect("items").len(), 1);
    }
}
