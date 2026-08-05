use std::fs;

use tempfile::TempDir;
use wsg_core::{AgentRuntime, OrchestrationRequest, Repository, TicketId};

#[test]
fn orchestration_request_and_repository_expose_the_frontend_neutral_seam() {
    let directory = TempDir::new().expect("temporary repository");
    fs::create_dir(directory.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(directory.path()).expect("open repository");
    let parent = TicketId::parse("ENG-100").expect("Parent Ticket");

    let request =
        OrchestrationRequest::new(parent.clone(), AgentRuntime::Codex).with_model("gpt-5");
    let runner = repository.orchestration_runner();

    assert_eq!(request.parent(), &parent);
    assert_eq!(request.agent_runtime(), AgentRuntime::Codex);
    assert_eq!(request.model(), Some("gpt-5"));
    assert_eq!(runner.repository_root(), repository.root());
}
