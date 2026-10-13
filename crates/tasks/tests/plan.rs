//! What write-test decides before asking a model, on the wallet fixture.

use skilj_agent_tasks::write_test::{Placement, plan};
use std::path::Path;

fn wallet() -> skilj_agent_index::ProjectIndex {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../checks/tests/fixtures/wallet");
    skilj_agent_index::index(&root).unwrap()
}

#[test]
fn a_rejection_shows_the_helper_holding_its_kind_and_goes_into_the_existing_tests() {
    let plan = plan(&wallet(), &Default::default(), "command.Withdraw.rejected.insufficient_funds").unwrap();
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
    let plan = plan(&wallet(), &Default::default(), "projection.Balance.folds.Withdrawn").unwrap();
    assert_eq!(plan.call, "project");
    assert!(plan.example.as_ref().unwrap().code.contains("Balance::project"));
}

#[test]
fn covered_and_integration_obligations_are_refused() {
    let index = wallet();
    let covered = plan(&index, &Default::default(), "command.Withdraw.rejected.invalid_amount").unwrap_err();
    assert!(covered.to_string().contains("already covered"), "{covered}");
    let integration = plan(&index, &Default::default(), "command.Withdraw.integration").unwrap_err();
    assert!(integration.to_string().contains("not a unit test obligation"), "{integration}");
}

fn ticket() -> skilj_agent_index::ProjectIndex {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ticket");
    skilj_agent_index::index(&root).unwrap()
}

#[test]
fn a_snapshot_command_is_shown_its_snapshot_impl_and_told_its_name() {
    let index = ticket();
    let plan = plan(&index, &Default::default(), "command.CloseTicket.rejected.ticket_not_open").unwrap();
    assert_eq!(plan.call, "decide_with");
    let code: String = plan.context.iter().map(|e| e.code.as_str()).collect();
    assert!(code.contains("impl Snapshot for TicketSnapshot"), "{code}");
    assert_eq!(plan.snapshots, vec![("TicketSnapshot".to_string(), "TicketSnapshot".to_string())]);
    assert!(plan.task_prompt().contains(r#"set `snapshot: "TicketSnapshot".to_string()`"#));
}

#[test]
fn a_kind_only_reached_through_another_command_says_so() {
    let index = ticket();
    let archive = index.types.iter().find(|t| t.name == "ArchiveTicket").unwrap();
    // Its own code reaches snapshot_unreadable; ticket_not_open only
    // through CloseTicket's logic.
    assert_eq!(archive.inherited_kinds.keys().collect::<Vec<_>>(), vec!["ticket_not_open"]);
    assert_eq!(archive.inherited_kinds["ticket_not_open"].iter().collect::<Vec<_>>(), vec!["CloseTicket::decide_on"]);
    let close = index.types.iter().find(|t| t.name == "CloseTicket").unwrap();
    assert!(close.inherited_kinds.is_empty(), "a command's own helper isn't another command");

    let plan = plan(&index, &Default::default(), "command.ArchiveTicket.rejected.ticket_not_open").unwrap();
    assert_eq!(plan.via, vec!["CloseTicket::decide_on".to_string()]);
    assert!(plan.task_prompt().contains("That kind comes from `CloseTicket::decide_on`, which `ArchiveTicket` calls"));
    let hint = plan.unreachable_hint(None).expect("a hint for an inherited kind");
    assert!(hint.contains("only reached through `CloseTicket::decide_on`"), "{hint}");
    assert!(hint.contains("subject = \"command.ArchiveTicket.rejected.ticket_not_open\""), "{hint}");

    let own = plan_of(&index, "command.ArchiveTicket.rejected.snapshot_unreadable");
    assert!(own.via.is_empty());
    assert!(own.unreachable_hint(None).is_none());
    let rejected_earlier = std::collections::BTreeSet::from(["ticket_not_open".to_string()]);
    let hint = own.unreachable_hint(Some(&rejected_earlier)).unwrap();
    assert!(hint.starts_with("Every test that ran was rejected earlier, with \"ticket_not_open\"."), "{hint}");
}

#[test]
fn a_dismissed_obligation_is_refused_with_its_reason() {
    let config: skilj_agent_checks::ProjectConfig = toml::from_str(
        r#"
        [[dismissed]]
        kind = "untested"
        subject = "command.ArchiveTicket.rejected.ticket_not_open"
        reason = "archiving only happens to open tickets"
        "#,
    )
    .unwrap();
    let refused = plan(&ticket(), &config, "command.ArchiveTicket.rejected.ticket_not_open").unwrap_err();
    assert_eq!(
        refused.to_string(),
        "`command.ArchiveTicket.rejected.ticket_not_open` is dismissed in .skilj-agent.toml: archiving only happens to open tickets"
    );
}

fn plan_of(index: &skilj_agent_index::ProjectIndex, id: &str) -> skilj_agent_tasks::write_test::Plan {
    plan(index, &Default::default(), id).unwrap()
}
