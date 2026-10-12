//! Recognises skilj errors in any output (a failing test, server logs, a
//! REST or GraphQL response) by their wire code, their message or their
//! variant name, and attaches the recorded cause and fix (spec:
//! DiagnoseError, lookup first). No model: an error that isn't in the
//! table is reported as unknown.
//!
//! The table is `knowledge/skilj-errors.json`, generated from the skilj
//! source by `scripts/extract-skilj-errors.py`; causes and fixes are in
//! `knowledge/error-fixes.toml`.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const ERRORS_JSON: &str = include_str!("../../../knowledge/skilj-errors.json");
const FIXES_TOML: &str = include_str!("../../../knowledge/error-fixes.toml");

/// The shortest message fragment matched on its own; shorter ones are
/// too likely to appear in unrelated output.
const MIN_MESSAGE_FRAGMENT: usize = 20;
const MAX_LINE_TEXT: usize = 200;

pub struct Knowledge {
    pub skilj_version: String,
    errors: Vec<SkiljError>,
    fixes: BTreeMap<String, Fix>,
    pitfalls: Vec<Pitfall>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkiljError {
    pub module: String,
    pub variant: String,
    pub code: String,
    pub message: String,
}

#[derive(Deserialize)]
struct ErrorTable {
    skilj_version: String,
    errors: Vec<SkiljError>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixes {
    #[serde(default)]
    fix: Vec<Fix>,
    #[serde(default)]
    pitfall: Vec<Pitfall>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fix {
    variant: String,
    when: Option<String>,
    cause: String,
    fix: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pitfall {
    matches: String,
    cause: String,
    fix: String,
}

impl Knowledge {
    /// The knowledge compiled into this binary.
    pub fn bundled() -> Self {
        let table: ErrorTable = serde_json::from_str(ERRORS_JSON).expect("knowledge/skilj-errors.json is valid");
        let fixes: Fixes = toml::from_str(FIXES_TOML).expect("knowledge/error-fixes.toml is valid");
        Knowledge {
            skilj_version: table.skilj_version,
            errors: table.errors,
            fixes: fixes.fix.into_iter().map(|f| (f.variant.clone(), f)).collect(),
            pitfalls: fixes.pitfall,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Diagnosis {
    pub knowledge_skilj_version: String,
    pub recognised: Vec<Recognised>,
}

#[derive(Debug, Serialize)]
pub struct Recognised {
    /// None for a pitfall: output worth recognising that isn't an error.
    pub error: Option<SkiljError>,
    pub matched_by: MatchedBy,
    /// 1-based line in the output where it was first seen.
    pub line: usize,
    pub line_text: String,
    pub when: Option<String>,
    /// None when no cause and fix are recorded for this error yet.
    pub cause: Option<String>,
    pub fix: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchedBy {
    Code,
    Message,
    Variant,
    Pitfall,
}

pub fn diagnose(knowledge: &Knowledge, output: &str) -> Diagnosis {
    let mut recognised: Vec<Recognised> = Vec::new();
    let mut seen_codes = BTreeSet::new();
    let mut seen_pitfalls = BTreeSet::new();
    for (i, line) in output.lines().enumerate() {
        for error in &knowledge.errors {
            let Some(matched_by) = matches(error, line) else { continue };
            // Two modules define external_subject_already_claimed with
            // the same meaning: report a code once.
            if !seen_codes.insert(error.code.clone()) {
                continue;
            }
            let fix = knowledge.fixes.get(&error.variant);
            recognised.push(Recognised {
                error: Some(error.clone()),
                matched_by,
                line: i + 1,
                line_text: shorten(line),
                when: fix.and_then(|f| f.when.clone()),
                cause: fix.map(|f| f.cause.clone()),
                fix: fix.map(|f| f.fix.clone()),
            });
        }
        for (p, pitfall) in knowledge.pitfalls.iter().enumerate() {
            if contains_word(line, &pitfall.matches) && seen_pitfalls.insert(p) {
                recognised.push(Recognised {
                    error: None,
                    matched_by: MatchedBy::Pitfall,
                    line: i + 1,
                    line_text: shorten(line),
                    when: None,
                    cause: Some(pitfall.cause.clone()),
                    fix: Some(pitfall.fix.clone()),
                });
            }
        }
    }
    Diagnosis { knowledge_skilj_version: knowledge.skilj_version.clone(), recognised }
}

fn matches(error: &SkiljError, line: &str) -> Option<MatchedBy> {
    if contains_word(line, &error.code) {
        return Some(MatchedBy::Code);
    }
    // Messages may hold format arguments ({0}, {name}): match the longest
    // literal part.
    let fragment = error
        .message
        .split(['{', '}'])
        .step_by(2)
        .map(str::trim)
        .max_by_key(|f| f.len())
        .unwrap_or("");
    if fragment.len() >= MIN_MESSAGE_FRAGMENT && line.contains(fragment) {
        return Some(MatchedBy::Message);
    }
    // Debug output: `Err(InvalidTagMapping)`. Only multi-word names; a
    // bare `Database` would match far too much.
    let words = error.variant.chars().filter(|c| c.is_uppercase()).count();
    if words >= 2 && contains_word(line, &error.variant) {
        return Some(MatchedBy::Variant);
    }
    None
}

/// `needle` in `line`, not as part of a longer identifier.
fn contains_word(line: &str, needle: &str) -> bool {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    line.match_indices(needle).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + needle.len()..].chars().next();
        !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
    })
}

fn shorten(line: &str) -> String {
    let line = line.trim();
    match line.char_indices().nth(MAX_LINE_TEXT) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variants(output: &str) -> Vec<(String, MatchedBy)> {
        diagnose(&Knowledge::bundled(), output)
            .recognised
            .into_iter()
            .map(|r| (r.error.map(|e| e.variant).unwrap_or_else(|| "pitfall".into()), r.matched_by))
            .collect()
    }

    #[test]
    fn every_fix_names_a_known_variant() {
        let k = Knowledge::bundled();
        for variant in k.fixes.keys() {
            assert!(
                k.errors.iter().any(|e| &e.variant == variant),
                "error-fixes.toml has {variant}, which skilj-errors.json doesn't"
            );
        }
    }

    #[test]
    fn recognises_a_rest_error_body_by_code() {
        let output = r#"{"code":"invalid_tag_mapping","message":"..."}"#;
        assert_eq!(variants(output), vec![("InvalidTagMapping".into(), MatchedBy::Code)]);
    }

    #[test]
    fn recognises_a_message_with_a_format_argument() {
        let output = "Error: decide() named an event type this bounded context has not registered: Withdrawn";
        assert_eq!(variants(output), vec![("UnregisteredEventType".into(), MatchedBy::Message)]);
    }

    #[test]
    fn recognises_debug_output_by_variant() {
        let output = "called `Result::unwrap()` on an `Err` value: EventStore(SchemaIncompatible)";
        assert_eq!(variants(output), vec![("SchemaIncompatible".into(), MatchedBy::Variant)]);
    }

    #[test]
    fn attaches_the_recorded_fix() {
        let d = diagnose(&Knowledge::bundled(), "no_decider_registered");
        assert_eq!(d.recognised[0].when.as_deref(), Some("submission"));
        assert!(d.recognised[0].fix.as_deref().unwrap().contains("binary"));
    }

    #[test]
    fn a_known_error_without_a_fix_is_still_recognised() {
        let d = diagnose(&Knowledge::bundled(), "at most 32 tags may be given in one query");
        assert_eq!(d.recognised.len(), 1);
        assert!(d.recognised[0].fix.is_none());
    }

    #[test]
    fn recognises_a_pitfall() {
        assert_eq!(variants("report.skipped_no_access = [\"marketing\"]"), vec![("pitfall".into(), MatchedBy::Pitfall)]);
    }

    #[test]
    fn ignores_parts_of_longer_identifiers_and_unrelated_output() {
        assert!(variants("my_invalid_filter_count = 3").is_empty());
        assert!(variants("test result: ok. 14 passed; 0 failed").is_empty());
        assert!(variants("Database connection pool ready").is_empty());
    }
}
