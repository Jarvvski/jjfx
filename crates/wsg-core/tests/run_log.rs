use std::fs;
use std::io::ErrorKind;
use std::time::Duration;

use tempfile::tempdir;
use wsg_core::{
    AgentRuntime, CollaborationEvent, CollaborationParticipant, RunActivity, RunActivityKind,
    RunActivityStatus, RunConclusion, RunCost, RunLog, RunLogError, RunLogEvent, RunLogParseError,
    RunLogParser, RunResult, RunUsage,
};

#[test]
fn current_activity_returns_the_latest_provider_neutral_activity() {
    let directory = tempdir().expect("temporary directory should be created");
    let path = directory.path().join("worker.log");
    fs::write(
        &path,
        concat!(
            r#"{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"Implemented the fix"}}"#,
            "\n",
            r#"{"type":"item.updated","item":{"id":"reasoning-1","type":"reasoning","text":"Checking the result"}}"#,
            "\n",
        ),
    )
    .expect("Run log should be written");
    let log = RunLog::new(&path, AgentRuntime::Codex);

    let activity = log
        .current_activity()
        .expect("current activity should be read");

    assert_eq!(
        activity,
        Some(RunActivity::new(RunActivityKind::Reasoning {
            text: "Checking the result".to_owned(),
        }))
    );
}

#[test]
fn current_activity_does_not_scan_before_the_final_64_kib() {
    let directory = tempdir().expect("temporary directory should be created");
    let path = directory.path().join("worker.log");
    let mut contents = concat!(
        r#"{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"Too old"}}"#,
        "\n",
    )
    .to_owned();
    contents.push_str(&"not structured log data\n".repeat(3_000));
    assert!(contents.len() > 65_536);
    fs::write(&path, contents).expect("Run log should be written");
    let log = RunLog::new(&path, AgentRuntime::Codex);

    let activity = log
        .current_activity()
        .expect("current activity should be read");

    assert_eq!(activity, None);
}

#[test]
fn current_activity_ignores_partial_records_at_tail_boundaries() {
    let directory = tempdir().expect("temporary directory should be created");
    let path = directory.path().join("worker.log");
    let oversized_record = format!(
        r#"{{"type":"item.completed","item":{{"id":"old","type":"agent_message","text":"{}"}}}}"#,
        "x".repeat(70_000)
    );
    let contents = format!(
        "{oversized_record}\n{}\n{}",
        r#"{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"Still working"}}"#,
        r#"{"type":"item.updated","item":{"id":"partial""#,
    );
    fs::write(&path, contents).expect("Run log should be written");
    let log = RunLog::new(&path, AgentRuntime::Codex);

    let activity = log
        .current_activity()
        .expect("current activity should be read");

    assert_eq!(
        activity,
        Some(RunActivity::new(RunActivityKind::Message {
            text: "Still working".to_owned(),
        }))
    );
}

#[test]
fn current_activity_ignores_newer_blank_and_malformed_lines() {
    let directory = tempdir().expect("temporary directory should be created");
    let path = directory.path().join("worker.log");
    fs::write(
        &path,
        concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"mise run check"}}]}}"#,
            "\n\n",
            "warning: could not update PATH aliases\n",
            r#"{"type":"assistant""#,
        ),
    )
    .expect("Run log should be written");
    let log = RunLog::new(&path, AgentRuntime::Claude);

    let activity = log
        .current_activity()
        .expect("current activity should be read");

    assert_eq!(
        activity,
        Some(RunActivity::new(RunActivityKind::Tool {
            name: "Bash".to_owned(),
            detail: Some("mise run check".to_owned()),
            status: RunActivityStatus::InProgress,
        }))
    );
}

#[test]
fn current_activity_omits_a_claude_completion_whose_start_is_outside_the_tail() {
    let directory = tempdir().expect("temporary directory should be created");
    let path = directory.path().join("worker.log");
    let mut contents = concat!(
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"mise run check"}}]}}"#,
        "\n",
    )
    .to_owned();
    contents.push_str(&"stderr noise\n".repeat(6_000));
    contents.push_str(concat!(
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tool-1","content":"complete"}]}}"#,
        "\n",
    ));
    assert!(contents.len() > 65_536);
    fs::write(&path, contents).expect("Run log should be written");
    let log = RunLog::new(&path, AgentRuntime::Claude);

    let activity = log
        .current_activity()
        .expect("current activity should be read");

    assert_eq!(activity, None);
}

#[test]
fn final_result_scans_the_entire_log() {
    let directory = tempdir().expect("temporary directory should be created");
    let path = directory.path().join("worker.log");
    let mut contents = concat!(
        r#"{"type":"result","subtype":"success","is_error":false,"result":"All done","duration_ms":2500,"num_turns":3,"total_cost_usd":0.125}"#,
        "\n",
    )
    .to_owned();
    contents.push_str(&"warning: trailing stderr\n".repeat(3_000));
    assert!(contents.len() > 65_536);
    fs::write(&path, contents).expect("Run log should be written");
    let log = RunLog::new(&path, AgentRuntime::Claude);

    let result = log.final_result().expect("final result should be read");

    assert_eq!(
        result,
        Some(
            RunResult::succeeded()
                .with_duration(Duration::from_millis(2_500))
                .with_turns(3)
                .with_cost(RunCost::from_micro_usd(125_000)),
        )
    );
}

#[test]
fn final_result_preserves_codex_usage() {
    let directory = tempdir().expect("temporary directory should be created");
    let path = directory.path().join("worker.log");
    fs::write(
        &path,
        concat!(
            r#"{"type":"thread.started","thread_id":"thread-1"}"#,
            "\n",
            r#"{"type":"turn.completed","usage":{"input_tokens":1200,"cached_input_tokens":300,"cache_write_input_tokens":50,"output_tokens":400,"reasoning_output_tokens":200}}"#,
            "\n",
        ),
    )
    .expect("Run log should be written");
    let log = RunLog::new(&path, AgentRuntime::Codex);

    let result = log.final_result().expect("final result should be read");

    assert_eq!(
        result,
        Some(
            RunResult::succeeded().with_usage(
                RunUsage::new(1_200, 400)
                    .with_cached_input_tokens(300)
                    .with_cache_write_input_tokens(50)
                    .with_reasoning_output_tokens(200),
            )
        )
    );
}

#[test]
fn final_result_returns_the_latest_terminal_result() {
    let directory = tempdir().expect("temporary directory should be created");
    let path = directory.path().join("worker.log");
    fs::write(
        &path,
        concat!(
            r#"{"type":"result","subtype":"success","is_error":false,"result":"First attempt"}"#,
            "\n",
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"Follow-up failed"}"#,
            "\n",
        ),
    )
    .expect("Run log should be written");
    let log = RunLog::new(&path, AgentRuntime::Claude);

    let result = log.final_result().expect("final result should be read");

    assert_eq!(result, Some(RunResult::failed("Follow-up failed")));
}

#[test]
fn final_result_is_absent_when_no_terminal_event_is_valid() {
    let directory = tempdir().expect("temporary directory should be created");
    let path = directory.path().join("worker.log");
    fs::write(
        &path,
        concat!(
            r#"{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"Still working"}}"#,
            "\n",
            "not structured log data\n",
            r#"{"type":"turn.completed""#,
        ),
    )
    .expect("Run log should be written");
    let log = RunLog::new(&path, AgentRuntime::Codex);

    let result = log.final_result().expect("Run log should be readable");

    assert_eq!(result, None);
}

#[test]
fn unavailable_log_returns_a_path_aware_io_error() {
    let directory = tempdir().expect("temporary directory should be created");
    let path = directory.path().join("missing.log");
    let log = RunLog::new(&path, AgentRuntime::Claude);

    let error = log
        .current_activity()
        .expect_err("a missing Run log should fail");

    match error {
        RunLogError::Io {
            path: error_path,
            source,
        } => {
            assert_eq!(error_path, path);
            assert_eq!(source.kind(), ErrorKind::NotFound);
        }
        RunLogError::Parse { .. } => panic!("expected an I/O error"),
    }
}

#[test]
fn semantic_result_failure_returns_a_path_aware_parse_error() {
    let directory = tempdir().expect("temporary directory should be created");
    let path = directory.path().join("worker.log");
    fs::write(
        &path,
        concat!(
            r#"{"type":"result","subtype":"success","is_error":false,"total_cost_usd":-0.01}"#,
            "\n",
        ),
    )
    .expect("Run log should be written");
    let log = RunLog::new(&path, AgentRuntime::Claude);

    let error = log
        .final_result()
        .expect_err("a negative Run cost should fail");

    match error {
        RunLogError::Parse {
            path: error_path,
            source:
                RunLogParseError::InvalidCost {
                    runtime: AgentRuntime::Claude,
                    value,
                },
        } => {
            assert_eq!(error_path, path);
            assert_eq!(value, "-0.01");
        }
        RunLogError::Parse { source, .. } => panic!("unexpected parse error: {source}"),
        RunLogError::Io { source, .. } => panic!("unexpected I/O error: {source}"),
    }
}

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
fn codex_thread_start_becomes_provider_neutral_session_activity() {
    let mut parser = RunLogParser::new(AgentRuntime::Codex);

    let events = parser
        .parse_line(r#"{"type":"thread.started","thread_id":"thread-1"}"#)
        .expect("Codex thread start should parse");

    assert_eq!(
        events,
        [RunLogEvent::Activity(RunActivity::new(
            RunActivityKind::SessionStarted,
        ))]
    );
}

#[test]
fn codex_narrative_items_become_provider_neutral_activity() {
    let mut parser = RunLogParser::new(AgentRuntime::Codex);

    let message = parser
        .parse_line(
            r#"{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"Implemented the fix"}}"#,
        )
        .expect("Codex agent message should parse");
    assert_eq!(
        message,
        [RunLogEvent::Activity(RunActivity::new(
            RunActivityKind::Message {
                text: "Implemented the fix".to_owned(),
            },
        ))]
    );

    let reasoning = parser
        .parse_line(
            r#"{"type":"item.updated","item":{"id":"reasoning-1","type":"reasoning","text":"Checking the failure"}}"#,
        )
        .expect("Codex reasoning should parse");
    assert_eq!(
        reasoning,
        [RunLogEvent::Activity(RunActivity::new(
            RunActivityKind::Reasoning {
                text: "Checking the failure".to_owned(),
            },
        ))]
    );
}

#[test]
fn codex_command_execution_preserves_status_and_failure_diagnostics() {
    let mut parser = RunLogParser::new(AgentRuntime::Codex);

    let started = parser
        .parse_line(
            r#"{"type":"item.started","item":{"id":"command-1","type":"command_execution","command":"mise run check","status":"in_progress"}}"#,
        )
        .expect("Codex command start should parse");
    assert_eq!(
        started,
        [RunLogEvent::Activity(RunActivity::new(
            RunActivityKind::Tool {
                name: "Command".to_owned(),
                detail: Some("mise run check".to_owned()),
                status: RunActivityStatus::InProgress,
            },
        ))]
    );

    let failed = parser
        .parse_line(
            r#"{"type":"item.completed","item":{"id":"command-1","type":"command_execution","command":"mise run check","aggregated_output":"tests failed","status":"failed"}}"#,
        )
        .expect("Codex command failure should parse");
    assert_eq!(
        failed,
        [RunLogEvent::Activity(RunActivity::new(
            RunActivityKind::Tool {
                name: "Command".to_owned(),
                detail: Some("mise run check".to_owned()),
                status: RunActivityStatus::Failed {
                    message: Some("tests failed".to_owned()),
                },
            },
        ))]
    );

    let declined = parser
        .parse_line(
            r#"{"type":"item.completed","item":{"id":"command-2","type":"command_execution","command":"rm file","status":"declined"}}"#,
        )
        .expect("Codex declined command should parse");
    assert_eq!(
        declined,
        [RunLogEvent::Activity(RunActivity::new(
            RunActivityKind::Tool {
                name: "Command".to_owned(),
                detail: Some("rm file".to_owned()),
                status: RunActivityStatus::Declined,
            },
        ))]
    );

    let failed_with_exit_code = parser
        .parse_line(
            r#"{"type":"item.completed","item":{"id":"command-3","type":"command_execution","command":"false","aggregated_output":"","exit_code":17,"status":"failed"}}"#,
        )
        .expect("Codex command exit code should parse");
    assert_eq!(
        failed_with_exit_code,
        [RunLogEvent::Activity(RunActivity::new(
            RunActivityKind::Tool {
                name: "Command".to_owned(),
                detail: Some("false".to_owned()),
                status: RunActivityStatus::Failed {
                    message: Some("exit code 17".to_owned()),
                },
            },
        ))]
    );
}

#[test]
fn codex_external_tools_preserve_targets_and_failure_diagnostics() {
    let mut parser = RunLogParser::new(AgentRuntime::Codex);

    let mcp = parser
        .parse_line(
            r#"{"type":"item.completed","item":{"id":"mcp-1","type":"mcp_tool_call","server":"linear","tool":"save_issue","status":"failed","error":{"message":"approval denied"}}}"#,
        )
        .expect("Codex MCP failure should parse");
    assert_eq!(
        mcp,
        [RunLogEvent::Activity(RunActivity::new(
            RunActivityKind::Tool {
                name: "linear.save_issue".to_owned(),
                detail: None,
                status: RunActivityStatus::Failed {
                    message: Some("approval denied".to_owned()),
                },
            },
        ))]
    );

    let search = parser
        .parse_line(
            r#"{"type":"item.completed","item":{"id":"search-1","type":"web_search","query":"Codex JSON events"}}"#,
        )
        .expect("Codex web search should parse");
    assert_eq!(
        search,
        [RunLogEvent::Activity(RunActivity::new(
            RunActivityKind::Tool {
                name: "WebSearch".to_owned(),
                detail: Some("Codex JSON events".to_owned()),
                status: RunActivityStatus::Completed,
            },
        ))]
    );
}

#[test]
fn codex_structured_items_preserve_files_plan_progress_and_warnings() {
    let mut parser = RunLogParser::new(AgentRuntime::Codex);

    let files = parser
        .parse_line(
            r#"{"type":"item.completed","item":{"id":"files-1","type":"file_change","changes":[{"path":"src/lib.rs","kind":"update"},{"path":"tests/lib.rs","kind":"add"}],"status":"completed"}}"#,
        )
        .expect("Codex file changes should parse");
    assert_eq!(
        files,
        [RunLogEvent::Activity(RunActivity::new(
            RunActivityKind::FileChanges {
                paths: vec!["src/lib.rs".to_owned(), "tests/lib.rs".to_owned()],
            },
        ))]
    );

    let plan = parser
        .parse_line(
            r#"{"type":"item.updated","item":{"id":"plan-1","type":"todo_list","items":[{"text":"Inspect","completed":true},{"text":"Fix","completed":false}]}}"#,
        )
        .expect("Codex plan update should parse");
    assert_eq!(
        plan,
        [RunLogEvent::Activity(RunActivity::new(
            RunActivityKind::Plan {
                completed: 1,
                total: 2,
            },
        ))]
    );

    let warning = parser
        .parse_line(
            r#"{"type":"item.completed","item":{"id":"warning-1","type":"error","message":"PATH alias unavailable"}}"#,
        )
        .expect("Codex warning item should parse");
    assert_eq!(
        warning,
        [RunLogEvent::Activity(RunActivity::new(
            RunActivityKind::Warning {
                message: "PATH alias unavailable".to_owned(),
            },
        ))]
    );
}

#[test]
fn codex_collaboration_preserves_context_and_deterministic_participants() {
    let mut parser = RunLogParser::new(AgentRuntime::Codex);

    let events = parser
        .parse_line(
            r#"{"type":"item.completed","item":{"id":"wait-1","type":"collab_tool_call","tool":"wait","sender_thread_id":"thread-main","receiver_thread_ids":["agent-b","agent-a"],"prompt":"Inspect\n  failing logs","agents_states":{"agent-b":{"status":"errored","message":"timed\n out"},"agent-a":{"status":"completed","message":"found issue"}},"status":"failed","error":{"message":"collaboration timed out"}}}"#,
        )
        .expect("Codex collaboration should parse");

    let expected = CollaborationEvent::new(
        "wait",
        RunActivityStatus::Failed {
            message: Some("collaboration timed out".to_owned()),
        },
    )
    .with_sender("thread-main")
    .with_receivers(["agent-b", "agent-a"])
    .with_prompt("Inspect failing logs")
    .with_participant(
        CollaborationParticipant::new("agent-a", "completed").with_message("found issue"),
    )
    .with_participant(
        CollaborationParticipant::new("agent-b", "errored").with_message("timed out"),
    );
    assert_eq!(
        events,
        [RunLogEvent::Activity(RunActivity::new(
            RunActivityKind::Collaboration(expected),
        ))]
    );
}

#[test]
fn codex_completed_turn_normalizes_all_usage_counters() {
    let mut parser = RunLogParser::new(AgentRuntime::Codex);

    let events = parser
        .parse_line(
            r#"{"type":"turn.completed","usage":{"input_tokens":1200,"cached_input_tokens":400,"cache_write_input_tokens":50,"output_tokens":300,"reasoning_output_tokens":75}}"#,
        )
        .expect("Codex completed turn should parse");

    assert_eq!(
        events,
        [RunLogEvent::Result(
            RunResult::succeeded().with_usage(
                RunUsage::new(1_200, 300)
                    .with_cached_input_tokens(400)
                    .with_cache_write_input_tokens(50)
                    .with_reasoning_output_tokens(75),
            )
        )]
    );
}

#[test]
fn codex_terminal_errors_preserve_provider_failure_details() {
    let mut parser = RunLogParser::new(AgentRuntime::Codex);

    let turn_failed = parser
        .parse_line(
            r#"{"type":"turn.failed","error":{"message":"sandbox denied"},"message":"less specific"}"#,
        )
        .expect("Codex turn failure should parse");
    assert_eq!(
        turn_failed,
        [RunLogEvent::Result(RunResult::failed("sandbox denied"))]
    );

    let fatal = parser
        .parse_line(r#"{"type":"error","message":"connection lost"}"#)
        .expect("Codex fatal error should parse");
    assert_eq!(
        fatal,
        [RunLogEvent::Result(RunResult::failed("connection lost"))]
    );
}

#[test]
fn codex_parser_ignores_unknown_events_but_rejects_malformed_json() {
    let mut parser = RunLogParser::new(AgentRuntime::Codex);

    let unknown_event = parser
        .parse_line(r#"{"type":"thread.metadata","version":2}"#)
        .expect("unknown Codex events should remain forward compatible");
    assert!(unknown_event.is_empty());

    let unknown_item = parser
        .parse_line(
            r#"{"type":"item.completed","item":{"id":"future-1","type":"future_item","detail":"preserved by Codex"}}"#,
        )
        .expect("unknown Codex items should remain forward compatible");
    assert!(unknown_item.is_empty());

    let error = parser
        .parse_line(r#"{"type":"item.completed","item":{"#)
        .expect_err("truncated Codex JSON should fail");
    assert!(matches!(
        error,
        RunLogParseError::InvalidEvent {
            runtime: AgentRuntime::Codex,
            ..
        }
    ));
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
