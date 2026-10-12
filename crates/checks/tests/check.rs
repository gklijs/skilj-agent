//! `check` end to end on a small fixture project with known coverage.

use skilj_agent_checks::{FindingKind, Level, ProjectConfig, check};
use std::path::Path;

fn report() -> skilj_agent_checks::Report {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wallet");
    let index = skilj_agent_index::index(&root).expect("the fixture indexes");
    check(&index, &ProjectConfig::default())
}

fn covered(report: &skilj_agent_checks::Report, id: &str) -> bool {
    report
        .obligations
        .iter()
        .find(|o| o.id == id)
        .unwrap_or_else(|| panic!("no obligation {id}"))
        .covered()
}

#[test]
fn derives_one_obligation_per_decision_branch_and_fold() {
    let report = report();
    let mut ids: Vec<_> = report.obligations.iter().map(|o| (o.id.as_str(), o.level)).collect();
    ids.sort_by_key(|(id, _)| *id);
    assert_eq!(
        ids,
        vec![
            ("command.Withdraw.accepted", Level::Unit),
            ("command.Withdraw.integration", Level::Integration),
            // Found through the call to Self::check_balance.
            ("command.Withdraw.rejected.insufficient_funds", Level::Unit),
            ("command.Withdraw.rejected.invalid_amount", Level::Unit),
            // Deposited::NAME resolves to the event's name.
            ("projection.Balance.folds.Deposited", Level::Unit),
            ("projection.Balance.folds.Withdrawn", Level::Unit),
            ("projection.Balance.integration", Level::Integration),
        ]
    );
}

#[test]
fn matches_tests_to_obligations() {
    let report = report();
    // Through the local withdraw() helper calling Withdraw::decide.
    assert!(covered(&report, "command.Withdraw.rejected.invalid_amount"));
    // By the `// skilj-agent: covers` marker.
    assert!(covered(&report, "command.Withdraw.accepted"));
    assert!(covered(&report, "projection.Balance.folds.Deposited"));
    assert!(covered(&report, "command.Withdraw.integration"));
    // Only an integration test checks it: not a unit test.
    assert!(!covered(&report, "command.Withdraw.rejected.insufficient_funds"));
    assert!(!covered(&report, "projection.Balance.folds.Withdrawn"));
    assert!(!covered(&report, "projection.Balance.integration"));
}

#[test]
fn says_where_an_untested_branch_is_tested_instead() {
    let report = report();
    let finding = report
        .findings
        .iter()
        .find(|f| f.subject == "command.Withdraw.rejected.insufficient_funds")
        .unwrap();
    assert_eq!(finding.kind, FindingKind::Untested);
    assert_eq!(
        finding.detail,
        "no unit test; only tested by tests/wallet.rs::withdrawing_too_much_is_rejected"
    );
}

#[test]
fn dismissed_findings_stay_dismissed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wallet");
    let index = skilj_agent_index::index(&root).unwrap();
    let config: ProjectConfig = toml::from_str(
        r#"
        [[dismissed]]
        kind = "untested"
        subject = "projection.Balance.integration"
        "#,
    )
    .unwrap();
    let report = check(&index, &config);
    assert_eq!(report.dismissed, 1);
    assert!(report.findings.iter().all(|f| f.subject != "projection.Balance.integration"));
}

#[test]
fn a_configured_layout_style_flags_contexts_that_dont_follow_it() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wallet");
    let index = skilj_agent_index::index(&root).unwrap();
    let report = check(&index, &ProjectConfig::default());
    // One file, so no style is detected and nothing is off-style yet.
    assert_eq!(report.layout.detected, None);
    assert!(report.findings.iter().all(|f| f.kind != FindingKind::OffStyle));

    let config: ProjectConfig = toml::from_str(r#"layout_style = "by_kind""#).unwrap();
    let report = check(&index, &config);
    let off: Vec<_> = report.findings.iter().filter(|f| f.kind == FindingKind::OffStyle).collect();
    assert_eq!(off.len(), 1);
    assert_eq!(off[0].subject, "wallet");
    assert_eq!(off[0].detail, "laid out as a single file, the project's layout style is by kind");
}
