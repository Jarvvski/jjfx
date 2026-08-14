use std::collections::VecDeque;
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use wsg_core::{
    AgentRuntime, AgentRuntimeQuery, Blocker, ParentTicket, PiDiscoveryHelper, ReadyTicketFilter,
    RepositoryIdentity, Ticket, TicketDiscovery, TicketId, TicketQuery, TicketQueryError,
    TicketQueryErrorKind,
    TicketQueryRequest, TicketStatus, TicketTitle,
};

const HELPER_RUNTIME: &str = "WSG_TICKET_QUERY_HELPER_RUNTIME";
const HELPER_WORKSPACE: &str = "WSG_TICKET_QUERY_HELPER_WORKSPACE";
const HELPER_RESULT: &str = "WSG_TICKET_QUERY_HELPER_RESULT";
const HELPER_ARGS: &str = "WSG_TICKET_QUERY_HELPER_ARGS";

struct StubQuery {
    responses: Mutex<VecDeque<Result<String, TicketQueryError>>>,
}

impl StubQuery {
    fn returning(response: &str) -> Self {
        Self::responding([Ok(response.to_owned())])
    }

    fn responding(responses: impl IntoIterator<Item = Result<String, TicketQueryError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
        }
    }
}

impl TicketQuery for StubQuery {
    fn query(&self, _request: &TicketQueryRequest) -> Result<String, TicketQueryError> {
        self.responses
            .lock()
            .expect("query responses")
            .pop_front()
            .expect("a configured query response")
    }
}

struct RecordingQuery {
    request: Arc<Mutex<Option<TicketQueryRequest>>>,
}

impl TicketQuery for RecordingQuery {
    fn query(&self, request: &TicketQueryRequest) -> Result<String, TicketQueryError> {
        self.request
            .lock()
            .expect("recorded request")
            .replace(request.clone());
        Ok(r#"{"tickets":[]}"#.to_owned())
    }
}

#[test]
fn ready_ticket_discovery_sends_a_typed_request_to_the_query_adapter() {
    let filter = ReadyTicketFilter::new(
        "ready-for-agent",
        TicketStatus::parse("Todo").expect("expected status"),
    )
    .expect("Ready Ticket filter");
    let recorded = Arc::new(Mutex::new(None));
    let discovery = TicketDiscovery::new(RecordingQuery {
        request: Arc::clone(&recorded),
    });

    discovery
        .ready_tickets(&filter)
        .expect("Ready Ticket discovery");

    assert_eq!(
        recorded.lock().expect("recorded request").as_ref(),
        Some(&TicketQueryRequest::ReadyTickets {
            filter: filter.clone(),
        })
    );
}

#[test]
fn pi_discovery_preserves_typed_setup_failures_for_callers() {
    let discovery = TicketDiscovery::new(AgentRuntimeQuery::new(AgentRuntime::Pi, "."));
    let filter = ReadyTicketFilter::new(
        "ready-for-agent",
        TicketStatus::parse("Todo").expect("expected status"),
    )
    .expect("Ready Ticket filter");

    let error = discovery
        .ready_tickets(&filter)
        .expect_err("missing Pi helper configuration should fail");

    assert_eq!(error.query_kind(), Some(TicketQueryErrorKind::Setup));
    assert!(error.to_string().contains("JJFX_PI_LINEAR_HELPER"));
}

#[test]
fn pi_discovery_without_a_helper_reports_typed_setup_guidance() {
    let query = AgentRuntimeQuery::new(AgentRuntime::Pi, ".");
    let request = TicketQueryRequest::ReadyTickets {
        filter: ReadyTicketFilter::new(
            "ready-for-agent",
            TicketStatus::parse("Todo").expect("expected status"),
        )
        .expect("Ready Ticket filter"),
    };

    let error = query
        .query(&request)
        .expect_err("missing Pi helper configuration should fail");

    assert_eq!(error.kind(), TicketQueryErrorKind::Setup);
    assert!(!error.is_retryable());
    assert!(error.to_string().contains("JJFX_PI_LINEAR_HELPER"));
}

#[test]
fn pi_discovery_times_out_and_reaps_the_helper_process() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let helper = temporary_directory.path().join("pi-linear-helper");
    write_executable(&helper, "#!/bin/sh\nsleep 5\n");
    let query = AgentRuntimeQuery::new(AgentRuntime::Pi, temporary_directory.path()).with_pi_helper(
        PiDiscoveryHelper::new(&helper).with_timeout(Duration::from_millis(50)),
    );
    let request = TicketQueryRequest::ReadyTickets {
        filter: ReadyTicketFilter::new(
            "ready-for-agent",
            TicketStatus::parse("Todo").expect("expected status"),
        )
        .expect("Ready Ticket filter"),
    };
    let started = Instant::now();

    let error = query
        .query(&request)
        .expect_err("slow Pi helper should time out");

    assert_eq!(error.kind(), TicketQueryErrorKind::Timeout);
    assert!(error.is_retryable());
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn pi_helper_malformed_ticket_payload_uses_the_existing_single_retry() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let helper = temporary_directory.path().join("pi-linear-helper");
    let attempts = temporary_directory.path().join("attempts");
    write_executable(
        &helper,
        &format!(
            "#!/bin/sh\ncat >/dev/null\nif [ ! -f '{}' ]; then printf '1' > '{}'; printf '%s\\n' '{{\"version\":1,\"result\":{{\"tickets\":\"invalid\"}}}}'; else printf '2' > '{}'; printf '%s\\n' '{{\"version\":1,\"result\":{{\"tickets\":[]}}}}'; fi\n",
            attempts.display(),
            attempts.display(),
            attempts.display(),
        ),
    );
    let discovery = TicketDiscovery::new(
        AgentRuntimeQuery::new(AgentRuntime::Pi, temporary_directory.path())
            .with_pi_helper(PiDiscoveryHelper::new(&helper)),
    );
    let filter = ReadyTicketFilter::new(
        "ready-for-agent",
        TicketStatus::parse("Todo").expect("expected status"),
    )
    .expect("Ready Ticket filter");

    let tickets = discovery
        .ready_tickets(&filter)
        .expect("malformed Pi payload should recover");

    assert!(tickets.tickets().is_empty());
    assert_eq!(fs::read_to_string(attempts).expect("attempt count"), "2");
}

#[test]
fn pi_helper_transient_errors_use_the_existing_single_retry() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let helper = temporary_directory.path().join("pi-linear-helper");
    let attempts = temporary_directory.path().join("attempts");
    write_executable(
        &helper,
        &format!(
            "#!/bin/sh\ncat >/dev/null\nif [ ! -f '{}' ]; then printf '1' > '{}'; printf '%s\\n' '{{\"version\":1,\"error\":{{\"kind\":\"transient\",\"message\":\"Linear temporarily unavailable\"}}}}'; else printf '2' > '{}'; printf '%s\\n' '{{\"version\":1,\"result\":{{\"tickets\":[{{\"id\":\"AMBA-42\",\"title\":\"Recovered\",\"status\":\"Todo\",\"labels\":[\"ready-for-agent\"]}}]}}}}'; fi\n",
            attempts.display(),
            attempts.display(),
            attempts.display(),
        ),
    );
    let discovery = TicketDiscovery::new(
        AgentRuntimeQuery::new(AgentRuntime::Pi, temporary_directory.path())
            .with_pi_helper(PiDiscoveryHelper::new(&helper)),
    );
    let filter = ReadyTicketFilter::new(
        "ready-for-agent",
        TicketStatus::parse("Todo").expect("expected status"),
    )
    .expect("Ready Ticket filter");

    let tickets = discovery
        .ready_tickets(&filter)
        .expect("transient Pi failure should recover");

    assert_eq!(tickets.tickets()[0].title().as_str(), "Recovered");
    assert_eq!(fs::read_to_string(attempts).expect("attempt count"), "2");
}

#[test]
fn pi_helper_malformed_envelopes_are_typed_protocol_errors() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let helper = temporary_directory.path().join("pi-linear-helper");
    write_executable(
        &helper,
        "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' 'not-json credential=secret-token'\n",
    );
    let query = AgentRuntimeQuery::new(AgentRuntime::Pi, temporary_directory.path())
        .with_pi_helper(PiDiscoveryHelper::new(&helper));
    let request = TicketQueryRequest::ReadyTickets {
        filter: ReadyTicketFilter::new(
            "ready-for-agent",
            TicketStatus::parse("Todo").expect("expected status"),
        )
        .expect("Ready Ticket filter"),
    };

    let error = query
        .query(&request)
        .expect_err("malformed helper envelope should fail");

    assert_eq!(error.kind(), TicketQueryErrorKind::Protocol);
    assert!(!error.is_retryable());
    assert!(!error.to_string().contains("secret-token"));
}

#[test]
fn pi_helper_unsupported_capability_errors_are_typed() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let helper = temporary_directory.path().join("pi-linear-helper");
    write_executable(
        &helper,
        "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"version\":1,\"error\":{\"kind\":\"unsupported\",\"message\":\"list_issues is unavailable\"}}'\n",
    );
    let query = AgentRuntimeQuery::new(AgentRuntime::Pi, temporary_directory.path())
        .with_pi_helper(PiDiscoveryHelper::new(&helper));
    let request = TicketQueryRequest::ReadyTickets {
        filter: ReadyTicketFilter::new(
            "ready-for-agent",
            TicketStatus::parse("Todo").expect("expected status"),
        )
        .expect("Ready Ticket filter"),
    };

    let error = query
        .query(&request)
        .expect_err("unsupported helper capability should fail");

    assert_eq!(error.kind(), TicketQueryErrorKind::Unsupported);
    assert!(!error.is_retryable());
    assert!(error.to_string().contains("list_issues is unavailable"));
}

#[test]
fn pi_helper_authentication_errors_are_typed_without_leaking_stderr() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let helper = temporary_directory.path().join("pi-linear-helper");
    write_executable(
        &helper,
        "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' 'credential=secret-token' >&2\nprintf '%s\\n' '{\"version\":1,\"error\":{\"kind\":\"authentication\",\"message\":\"configure Linear credentials\"},\"future\":true}'\n",
    );
    let query = AgentRuntimeQuery::new(AgentRuntime::Pi, temporary_directory.path())
        .with_pi_helper(PiDiscoveryHelper::new(&helper));
    let request = TicketQueryRequest::ReadyTickets {
        filter: ReadyTicketFilter::new(
            "ready-for-agent",
            TicketStatus::parse("Todo").expect("expected status"),
        )
        .expect("Ready Ticket filter"),
    };

    let error = query
        .query(&request)
        .expect_err("authentication response should fail");

    assert_eq!(error.kind(), TicketQueryErrorKind::Authentication);
    assert!(!error.is_retryable());
    assert!(error.to_string().contains("configure Linear credentials"));
    assert!(!error.to_string().contains("secret-token"));
}

#[test]
fn pi_dependency_discovery_uses_the_typed_helper_request() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let helper = temporary_directory.path().join("pi-linear-helper");
    let request = temporary_directory.path().join("request.json");
    write_executable(
        &helper,
        &format!(
            "#!/bin/sh\ncat > '{}'\nprintf '%s\\n' '{{\"version\":1,\"result\":{{\"sub_issues\":[{{\"id\":\"AMBA-41\",\"title\":\"Foundation\",\"status\":\"Todo\",\"blocked_by\":[],\"cross_repo\":false,\"future\":true}}]}},\"future\":true}}'\n",
            request.display(),
        ),
    );
    let discovery = TicketDiscovery::new(
        AgentRuntimeQuery::new(AgentRuntime::Pi, temporary_directory.path())
            .with_pi_helper(PiDiscoveryHelper::new(&helper)),
    );
    let parent = ParentTicket::new(TicketId::parse("AMBA-40").expect("parent Ticket ID"));
    let repository = RepositoryIdentity::parse("owner/repo").expect("repository identity");

    let graph = discovery
        .dependency_graph(&parent, &repository)
        .expect("Pi dependency discovery");

    assert!(
        graph
            .sub_issue(&TicketId::parse("AMBA-41").expect("child Ticket ID"))
            .is_some()
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            &fs::read_to_string(request).expect("captured helper request")
        )
        .expect("valid request JSON"),
        serde_json::json!({
            "version": 1,
            "operation": "dependency_graph",
            "parent": "AMBA-40",
            "repository": "owner/repo",
        }),
    );
}

#[test]
fn pi_helper_nonzero_exit_is_retryable_without_leaking_stderr() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let helper = temporary_directory.path().join("pi-linear-helper");
    write_executable(
        &helper,
        "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' 'credential=secret-token' >&2\nexit 17\n",
    );
    let query = AgentRuntimeQuery::new(AgentRuntime::Pi, temporary_directory.path())
        .with_pi_helper(PiDiscoveryHelper::new(&helper));
    let request = TicketQueryRequest::ReadyTickets {
        filter: ReadyTicketFilter::new(
            "ready-for-agent",
            TicketStatus::parse("Todo").expect("expected status"),
        )
        .expect("Ready Ticket filter"),
    };

    let error = query
        .query(&request)
        .expect_err("non-zero helper exit should fail");

    assert_eq!(error.kind(), TicketQueryErrorKind::Transport);
    assert!(error.is_retryable());
    assert!(!error.to_string().contains("secret-token"));
}

#[test]
fn pi_ready_ticket_discovery_uses_the_configured_helper_protocol() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let workspace = temporary_directory.path().join("workspace");
    let helper = temporary_directory.path().join("pi-linear-helper");
    let request = temporary_directory.path().join("request.json");
    let working_directory = temporary_directory.path().join("working-directory");
    fs::create_dir(&workspace).expect("query workspace");
    write_executable(
        &helper,
        &format!(
            "#!/bin/sh\npwd > '{}'\ncat > '{}'\nprintf '%s\\n' '{{\"version\":1,\"result\":{{\"tickets\":[{{\"id\":\"AMBA-42\",\"title\":\"Pi result\",\"status\":\"Todo\",\"labels\":[\"ready-for-agent\"],\"future\":true}}]}},\"future\":true}}'\n",
            working_directory.display(),
            request.display(),
        ),
    );
    let query = AgentRuntimeQuery::new(AgentRuntime::Pi, &workspace)
        .with_pi_helper(PiDiscoveryHelper::new(&helper));
    let discovery = TicketDiscovery::new(query);
    let filter = ReadyTicketFilter::new(
        "ready-for-agent",
        TicketStatus::parse("Todo").expect("expected status"),
    )
    .expect("Ready Ticket filter");

    let tickets = discovery
        .ready_tickets(&filter)
        .expect("Pi Ready Ticket discovery");

    assert_eq!(tickets.tickets()[0].title().as_str(), "Pi result");
    assert_eq!(
        fs::canonicalize(
            fs::read_to_string(working_directory)
                .expect("captured working directory")
                .trim()
        )
        .expect("canonical captured working directory"),
        fs::canonicalize(&workspace).expect("canonical query workspace"),
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            &fs::read_to_string(request).expect("captured helper request")
        )
        .expect("valid request JSON"),
        serde_json::json!({
            "version": 1,
            "operation": "ready_tickets",
            "label": "ready-for-agent",
            "status": "Todo",
        }),
    );
}

#[test]
fn ready_tickets_are_discovered_through_either_configured_agent_runtime() {
    for runtime in [AgentRuntime::Claude, AgentRuntime::Codex] {
        let temporary_directory = TempDir::new().expect("temporary directory");
        let bin = temporary_directory.path().join("bin");
        let workspace = temporary_directory.path().join("workspace");
        let result = temporary_directory.path().join("result");
        let captured_args = temporary_directory.path().join("args");
        fs::create_dir(&bin).expect("runtime directory");
        fs::create_dir(&workspace).expect("query workspace");
        let response = if runtime == AgentRuntime::Claude {
            r#"#!/bin/sh
printf '%s\n' "$@" > "$WSG_TICKET_QUERY_HELPER_ARGS"
printf '%s\n' '{"result":"{\"tickets\":[{\"id\":\"AMBA-42\",\"title\":\"Claude result\",\"status\":\"Todo\",\"labels\":[\"ready-for-agent\"]}]}"}'
"#
        } else {
            r#"#!/bin/sh
printf '%s\n' "$@" > "$WSG_TICKET_QUERY_HELPER_ARGS"
printf '%s\n' '{"type":"thread.started","thread_id":"thread-42"}'
printf '%s\n' '{"type":"item.completed","item":{"id":"item-1","type":"agent_message","text":"{\"tickets\":[{\"id\":\"AMBA-42\",\"title\":\"Codex result\",\"status\":\"Todo\",\"labels\":[\"ready-for-agent\"]}]}"}}'
printf '%s\n' '{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5}}'
"#
        };
        write_executable(&bin.join(runtime.as_str()), response);
        let path = env::join_paths([bin.as_os_str()]).expect("runtime PATH");

        let output = Command::new(env::current_exe().expect("test executable"))
            .args(["--exact", "agent_runtime_query_helper", "--ignored"])
            .env("PATH", path)
            .env(HELPER_RUNTIME, runtime.as_str())
            .env(HELPER_WORKSPACE, &workspace)
            .env(HELPER_RESULT, &result)
            .env(HELPER_ARGS, &captured_args)
            .stdin(Stdio::null())
            .output()
            .expect("query helper should run");

        assert!(
            output.status.success(),
            "{runtime} helper failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read_to_string(&result).expect("query result"),
            format!("{runtime}:AMBA-42")
        );
        let args = fs::read_to_string(captured_args).expect("captured query arguments");
        if runtime == AgentRuntime::Claude {
            assert!(!args.contains("--model\n"));
            assert!(args.contains("--no-session-persistence\n"));
            assert!(args.contains("--allowedTools=mcp__claude_ai_Linear__list_issues"));
        } else {
            assert!(args.contains("--sandbox\nread-only\n"));
            assert!(args.contains("--ephemeral\n"));
        }
        assert!(!args.contains("multi_agent"));
        assert!(!args.contains("forward-subagent"));
    }
}

#[test]
#[ignore]
fn agent_runtime_query_helper() {
    let runtime = match env::var(HELPER_RUNTIME).expect("runtime").as_str() {
        "claude" => AgentRuntime::Claude,
        "codex" => AgentRuntime::Codex,
        value => panic!("unexpected runtime {value}"),
    };
    let query = AgentRuntimeQuery::new(
        runtime,
        env::var_os(HELPER_WORKSPACE).expect("query workspace"),
    );
    let discovery = TicketDiscovery::new(query);
    let filter = ReadyTicketFilter::new(
        "ready-for-agent",
        TicketStatus::parse("Todo").expect("expected status"),
    )
    .expect("Ready Ticket filter");
    let tickets = discovery
        .ready_tickets(&filter)
        .expect("Ready Ticket discovery");
    fs::write(
        env::var_os(HELPER_RESULT).expect("result path"),
        format!("{runtime}:{}", tickets.tickets()[0].id()),
    )
    .expect("write result");
}

#[test]
fn dependency_graph_retries_one_malformed_response() {
    let discovery = TicketDiscovery::new(StubQuery::responding([
        Ok("not JSON".to_owned()),
        Ok(r#"{"sub_issues":[]}"#.to_owned()),
    ]));
    let parent = ParentTicket::new(TicketId::parse("AMBA-40").expect("Parent Ticket ID"));
    let repository = RepositoryIdentity::parse("owner/repo").expect("Repository identity");

    let graph = discovery
        .dependency_graph(&parent, &repository)
        .expect("the retry should recover");

    assert!(graph.sub_issues().is_empty());
}

#[test]
fn permanent_query_failure_is_not_retried() {
    let discovery = TicketDiscovery::new(StubQuery::responding([Err(
        TicketQueryError::permanent("runtime executable missing"),
    )]));
    let filter = ReadyTicketFilter::new(
        "ready-for-agent",
        TicketStatus::parse("Todo").expect("expected workflow status"),
    )
    .expect("Ready Ticket filter");

    let error = discovery
        .ready_tickets(&filter)
        .expect_err("permanent failure should surface immediately");

    assert_eq!(
        error.to_string(),
        "Ticket discovery query failed: query failed: runtime executable missing"
    );
}

#[test]
fn persistent_discovery_failure_reports_both_attempts() {
    let discovery = TicketDiscovery::new(StubQuery::responding([
        Err(TicketQueryError::transient("network unavailable")),
        Ok("still not JSON".to_owned()),
    ]));
    let filter = ReadyTicketFilter::new(
        "ready-for-agent",
        TicketStatus::parse("Todo").expect("expected workflow status"),
    )
    .expect("Ready Ticket filter");

    let error = discovery
        .ready_tickets(&filter)
        .expect_err("both failed attempts should surface");
    let message = error.to_string();

    assert!(message.contains("first attempt query failed: network unavailable"));
    assert!(message.contains("second attempt response was malformed"));
}

#[test]
fn ready_ticket_discovery_retries_one_transient_query_failure() {
    let discovery = TicketDiscovery::new(StubQuery::responding([
        Err(TicketQueryError::transient("Linear MCP unavailable")),
        Ok(r#"{"tickets":[{"id":"AMBA-42","title":"Recovered","status":"Todo","labels":["ready-for-agent"]}]}"#.to_owned()),
    ]));
    let filter = ReadyTicketFilter::new(
        "ready-for-agent",
        TicketStatus::parse("Todo").expect("expected workflow status"),
    )
    .expect("Ready Ticket filter");

    let tickets = discovery
        .ready_tickets(&filter)
        .expect("the retry should recover");

    assert_eq!(tickets.tickets()[0].title().as_str(), "Recovered");
}

#[test]
fn dependency_graph_fails_when_every_reported_child_is_invalid() {
    let discovery = TicketDiscovery::new(StubQuery::returning(
        r#"{"sub_issues":[{"id":"AMBA-40","title":"Parent","status":"Todo","blocked_by":[],"cross_repo":false},{"id":"AMBA-41","title":"   ","status":"Todo","blocked_by":[],"cross_repo":false}]}"#,
    ));
    let parent = ParentTicket::new(TicketId::parse("AMBA-40").expect("Parent Ticket ID"));
    let repository = RepositoryIdentity::parse("owner/repo").expect("Repository identity");

    let error = discovery
        .dependency_graph(&parent, &repository)
        .expect_err("an unusable graph should fail");

    assert_eq!(
        error.to_string(),
        "Ticket query returned an unusable dependency graph (2 invalid entries)"
    );
}

#[test]
fn dependency_graph_excludes_unsafe_children_and_relationships() {
    let discovery = TicketDiscovery::new(StubQuery::returning(
        r#"{"sub_issues":[{"id":"AMBA-40","title":"Parent","status":"Todo","blocked_by":[],"cross_repo":false},{"id":"AMBA-41","title":"Foundation","status":"Todo","blocked_by":[],"cross_repo":false},{"id":"AMBA-41","title":"Duplicate","status":"Todo","blocked_by":[],"cross_repo":false},{"id":"AMBA-46","title":"Unsafe blockers","status":"Todo","blocked_by":["AMBA-46","AMBA-999"],"cross_repo":false},{"id":"AMBA-42","title":"   ","status":"Todo","blocked_by":[],"cross_repo":false},{"id":"AMBA-43","title":"Malformed status","status":"   ","blocked_by":[],"cross_repo":false},{"id":"AMBA-44","title":"Depends on excluded child","status":"Todo","blocked_by":["AMBA-41"],"cross_repo":false},{"id":"AMBA-45","title":"Safe child","status":"Todo","blocked_by":[],"cross_repo":false}]}"#,
    ));
    let parent = ParentTicket::new(TicketId::parse("AMBA-40").expect("Parent Ticket ID"));
    let repository = RepositoryIdentity::parse("owner/repo").expect("Repository identity");

    let graph = discovery
        .dependency_graph(&parent, &repository)
        .expect("partly valid graph should remain usable");

    let ids = graph
        .sub_issues()
        .keys()
        .map(TicketId::as_str)
        .collect::<Vec<_>>();
    assert_eq!(ids, ["AMBA-45"]);
    let reasons = graph
        .diagnostics()
        .iter()
        .map(|diagnostic| diagnostic.reason())
        .collect::<Vec<_>>()
        .join("\n");
    for expected in [
        "parent cannot be its own child",
        "duplicate child",
        "Ticket title cannot be blank",
        "Ticket status cannot be blank",
        "self-blocker",
        "unknown Blocker",
    ] {
        assert!(
            reasons.contains(expected),
            "missing {expected:?} in {reasons}"
        );
    }
}

#[test]
fn parent_ticket_discovery_returns_a_typed_dependency_graph() {
    let discovery = TicketDiscovery::new(StubQuery::returning(
        r#"{"sub_issues":[{"id":"AMBA-41","title":"Foundation","status":"Done","blocked_by":[],"cross_repo":false},{"id":"AMBA-42","title":"Ship typed discovery","status":"Todo","blocked_by":["AMBA-41"],"cross_repo":false}]}"#,
    ));
    let parent = ParentTicket::new(TicketId::parse("AMBA-40").expect("Parent Ticket ID"));
    let repository = RepositoryIdentity::parse("owner/repo").expect("Repository identity");

    let graph = discovery
        .dependency_graph(&parent, &repository)
        .expect("dependency graph should be discovered");

    assert_eq!(graph.parent(), &parent);
    assert_eq!(graph.sub_issues().len(), 2);
    let ticket = TicketId::parse("AMBA-42").expect("Ticket ID");
    let child = graph.sub_issue(&ticket).expect("discovered Sub-issue");
    assert_eq!(child.ticket().title().as_str(), "Ship typed discovery");
    assert_eq!(child.blockers().len(), 1);
    assert_eq!(child.blockers()[0].id().as_str(), "AMBA-41");
    assert!(!child.is_cross_repository());
    assert!(graph.diagnostics().is_empty());
}

#[test]
fn ready_ticket_discovery_excludes_tickets_without_the_configured_label() {
    let discovery = TicketDiscovery::new(StubQuery::returning(
        r#"{"tickets":[{"id":"AMBA-42","title":"Wrong label","status":"Todo","labels":["ready-for-human"]}]}"#,
    ));
    let filter = ReadyTicketFilter::new(
        "ready-for-agent",
        TicketStatus::parse("Todo").expect("expected workflow status"),
    )
    .expect("Ready Ticket filter");

    let tickets = discovery
        .ready_tickets(&filter)
        .expect("partly invalid Ready Ticket response");

    assert!(tickets.tickets().is_empty());
    assert_eq!(
        tickets.diagnostics()[0].reason(),
        "missing label \"ready-for-agent\""
    );
}

#[test]
fn ready_ticket_discovery_returns_typed_tickets_matching_the_filter() {
    let discovery = TicketDiscovery::new(StubQuery::returning(
        r#"{"tickets":[{"id":"AMBA-42","title":"Ship typed discovery","status":"Todo","labels":["ready-for-agent"]}]}"#,
    ));
    let filter = ReadyTicketFilter::new(
        "ready-for-agent",
        TicketStatus::parse("Todo").expect("expected workflow status"),
    )
    .expect("Ready Ticket filter");

    let tickets = discovery
        .ready_tickets(&filter)
        .expect("Ready Tickets should be discovered");

    assert_eq!(
        tickets.tickets(),
        &[Ticket::new(
            TicketId::parse("AMBA-42").expect("Ticket ID"),
            TicketTitle::parse("Ship typed discovery").expect("Ticket title"),
            TicketStatus::parse("Todo").expect("Ticket status"),
        )]
    );
    assert!(tickets.diagnostics().is_empty());
}

#[test]
fn ticket_values_preserve_valid_linear_identity_and_relationships() {
    let id = TicketId::parse("AMBA-42").expect("Ticket ID");
    let ticket = Ticket::new(
        id.clone(),
        TicketTitle::parse("Ship typed discovery").expect("Ticket title"),
        TicketStatus::parse("Todo").expect("Ticket status"),
    );
    let parent = ParentTicket::new(TicketId::parse("AMBA-40").expect("Parent Ticket ID"));
    let blocker = Blocker::new(TicketId::parse("AMBA-41").expect("Blocker ID"));

    assert_eq!(ticket.id(), &id);
    assert_eq!(ticket.title().as_str(), "Ship typed discovery");
    assert_eq!(ticket.status().as_str(), "Todo");
    assert_eq!(parent.id().as_str(), "AMBA-40");
    assert_eq!(blocker.id().as_str(), "AMBA-41");
}

#[test]
fn ticket_values_reject_missing_titles_and_statuses() {
    assert_eq!(
        TicketTitle::parse("   ")
            .expect_err("blank title should fail")
            .to_string(),
        "Ticket title cannot be blank"
    );
    assert_eq!(
        TicketStatus::parse("")
            .expect_err("blank status should fail")
            .to_string(),
        "Ticket status cannot be blank"
    );
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write fake runtime");
    let mut permissions = fs::metadata(path).expect("runtime metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make runtime executable");
}
