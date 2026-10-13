use skilj::{auto_register, CommandDecision, CommandType, Snapshot};
use skilj_core::plugin::Decision;

pub const BOUNDED_CONTEXT: &str = "ticket";

#[derive(Default, Serialize, Deserialize)]
pub struct TicketFacts {
    pub open: bool,
}

pub struct TicketSnapshot;

#[auto_register(BOUNDED_CONTEXT)]
impl Snapshot for TicketSnapshot {
    type State = TicketFacts;
    type Event = TicketEvent;
    const NAME: &'static str = "TicketSnapshot";
    const TAG_KEY: &'static str = "ticket";
    fn fold_for(state: &mut Self::State, _tag_value: &str, event: &Self::Event) {
        state.open = matches!(event, TicketEvent::TicketOpened(_));
    }
}

fn on_ticket(decision: &Decision<'_, TicketEvent>, ticket_id: &str, decide: impl FnOnce(&TicketFacts) -> CommandDecision) -> CommandDecision {
    match decision.snapshot::<TicketSnapshot>(ticket_id) {
        Some(facts) => decide(&facts),
        None => CommandDecision::Rejected { reason: "no snapshot".into(), kind: "snapshot_unreadable".into() },
    }
}

pub struct CloseTicket;

#[auto_register(BOUNDED_CONTEXT)]
impl CommandType for CloseTicket {
    type Payload = CloseTicketPayload;
    type Event = TicketEvent;
    const NAME: &'static str = "CloseTicket";

    fn decide_with(payload: &Self::Payload, decision: &Decision<'_, Self::Event>) -> CommandDecision {
        on_ticket(decision, &payload.ticket_id, |facts| Self::decide_on(facts))
    }
}

impl CloseTicket {
    pub fn decide_on(facts: &TicketFacts) -> CommandDecision {
        if facts.open {
            CommandDecision::Accepted { events: vec![] }
        } else {
            CommandDecision::Rejected { reason: "not open".into(), kind: "ticket_not_open".into() }
        }
    }
}

/// Closes through `CloseTicket`'s logic: its `ticket_not_open` is only
/// reached through `CloseTicket::decide_on`.
pub struct ArchiveTicket;

#[auto_register(BOUNDED_CONTEXT)]
impl CommandType for ArchiveTicket {
    type Payload = CloseTicketPayload;
    type Event = TicketEvent;
    const NAME: &'static str = "ArchiveTicket";

    fn decide_with(payload: &Self::Payload, decision: &Decision<'_, Self::Event>) -> CommandDecision {
        on_ticket(decision, &payload.ticket_id, |facts| CloseTicket::decide_on(facts))
    }
}
