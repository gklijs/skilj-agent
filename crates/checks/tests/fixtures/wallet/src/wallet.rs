use skilj::{auto_register, CommandDecision, CommandType, EventSpec, EventType, Projection};

pub const BOUNDED_CONTEXT: &str = "wallet";

pub struct Deposited;

#[auto_register(BOUNDED_CONTEXT)]
impl EventType for Deposited {
    type Payload = DepositedPayload;
    const NAME: &'static str = "Deposited";
}

pub struct Withdrawn;

#[auto_register(BOUNDED_CONTEXT)]
impl EventType for Withdrawn {
    type Payload = WithdrawnPayload;
    const NAME: &'static str = "Withdrawn";
}

pub struct Withdraw;

#[auto_register(BOUNDED_CONTEXT)]
impl CommandType for Withdraw {
    type Payload = WithdrawPayload;
    type Event = WalletEvent;
    const NAME: &'static str = "Withdraw";

    fn decide(payload: &Self::Payload, events: &[Self::Event]) -> CommandDecision {
        if payload.amount <= 0 {
            return CommandDecision::Rejected {
                reason: "amount must be positive".into(),
                kind: "invalid_amount".into(),
            };
        }
        Self::check_balance(payload, events).unwrap_or_else(|| CommandDecision::Accepted {
            events: vec![EventSpec { event_type: Withdrawn::NAME.into(), payload: serde_json::json!({}) }],
        })
    }
}

impl Withdraw {
    fn check_balance(payload: &WithdrawPayload, events: &[WalletEvent]) -> Option<CommandDecision> {
        (balance(events) < payload.amount).then(|| CommandDecision::Rejected {
            reason: "not enough money".into(),
            kind: "insufficient_funds".into(),
        })
    }
}

pub struct Balance;

#[auto_register(BOUNDED_CONTEXT)]
impl Projection for Balance {
    type State = BalanceState;
    type Event = WalletEvent;
    const NAME: &'static str = "Balance";

    fn consumed_event_types() -> Vec<&'static str> {
        vec![Deposited::NAME, "Withdrawn"]
    }

    fn project(state: &mut Self::State, event: &Self::Event, _key: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn withdraw(amount: i64) -> CommandDecision {
        Withdraw::decide(&WithdrawPayload { amount }, &[])
    }

    #[test]
    fn a_negative_amount_is_rejected() {
        assert_eq!(kind(withdraw(-1)), Some("invalid_amount".to_string()));
    }

    #[test]
    fn deposits_add_up() {
        let mut state = BalanceState::default();
        Balance::project(&mut state, &WalletEvent::Deposited(DepositedPayload { amount: 5 }), "");
        assert_eq!(state.amount, 5);
    }

    #[test]
    fn checked_by_hand() {
        // skilj-agent: covers command.Withdraw.accepted
    }
}
