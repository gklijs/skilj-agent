//! How each bounded context is laid out, which project style that adds
//! up to, and the layout findings (spec: CheckLayout).

use crate::{ContextLayout, ContextStyle, Finding, FindingKind, LayoutStyle};
use skilj_agent_index::{FileKind, ProjectIndex, TypeKind};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// A context file longer than this is too big (spec: max_context_file_lines).
pub const MAX_CONTEXT_FILE_LINES: usize = 800;

const KIND_STEMS: [&str; 8] = [
    "events", "event", "commands", "command", "projections", "projection", "snapshots", "snapshot",
];

pub(crate) fn contexts(index: &ProjectIndex) -> Vec<ContextLayout> {
    // Which contexts have types in each file.
    let mut contexts_in_file: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for t in &index.types {
        contexts_in_file.entry(&t.file).or_default().insert(&t.context);
    }
    let contexts_per_package = {
        let mut m: BTreeMap<&str, usize> = BTreeMap::new();
        for c in &index.contexts {
            *m.entry(&c.package).or_default() += 1;
        }
        m
    };
    let lines = |f: &str| index.files.iter().find(|s| s.path == f).map_or(0, |s| s.lines);

    index
        .contexts
        .iter()
        .map(|ctx| {
            // A skilj-codegen pair (banking.skilj.toml + banking.rs) is
            // one unit, as far as layout goes.
            let files: Vec<&str> = ctx
                .files
                .iter()
                .map(String::as_str)
                .filter(|f| {
                    f.strip_suffix(".skilj.toml")
                        .is_none_or(|stem| !ctx.files.contains(&format!("{stem}.rs")))
                })
                .collect();
            let shares = files.iter().any(|f| contexts_in_file.get(f).is_some_and(|c| c.len() > 1));
            let style = if shares {
                ContextStyle::SharesFile
            } else if contexts_per_package.len() > 1 && contexts_per_package.get(ctx.package.as_str()) == Some(&1) {
                ContextStyle::CratePerContext
            } else if files.len() <= 1 {
                ContextStyle::SingleFile
            } else {
                folder_style(index, ctx.name.as_str(), &files)
            };
            ContextLayout {
                name: ctx.name.clone(),
                style,
                files: ctx.files.clone(),
                lines: files.iter().map(|f| lines(f)).sum(),
            }
        })
        .collect()
}

fn folder_style(index: &ProjectIndex, context: &str, files: &[&str]) -> ContextStyle {
    let dirs: BTreeSet<&Path> = files.iter().filter_map(|f| Path::new(f).parent()).collect();
    let dir = match dirs.iter().next() {
        Some(d) if dirs.len() == 1 && !d.ends_with("src") => *d,
        _ => return ContextStyle::Scattered,
    };
    // Another context's types in the same folder: not its own folder.
    if index
        .types
        .iter()
        .any(|t| t.context != context && Path::new(&t.file).parent() == Some(dir))
    {
        return ContextStyle::Scattered;
    }
    let stems: BTreeSet<String> = index
        .files
        .iter()
        .filter(|f| f.kind == FileKind::Src && Path::new(&f.path).parent() == Some(dir))
        .filter_map(|f| Path::new(&f.path).file_stem().map(|s| s.to_string_lossy().to_string()))
        .collect();
    if stems.iter().filter(|s| KIND_STEMS.contains(&s.as_str())).count() >= 2 {
        return ContextStyle::ByKind;
    }
    // By slice: no file holds more than one command or projection.
    let mut per_file: BTreeMap<&str, usize> = BTreeMap::new();
    for t in index.types.iter().filter(|t| t.context == context) {
        if matches!(t.kind, TypeKind::Command | TypeKind::Projection) {
            *per_file.entry(&t.file).or_default() += 1;
        }
    }
    if per_file.values().all(|n| *n <= 1) {
        ContextStyle::BySlice
    } else {
        ContextStyle::OtherFolder
    }
}

/// The style most contexts follow, when at least half of them do.
pub(crate) fn detected(contexts: &[ContextLayout]) -> Option<LayoutStyle> {
    let mut counts: BTreeMap<LayoutStyle, usize> = BTreeMap::new();
    for c in contexts {
        if let Some(style) = c.style.as_layout_style() {
            *counts.entry(style).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .filter(|(_, n)| n * 2 >= contexts.len())
        .map(|(style, _)| style)
}

pub(crate) fn findings(
    index: &ProjectIndex,
    contexts: &[ContextLayout],
    configured: Option<LayoutStyle>,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut seen_files = BTreeSet::new();
    for t in &index.types {
        let in_file: BTreeSet<&str> = index.types.iter().filter(|o| o.file == t.file).map(|o| o.context.as_str()).collect();
        if in_file.len() > 1 && seen_files.insert(t.file.clone()) {
            findings.push(Finding {
                kind: FindingKind::MixedContexts,
                subject: t.file.clone(),
                detail: format!(
                    "holds types of {} bounded contexts: {}",
                    in_file.len(),
                    in_file.into_iter().collect::<Vec<_>>().join(", ")
                ),
            });
        }
    }
    for ctx in contexts {
        for file in &ctx.files {
            let lines = index.files.iter().find(|f| &f.path == file).map_or(0, |f| f.lines);
            if lines > MAX_CONTEXT_FILE_LINES {
                findings.push(Finding {
                    kind: FindingKind::TooBig,
                    subject: file.clone(),
                    detail: format!(
                        "{lines} lines (more than {MAX_CONTEXT_FILE_LINES}) in bounded context {}",
                        ctx.name
                    ),
                });
            }
        }
        if let Some(style) = configured
            && ctx.style.as_layout_style() != Some(style) {
                findings.push(Finding {
                    kind: FindingKind::OffStyle,
                    subject: ctx.name.clone(),
                    detail: format!(
                        "laid out as {}, the project's layout style is {}",
                        ctx.style.describe(),
                        style.describe()
                    ),
                });
            }
    }
    findings
}
