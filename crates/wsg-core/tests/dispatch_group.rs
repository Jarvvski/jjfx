use wsg_core::{
    DispatchGroup, DispatchGroupOptions, DispatchGroupState, SubIssueStatus, TicketId, WireStatus,
    WireTimestamp,
};

fn state() -> DispatchGroupState {
    DispatchGroupState::new(
        TicketId::parse("ENG-100").expect("parent Ticket ID"),
        WireTimestamp::new("2026-08-01T10:00:00Z"),
        "owner/repo",
        DispatchGroupOptions::new("model"),
    )
}

#[test]
fn compatible_sub_issue_statuses_round_trip_their_wire_spellings() {
    for spelling in ["pending", "dispatched", "done", "failed", "skipped"] {
        let wire = WireStatus::new(spelling);
        let status = SubIssueStatus::try_from(&wire).expect("compatible status");
        assert_eq!(status.to_string(), spelling);
        assert_eq!(WireStatus::from(status), wire);
    }
}

#[test]
fn unknown_sub_issue_status_is_rejected_without_changing_the_wire_value() {
    let wire = WireStatus::new("future-status");
    let error = SubIssueStatus::try_from(&wire).expect_err("unknown status");
    assert_eq!(error.as_str(), "future-status");
}

#[test]
fn aggregate_wraps_valid_wire_state_without_persistence() {
    let group = DispatchGroup::from_state(state()).expect("valid state");
    assert_eq!(group.state().parent.as_str(), "ENG-100");
}
