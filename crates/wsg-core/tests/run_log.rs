use std::time::Duration;

use wsg_core::{
    AgentRuntime, CollaborationEvent, CollaborationParticipant, RunActivity, RunActivityKind,
    RunActivityStatus, RunConclusion, RunCost, RunLogEvent, RunLogParser, RunResult, RunUsage,
};

#[test]
fn claude_session_initialization_becomes_provider_neutral_activity() {
    let mut parser = RunLogParser::new(AgentRuntime::Claude);

    let events = parser
        .parse_line(
            r#"{"type":"system","subtype":"init","session_id":"e046ef61-7c94-48cc-9852-c3e98adae73a"}"#,
        )
        .expect("Claude session initialization should parse");

    assert_eq!(
        events,
        [RunLogEvent::Activity(RunActivity::new(
            RunActivityKind::SessionStarted,
        ))]
    );
}

#[test]
fn claude_assistant_text_normalizes_message_and_usage() {
    let mut parser = RunLogParser::new(AgentRuntime::Claude);
    let usage = RunUsage::new(30_000, 4_000)
        .with_cached_input_tokens(10_000)
        .with_cache_write_input_tokens(5_000);

    let events = parser
        .parse_line(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Checking the failure"}],"usage":{"input_tokens":30000,"output_tokens":4000,"cache_read_input_tokens":10000,"cache_creation_input_tokens":5000}}}"#,
        )
        .expect("Claude assistant text should parse");

    assert_eq!(
        events,
        [RunLogEvent::Activity(
            RunActivity::new(RunActivityKind::Message {
                text: "Checking the failure".to_owned(),
            })
            .with_usage(usage),
        )]
    );
}

#[test]
fn claude_tool_lifecycle_preserves_content_order_and_concise_detail() {
    let mut parser = RunLogParser::new(AgentRuntime::Claude);

    let started = parser
        .parse_line(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Running checks"},{"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"mise run check"}}]}}"#,
        )
        .expect("Claude tool start should parse");
    assert_eq!(
        started,
        [
            RunLogEvent::Activity(RunActivity::new(RunActivityKind::Message {
                text: "Running checks".to_owned(),
            })),
            RunLogEvent::Activity(RunActivity::new(RunActivityKind::Tool {
                name: "Bash".to_owned(),
                detail: Some("mise run check".to_owned()),
                status: RunActivityStatus::InProgress,
            })),
        ]
    );

    let completed = parser
        .parse_line(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tool-1","content":"checks passed"}]}}"#,
        )
        .expect("Claude tool completion should parse");
    assert_eq!(
        completed,
        [RunLogEvent::Activity(RunActivity::new(
            RunActivityKind::Tool {
                name: "Bash".to_owned(),
                detail: Some("mise run check".to_owned()),
                status: RunActivityStatus::Completed,
            },
        ))]
    );
}

#[test]
fn claude_legacy_tool_result_completes_the_correlated_tool() {
    let mut parser = RunLogParser::new(AgentRuntime::Claude);
    parser
        .parse_line(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"agent-1","name":"Agent","input":{"description":"Inspect logs"}}]}}"#,
        )
        .expect("Claude Agent tool start should parse");

    let events = parser
        .parse_line(
            r#"{"type":"tool","tool":{"type":"tool_result","name":"Agent","tool_use_id":"agent-1"}}"#,
        )
        .expect("legacy Claude tool result should parse");

    assert_eq!(
        events,
        [RunLogEvent::Activity(RunActivity::new(
            RunActivityKind::Tool {
                name: "Agent".to_owned(),
                detail: Some("Inspect logs".to_owned()),
                status: RunActivityStatus::Completed,
            },
        ))]
    );
}

#[test]
fn claude_failed_tool_result_preserves_provider_message() {
    let mut parser = RunLogParser::new(AgentRuntime::Claude);
    parser
        .parse_line(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tool-1","name":"Write","input":{"file_path":"src/lib.rs"}}]}}"#,
        )
        .expect("Claude tool start should parse");

    let events = parser
        .parse_line(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tool-1","is_error":true,"content":"approval denied"}]}}"#,
        )
        .expect("Claude failed tool result should parse");

    assert_eq!(
        events,
        [RunLogEvent::Activity(RunActivity::new(
            RunActivityKind::Tool {
                name: "Write".to_owned(),
                detail: Some("src/lib.rs".to_owned()),
                status: RunActivityStatus::Failed {
                    message: Some("approval denied".to_owned()),
                },
            },
        ))]
    );
}

#[test]
fn claude_success_result_normalizes_completion_metrics() {
    let mut parser = RunLogParser::new(AgentRuntime::Claude);

    let events = parser
        .parse_line(
            r#"{"type":"result","subtype":"success","duration_ms":5000,"num_turns":3,"total_cost_usd":0.42,"is_error":false,"result":"All done"}"#,
        )
        .expect("Claude success result should parse");

    assert_eq!(
        events,
        [RunLogEvent::Result(
            RunResult::succeeded()
                .with_duration(Duration::from_secs(5))
                .with_turns(3)
                .with_cost(RunCost::from_micro_usd(420_000)),
        )]
    );
}

#[test]
fn claude_failure_result_uses_subtype_fallback_and_rounds_to_micro_usd() {
    let mut parser = RunLogParser::new(AgentRuntime::Claude);

    let events = parser
        .parse_line(
            r#"{"type":"result","subtype":"error_during_execution","duration_ms":1500,"num_turns":2,"total_cost_usd":0.0103125,"is_error":true,"result":""}"#,
        )
        .expect("Claude failure result should parse");

    assert_eq!(
        events,
        [RunLogEvent::Result(
            RunResult::failed("error_during_execution")
                .with_duration(Duration::from_millis(1_500))
                .with_turns(2)
                .with_cost(RunCost::from_micro_usd(10_313)),
        )]
    );
}

#[test]
fn claude_parser_ignores_unknown_events_but_rejects_malformed_json() {
    let mut parser = RunLogParser::new(AgentRuntime::Claude);

    let unknown = parser
        .parse_line(r#"{"type":"rate_limit_event","rate_limit_info":{}}"#)
        .expect("unknown Claude events should remain forward compatible");
    assert!(unknown.is_empty());

    assert!(
        parser
            .parse_line(r#"{"type":"assistant","message":{"#)
            .is_err(),
        "truncated Claude JSON should report a typed parse failure",
    );
}

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
