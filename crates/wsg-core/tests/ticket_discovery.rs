use wsg_core::{Blocker, ParentTicket, Ticket, TicketId, TicketStatus, TicketTitle};

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
        TicketTitle::parse("   ").expect_err("blank title should fail").to_string(),
        "Ticket title cannot be blank"
    );
    assert_eq!(
        TicketStatus::parse("").expect_err("blank status should fail").to_string(),
        "Ticket status cannot be blank"
    );
}
