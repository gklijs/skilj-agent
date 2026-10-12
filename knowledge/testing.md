# Unit testing skilj commands and projections

`decide()` and `project()` are pure functions: no database, no async, no I/O. A unit test calls
them directly.

## A command: `decide()`

```rust
fn decide(payload: &Self::Payload, matching_events: &[Self::Event]) -> CommandDecision;
```

- Build the payload struct with every field set.
- `matching_events` is the history `decide()` sees: a slice of the bounded context's event enum,
  one variant per event type, each wrapping that event's payload struct:
  `&[WalletEvent::Deposited(DepositedPayload { amount: 10 })]`. Use `&[]` for no history.
- Call it on the command type: `Withdraw::decide(&payload, &events)`. It is a trait method, so
  `CommandType` must be in scope (`use super::*;` brings it in when the file imports it).
- Check the outcome by matching on `CommandDecision`:

```rust
match Withdraw::decide(&payload, &events) {
    CommandDecision::Rejected { kind, .. } => assert_eq!(kind, "insufficient_funds"),
    CommandDecision::Accepted { .. } => panic!("expected a rejection"),
}
```

```rust
match Withdraw::decide(&payload, &events) {
    CommandDecision::Accepted { events } => {
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "Withdrawn");
    }
    CommandDecision::Rejected { reason, kind } => panic!("rejected: {kind}: {reason}"),
}
```

- `kind` is a `String`; compare it with the string literal of the rejection kind.
- `EventSpec` has two fields: `event_type: String` and `payload: serde_json::Value`.
- To reach a rejection, read `decide()` and its helpers: find the condition that returns that
  kind, and build a payload and history that meet exactly that condition, and not an earlier one.
- For the accepted path, build input that passes every check.

## A command with a snapshot: `decide_with()`

A command that overrides `decide_with()` decides from a `Decision`: the matching events plus
the state of snapshots. Build one with `Decision::new(&events, &snapshots)`:

```rust
use skilj_core::plugin::{Decision, SnapshotState};

let state = AccountState { balance: 10, ..Default::default() };
let snapshots = vec![SnapshotState {
    snapshot: AccountSnapshot::NAME.to_string(), // the Snapshot type's NAME
    tag_value: "acct-1".to_string(),              // the value decide_with() looks it up by
    state_json: serde_json::to_string(&state).unwrap(),
    folded_through: -1,
}];
let events: Vec<WalletEvent> = vec![];
let decision = Decision::new(&events, &snapshots);
match Withdraw::decide_with(&payload, &decision) { ... }
```

- `NAME` is a trait constant: `Snapshot` must be in scope for `AccountSnapshot::NAME`; or write
  the name as a string literal.
- `decision.snapshot::<S>(tag_value)` returns `None` when no `SnapshotState` has that snapshot
  name and tag value: that is usually a "snapshot unreadable" rejection.
- Build the snapshot's state struct directly (it's the `State` of the `Snapshot` impl); don't
  fold events by hand to get it.

## A projection: `project()`

```rust
fn project(state: &mut Self::State, event: &Self::Event, key: &str);
```

- Start from `Default::default()` (or a state built by folding earlier events), fold one or more
  events, then check the fields of the state.
- `key` is the instance key from `keys()`; pass what `keys()` returns for that event, or `""` for
  a projection with a single instance.

```rust
let mut state = BalanceState::default();
Balance::project(&mut state, &WalletEvent::Deposited(DepositedPayload { amount: 5 }), "acct-1");
assert_eq!(state.balance, 5);
```

## Rules

- One `#[test]` function. No `async`, no database, no new crates.
- Only use types, fields and functions that appear in the code you are shown. Don't guess a
  field name: read the struct.
- Set every field of a struct literal. Only use `..Default::default()` when the struct derives
  `Default`.
- A field whose type is `Option<T>` takes `Some(...)` or `None`; a `String` field takes
  `"text".to_string()` or `"text".into()`.
- Name the test after the behaviour: `withdrawing_more_than_the_balance_is_rejected`.
