//! What write-test decides before asking a model, on the wallet fixture.

use skilj_agent_tasks::write_test::{Placement, plan};
use std::path::Path;

fn wallet() -> skilj_agent_index::ProjectIndex {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../checks/tests/fixtures/wallet");
    skilj_agent_index::index(&root).unwrap()
}

#[test]
fn a_rejection_shows_the_helper_holding_its_kind_and_goes_into_the_existing_tests() {
    let plan = plan(&wallet(), "command.Withdraw.rejected.insufficient_funds").unwrap();
    assert_eq!(plan.call, "decide");
    assert_eq!(plan.file, "src/wallet.rs");
    let code: String = plan.context.iter().map(|e| e.code.as_str()).collect();
    assert!(code.contains("\"insufficient_funds\""), "{code}");
    assert!(code.contains("fn check_balance"));
    assert!(matches!(&plan.placement, Placement::Module { name, .. } if name == "tests"));
    let example = plan.example.as_ref().expect("an example test");
    assert!(example.code.contains("fn withdraw(amount: i64)"), "the helper the example uses");
    assert!(plan.taken.contains("a_negative_amount_is_rejected"));
    assert!(plan.task_prompt().contains("Withdraw::decide(...)"));
}

#[test]
fn a_fold_names_the_event_and_imitates_a_fold_test() {
    let plan = plan(&wallet(), "projection.Balance.folds.Withdrawn").unwrap();
    assert_eq!(plan.call, "project");
    assert!(plan.example.as_ref().unwrap().code.contains("Balance::project"));
}

#[test]
fn covered_and_integration_obligations_are_refused() {
    let index = wallet();
    let covered = plan(&index, "command.Withdraw.rejected.invalid_amount").unwrap_err();
    assert!(covered.to_string().contains("already covered"), "{covered}");
    let integration = plan(&index, "command.Withdraw.integration").unwrap_err();
    assert!(integration.to_string().contains("not a unit test obligation"), "{integration}");
}
