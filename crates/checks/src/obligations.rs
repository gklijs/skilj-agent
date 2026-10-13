//! What "properly tested" means (spec: TestObligation), and which tests
//! cover each obligation.
//!
//! Commands: a `decide()` unit test for the accepted path and for every
//! rejection kind, plus one integration test. Projections: a fold unit
//! test per consumed event type, plus one integration test. Matching is
//! by what a test mentions; a `// skilj-agent: covers <id>` comment in a
//! test covers an obligation the matching misses.

use crate::{Level, Obligation, Source};
use skilj_agent_index::{PluginType, ProjectIndex, TestFn, TestLevel, TypeKind};

pub(crate) fn from_code(index: &ProjectIndex) -> Vec<Obligation> {
    let mut obligations = Vec::new();
    for ty in &index.types {
        match ty.kind {
            TypeKind::Command => command(index, ty, &mut obligations),
            TypeKind::Projection => projection(index, ty, &mut obligations),
            TypeKind::Event | TypeKind::Snapshot => {}
        }
    }
    obligations
}

fn command(index: &ProjectIndex, cmd: &PluginType, out: &mut Vec<Obligation>) {
    let unit = |t: &&TestFn| t.level == TestLevel::Unit && t.decides(cmd);
    let integration = |t: &&TestFn| t.level == TestLevel::Integration && t.mentions(cmd);

    let accepted = format!("command.{}.accepted", cmd.name);
    out.push(obligation(
        index,
        &accepted,
        &cmd.context,
        Level::Unit,
        |t| unit(t) && cmd.rejection_kinds.iter().all(|k| !t.literals.contains(k)),
        |t| integration(t) && cmd.rejection_kinds.iter().all(|k| !t.literals.contains(k)),
    ));
    for kind in &cmd.rejection_kinds {
        let mut rejected = obligation(
            index,
            &format!("command.{}.rejected.{kind}", cmd.name),
            &cmd.context,
            Level::Unit,
            |t| unit(t) && t.literals.contains(kind),
            |t| integration(t) && t.literals.contains(kind),
        );
        rejected.via = cmd.inherited_kinds.get(kind).map(|v| v.iter().cloned().collect()).unwrap_or_default();
        out.push(rejected);
    }
    out.push(obligation(
        index,
        &format!("command.{}.integration", cmd.name),
        &cmd.context,
        Level::Integration,
        |t| integration(t),
        |_| false,
    ));
}

fn projection(index: &ProjectIndex, p: &PluginType, out: &mut Vec<Obligation>) {
    for event in &p.consumed_event_types {
        let event_rust = index
            .types
            .iter()
            .find(|t| t.kind == TypeKind::Event && &t.name == event)
            .map(|t| t.rust_name.as_str())
            .unwrap_or(event);
        let mentions_event = |t: &TestFn| t.literals.contains(event) || t.idents.contains(event_rust);
        out.push(obligation(
            index,
            &format!("projection.{}.folds.{event}", p.name),
            &p.context,
            Level::Unit,
            |t| t.level == TestLevel::Unit && t.folds(p) && mentions_event(t),
            |t| t.level == TestLevel::Integration && t.mentions(p) && mentions_event(t),
        ));
    }
    out.push(obligation(
        index,
        &format!("projection.{}.integration", p.name),
        &p.context,
        Level::Integration,
        |t| t.level == TestLevel::Integration && t.mentions(p),
        |_| false,
    ));
}

/// `covers` decides coverage; `nearly` finds tests that exercise the
/// same thing at the other level, mentioned in the finding's detail.
fn obligation(
    index: &ProjectIndex,
    id: &str,
    context: &str,
    level: Level,
    covers: impl Fn(&&TestFn) -> bool,
    nearly: impl Fn(&&TestFn) -> bool,
) -> Obligation {
    let name = |t: &TestFn| format!("{}::{}", t.file, t.name);
    let covered_by: Vec<String> = index
        .tests
        .iter()
        .filter(|t| t.markers.contains(id) || covers(t))
        .map(name)
        .collect();
    let at_other_level = if covered_by.is_empty() {
        index.tests.iter().filter(|t| nearly(t)).map(name).collect()
    } else {
        Vec::new()
    };
    Obligation {
        id: id.to_string(),
        context: Some(context.to_string()),
        source: Source::CodeIndex,
        level,
        covered_by,
        at_other_level,
        via: Vec::new(),
    }
}
