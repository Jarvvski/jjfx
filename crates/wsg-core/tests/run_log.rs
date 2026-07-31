use std::time::Duration;

use wsg_core::{
    CollaborationEvent, CollaborationParticipant, RunActivity, RunActivityKind, RunActivityStatus,
    RunConclusion, RunCost, RunResult, RunUsage,
};

#[test]
fn failed_run_result_preserves_normalized_usage_and_completion_details() {
    let usage = RunUsage::new(1_200, 300)
        .with_cached_input_tokens(400)
        .with_cache_write_input_tokens(50)
        .with_reasoning_output_tokens(75);
    let cost = RunCost::from_micro_usd(420_000);
    let result = RunResult::failed("approval denied")
        .with_usage(usage.clone())
        .with_duration(Duration::from_secs(5))
        .with_turns(3)
        .with_cost(cost);

    assert_eq!(
        result.conclusion(),
        &RunConclusion::Failed {
            message: "approval denied".to_owned(),
        }
    );
    assert_eq!(result.usage(), Some(&usage));
    assert_eq!(result.duration(), Some(Duration::from_secs(5)));
    assert_eq!(result.turns(), Some(3));
    assert_eq!(result.cost(), Some(cost));
    assert_eq!(result.cost().map(RunCost::as_micro_usd), Some(420_000));
    assert_eq!(usage.input_tokens(), 1_200);
    assert_eq!(usage.cached_input_tokens(), 400);
    assert_eq!(usage.cache_write_input_tokens(), 50);
    assert_eq!(usage.output_tokens(), 300);
    assert_eq!(usage.reasoning_output_tokens(), 75);
}

#[test]
fn successful_run_result_needs_no_failure_message_or_optional_metrics() {
    let result = RunResult::succeeded();

    assert_eq!(result.conclusion(), &RunConclusion::Succeeded);
    assert_eq!(result.usage(), None);
    assert_eq!(result.duration(), None);
    assert_eq!(result.turns(), None);
    assert_eq!(result.cost(), None);
}

#[test]
fn activity_preserves_meaning_without_provider_event_shapes() {
    let usage = RunUsage::new(2_000, 500).with_cached_input_tokens(1_000);
    let tool = RunActivity::new(RunActivityKind::Tool {
        name: "linear.save_issue".to_owned(),
        detail: Some("AMBA-42".to_owned()),
        status: RunActivityStatus::Failed {
            message: Some("approval denied".to_owned()),
        },
    })
    .with_usage(usage.clone());

    assert_eq!(
        tool.kind(),
        &RunActivityKind::Tool {
            name: "linear.save_issue".to_owned(),
            detail: Some("AMBA-42".to_owned()),
            status: RunActivityStatus::Failed {
                message: Some("approval denied".to_owned()),
            },
        }
    );
    assert_eq!(tool.usage(), Some(&usage));

    let remaining = [
        RunActivityKind::SessionStarted,
        RunActivityKind::Message {
            text: "Implemented the fix".to_owned(),
        },
        RunActivityKind::FileChanges {
            paths: vec!["src/lib.rs".to_owned(), "tests/lib.rs".to_owned()],
        },
        RunActivityKind::Plan {
            completed: 2,
            total: 3,
        },
        RunActivityKind::Reasoning {
            text: "Checking the failure".to_owned(),
        },
        RunActivityKind::Warning {
            message: "PATH aliases unavailable".to_owned(),
        },
    ];

    for kind in remaining {
        let activity = RunActivity::new(kind.clone());
        assert_eq!(activity.kind(), &kind);
        assert_eq!(activity.usage(), None);
    }
}

#[test]
fn collaboration_preserves_participant_failure_context_without_allocating_workers() {
    let participant = CollaborationParticipant::new("agent-b", "errored")
        .with_message("timed out while inspecting logs");
    let collaboration =
        CollaborationEvent::new("wait", RunActivityStatus::Failed { message: None })
            .with_sender("thread-main")
            .with_receivers(["agent-b"])
            .with_prompt("Inspect the failing logs")
            .with_participant(participant.clone());
    let activity = RunActivity::new(RunActivityKind::Collaboration(collaboration.clone()));

    assert_eq!(collaboration.operation(), "wait");
    assert_eq!(
        collaboration.status(),
        &RunActivityStatus::Failed { message: None }
    );
    assert_eq!(collaboration.sender(), Some("thread-main"));
    assert_eq!(collaboration.receivers(), ["agent-b"]);
    assert_eq!(collaboration.prompt(), Some("Inspect the failing logs"));
    assert_eq!(
        collaboration.participants(),
        std::slice::from_ref(&participant)
    );
    assert_eq!(participant.id(), "agent-b");
    assert_eq!(participant.status(), "errored");
    assert_eq!(
        participant.message(),
        Some("timed out while inspecting logs")
    );
    assert_eq!(
        activity.kind(),
        &RunActivityKind::Collaboration(collaboration)
    );
}
