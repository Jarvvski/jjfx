//! Steps keyed by identity: a repeat is recognised, and a recorded decision is
//! replayed rather than re-derived.
//!
//! The durable queue (ticket 06) records what a step *is*, so these tests are
//! about the two properties that follow: the same review dimension and pass
//! offered twice is the same step and is reported rather than run again, and a
//! decision a step already recorded is read back from disk instead of the model
//! being asked to decide it a second time.

use std::fs;

use jjfx::engine::{
    QueueAdmission, QueueItemState, QueueSettlement, QueuedStep, REPLAYED_DETAIL,
    RecordedReviewLoop, ReplayingReviewer, Repository, ReviewCompleteness, ReviewFinding,
    ReviewLoop, ReviewOutcome, ReviewRequest, ReviewSeverity, ReviewStop, ReviewSubject, Reviewer,
    ReviewerError, StepKey, WireTimestamp,
};
use tempfile::TempDir;

/// A Repository whose `.jj` marker exists, so the state layer can open it.
fn repository() -> (TempDir, Repository) {
    let temp = TempDir::new().expect("temporary repository");
    fs::create_dir(temp.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(temp.path()).expect("repository");
    (temp, repository)
}

fn at() -> WireTimestamp {
    WireTimestamp::new("2026-10-19T00:00:00Z")
}

fn request() -> ReviewRequest {
    ReviewRequest::new("owner/repo", "main", "adam/feature")
}

fn finding(title: &str) -> ReviewFinding {
    ReviewFinding::new(
        ReviewSeverity::Major,
        Some("src/x.rs".to_owned()),
        Some((12, 14)),
        Some(title.to_owned()),
        "Cap the loop.",
        vec!["n = 3".to_owned()],
    )
}

/// A reviewer that decides the same thing every time it is asked.
struct ScriptedReviewer;

impl Reviewer for ScriptedReviewer {
    fn review(&self, _: &ReviewRequest) -> Result<ReviewOutcome, ReviewerError> {
        Ok(ReviewOutcome::new(
            vec![finding("bound the loop")],
            ReviewCompleteness::Complete,
        ))
    }
}

/// A reviewer that fails loudly if it is ever asked, so a test can prove a
/// replayed decision never reached the model.
struct PanickingReviewer;

impl Reviewer for PanickingReviewer {
    fn review(&self, _: &ReviewRequest) -> Result<ReviewOutcome, ReviewerError> {
        panic!("a replayed decision must not re-invoke the reviewer");
    }
}

/// Ticket 09, criterion 4: the same review step driven twice is recognised the
/// second time - and recognised by a fresh handle, so the identity is the
/// durable record rather than this process's memory.
#[test]
fn a_review_step_driven_twice_is_recognised_the_second_time() {
    let (temp, repository) = repository();
    let offered = QueuedStep::review("ENG-1", "security", 1, at(), None);
    let step = StepKey::review("ENG-1", "security", 1);

    let first = repository
        .work_queue()
        .enqueue(std::slice::from_ref(&offered))
        .expect("offer the step");
    assert_eq!(first, [QueueAdmission::Queued { sequence: 0 }]);
    repository
        .work_queue()
        .settle(&step, QueueSettlement::Done, at())
        .expect("settle the step");

    // A fresh handle over the same Repository: the record is read from disk.
    let reopened = Repository::open(temp.path()).expect("reopen repository");
    let second = reopened
        .work_queue()
        .enqueue(&[offered])
        .expect("offer the step again");

    assert_eq!(
        second,
        [QueueAdmission::Known {
            state: QueueItemState::Done
        }],
        "a completed step offered again must be reported, not queued a second time"
    );
    assert_eq!(
        reopened.work_queue().items().expect("read the queue").len(),
        1,
        "the repeat must not add a second step"
    );
}

/// Ticket 09, criterion 3: a decision is replayed from what was stored, without
/// calling the model again - and a fresh process can replay it.
#[test]
fn a_recorded_decision_is_replayed_from_disk_without_the_model() {
    let (temp, repository) = repository();
    let step = StepKey::review("ENG-1", "security", 1);

    let decided = ReplayingReviewer::new(
        repository.state_store().decisions(),
        step.clone(),
        ScriptedReviewer,
    )
    .review(&request())
    .expect("the first pass decides");
    assert_eq!(decided.findings().len(), 1);

    // A fresh handle reads the recorded decision from disk. The inner reviewer
    // panics if it is asked, so a replay that reached the model fails the test.
    let replayed = ReplayingReviewer::new(
        Repository::open(temp.path())
            .expect("reopen repository")
            .state_store()
            .decisions(),
        step,
        PanickingReviewer,
    )
    .review(&request())
    .expect("the recorded decision replays");

    assert_eq!(
        replayed, decided,
        "a replay must reproduce exactly what was recorded"
    );
}

/// Ticket 12, at the loop level: a repeated `(ticket, dimension, pass)` does not
/// reach the model. The first drive decides through a scripted reviewer; the
/// second runs through a reviewer that panics if it is asked, so a pass that
/// reached the model fails the test - and the replay is visible in the durable
/// queue an operator reads.
#[test]
fn a_review_loop_driven_twice_replays_without_reaching_the_model() {
    let (temp, repository) = repository();
    let subject = ReviewSubject::new("ENG-1", "security");

    let first = RecordedReviewLoop::new(
        repository.state_store().decisions(),
        repository.work_queue(),
        subject.clone(),
    )
    .run(ReviewLoop::new(3), &ScriptedReviewer, &request(), |_, _| {
        Ok::<(), String>(())
    })
    .expect("the first drive decides");

    assert_eq!(first.passes(), 2);
    assert_eq!(first.stop(), ReviewStop::NoProgress);
    assert_eq!(first.replayed(), 0, "the first drive decided every pass");

    // A fresh handle over the same Repository: the decisions and the queue are
    // read from disk, so a replay is durable rather than this process's memory.
    let reopened = Repository::open(temp.path()).expect("reopen repository");
    let second = RecordedReviewLoop::new(
        reopened.state_store().decisions(),
        reopened.work_queue(),
        subject.clone(),
    )
    .run(
        ReviewLoop::new(3),
        &PanickingReviewer,
        &request(),
        |_, _| Ok::<(), String>(()),
    )
    .expect("the second drive replays");

    assert_eq!(second.passes(), 2);
    assert_eq!(second.stop(), ReviewStop::NoProgress);
    assert_eq!(second.replayed(), 2);
    assert_eq!(second.replayed_passes(), [1, 2]);

    // The operator sees the replay in `status --json`'s queue: the step is still
    // done, and its detail names the replay rather than leaving it silent.
    for pass in 1..=2 {
        let item = reopened
            .work_queue()
            .item(&subject.step(pass))
            .expect("item")
            .expect("recorded");
        assert_eq!(item.state, QueueItemState::Done);
        assert_eq!(item.detail.as_deref(), Some(REPLAYED_DETAIL));
    }
}
