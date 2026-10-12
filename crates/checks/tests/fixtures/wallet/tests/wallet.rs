#[test]
fn withdrawing_too_much_is_rejected() {
    let response = trigger("Withdraw", serde_json::json!({ "amount": 100 }));
    assert_eq!(rejection_kind(&response), "insufficient_funds");
}
