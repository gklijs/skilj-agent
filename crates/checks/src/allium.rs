//! Obligations from the project's Allium specs, via `allium plan`. The
//! level follows from the category (spec: DeriveSpecObligations); the
//! categories the compiler and the registered JSON Schema already
//! guarantee start out covered. The rest are covered by a
//! `// allium: <id>` (or `// skilj-agent: covers <id>`) comment in a test.

use crate::{Level, Obligation, Source};
use serde::Deserialize;
use skilj_agent_index::ProjectIndex;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Deserialize)]
struct Plan {
    obligations: Vec<Planned>,
}

#[derive(Deserialize)]
struct Planned {
    id: String,
    category: String,
}

pub(crate) fn level_for_category(category: &str) -> Level {
    match category {
        "rule_success" | "rule_failure" | "when_set" | "when_clear" | "when_presence"
        | "rule_entity_creation" | "derived" => Level::Unit,
        c if c.starts_with("transition_") => Level::Unit,
        "surface_actor" | "surface_provides" | "surface_exposure" | "temporal" => Level::Integration,
        "entity_fields" | "entity_optional" | "enum_comparable" | "config_default"
        | "entity_relationship" => Level::Compiler,
        // A category this release doesn't know yet: ask for a test until
        // someone decides otherwise.
        _ => Level::Unit,
    }
}

/// The `.allium` files under `specs/` in the project root.
pub(crate) fn spec_files(root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join("specs"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "allium"))
        .collect();
    files.sort();
    files
}

pub(crate) fn from_specs(index: &ProjectIndex, warnings: &mut Vec<String>) -> Vec<Obligation> {
    let specs = spec_files(&index.root);
    let mut obligations = Vec::new();
    for spec in &specs {
        let rel = spec.strip_prefix(&index.root).unwrap_or(spec).display().to_string();
        let output = match Command::new("allium").arg("plan").arg(spec).output() {
            Ok(o) => o,
            Err(_) => {
                warnings.push("allium CLI not found: skipped the obligations from specs/*.allium".into());
                return Vec::new();
            }
        };
        let plan: Plan = match serde_json::from_slice(&output.stdout) {
            Ok(p) => p,
            Err(e) => {
                warnings.push(format!("{rel}: could not read `allium plan` output: {e}"));
                continue;
            }
        };
        if !output.status.success() {
            warnings.push(format!("{rel}: the spec has errors (`allium check {rel}`)"));
        }
        for planned in plan.obligations {
            let level = level_for_category(&planned.category);
            let covered_by = if level == Level::Compiler {
                vec!["compiler".to_string()]
            } else {
                index
                    .tests
                    .iter()
                    .filter(|t| t.markers.contains(&planned.id))
                    .map(|t| format!("{}::{}", t.file, t.name))
                    .collect()
            };
            obligations.push(Obligation {
                id: planned.id,
                context: None,
                source: Source::AlliumSpec { spec: rel.clone() },
                level,
                covered_by,
                at_other_level: Vec::new(),
            });
        }
    }
    obligations
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_map_to_levels() {
        assert_eq!(level_for_category("rule_failure"), Level::Unit);
        assert_eq!(level_for_category("transition_edge"), Level::Unit);
        assert_eq!(level_for_category("surface_actor"), Level::Integration);
        assert_eq!(level_for_category("temporal"), Level::Integration);
        assert_eq!(level_for_category("entity_fields"), Level::Compiler);
    }
}
