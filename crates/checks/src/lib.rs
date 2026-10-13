//! Turns a project index into findings, without a model (spec:
//! CheckTesting, CheckLayout, the version check). Same project, same
//! report.

mod allium;
pub mod config;
pub mod diagnose;
pub mod layout;
mod obligations;

pub use config::ProjectConfig;
use serde::{Deserialize, Serialize};
use skilj_agent_index::{ProjectIndex, TypeKind};

/// The skilj version this release's knowledge is written for (spec:
/// config.knowledge_skilj_version).
pub const KNOWLEDGE_SKILJ_VERSION: &str = "0.0.10";

#[derive(Debug, Serialize)]
pub struct Report {
    pub project: String,
    pub skilj_version: Option<String>,
    pub knowledge_skilj_version: &'static str,
    pub version_fit: VersionFit,
    pub layout: LayoutReport,
    pub contexts: Vec<ContextSummary>,
    pub obligations: Vec<Obligation>,
    /// Open findings; dismissed ones are only counted.
    pub findings: Vec<Finding>,
    pub dismissed: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionFit {
    Same,
    /// The project uses an older skilj: upgrading is worth offering.
    Older,
    /// The project uses a newer skilj: this skilj-agent may be outdated.
    Newer,
    Unknown,
}

#[derive(Debug, Serialize)]
pub struct LayoutReport {
    /// From `.skilj-agent.toml`; off-style findings only when set.
    pub configured: Option<LayoutStyle>,
    pub detected: Option<LayoutStyle>,
    pub contexts: Vec<ContextLayout>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutStyle {
    /// src/<context>/{mod,events,commands,projections}.rs
    ByKind,
    /// src/<context>/<one command or projection>.rs
    BySlice,
    /// crates/<context>/ in a workspace
    CratePerContext,
}

impl LayoutStyle {
    pub fn describe(self) -> &'static str {
        match self {
            LayoutStyle::ByKind => "by kind",
            LayoutStyle::BySlice => "by slice",
            LayoutStyle::CratePerContext => "crate per context",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ContextLayout {
    pub name: String,
    pub style: ContextStyle,
    pub files: Vec<String>,
    pub lines: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextStyle {
    ByKind,
    BySlice,
    CratePerContext,
    /// All of it in one file.
    SingleFile,
    /// Shares a file with another context.
    SharesFile,
    /// Its own folder, but neither by kind nor by slice.
    OtherFolder,
    /// Spread over files in a folder it shares with other code.
    Scattered,
}

impl ContextStyle {
    pub fn as_layout_style(self) -> Option<LayoutStyle> {
        match self {
            ContextStyle::ByKind => Some(LayoutStyle::ByKind),
            ContextStyle::BySlice => Some(LayoutStyle::BySlice),
            ContextStyle::CratePerContext => Some(LayoutStyle::CratePerContext),
            _ => None,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            ContextStyle::ByKind => "by kind",
            ContextStyle::BySlice => "by slice",
            ContextStyle::CratePerContext => "crate per context",
            ContextStyle::SingleFile => "a single file",
            ContextStyle::SharesFile => "sharing a file with another context",
            ContextStyle::OtherFolder => "its own folder (no recognised style)",
            ContextStyle::Scattered => "files shared with other code",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ContextSummary {
    pub name: String,
    pub events: usize,
    pub commands: usize,
    pub projections: usize,
    pub snapshots: usize,
}

#[derive(Debug, Serialize)]
pub struct Obligation {
    /// e.g. `command.Withdraw.rejected.insufficient_funds`, or an
    /// `allium plan` id like `rule-success.Withdraw`.
    pub id: String,
    pub context: Option<String>,
    pub source: Source,
    pub level: Level,
    /// `file::test` for every covering test; `compiler` for obligations
    /// the compiler already guarantees.
    pub covered_by: Vec<String>,
    /// When uncovered: tests exercising the same thing at the other
    /// level, e.g. a rejection only checked by an integration test.
    pub at_other_level: Vec<String>,
    /// A rejection reached only through other commands' code: those
    /// methods (`RequestInfoFromCustomer::decide_on`). It may be
    /// unreachable, depending on what the command passes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub via: Vec<String>,
}

impl Obligation {
    pub fn covered(&self) -> bool {
        !self.covered_by.is_empty()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Source {
    CodeIndex,
    AlliumSpec { spec: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Unit,
    Integration,
    Compiler,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub kind: FindingKind,
    /// An obligation id, a file or a bounded context.
    pub subject: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    Untested,
    TooBig,
    MixedContexts,
    OffStyle,
    Performance,
    KnownError,
}

/// The obligations derived from the code alone: no `allium plan`, so
/// cheap enough to rerun after every proposed change.
pub fn code_obligations(index: &ProjectIndex) -> Vec<Obligation> {
    obligations::from_code(index)
}

/// Runs every check on an indexed project.
pub fn check(index: &ProjectIndex, config: &ProjectConfig) -> Report {
    let mut warnings = index.warnings.clone();

    let mut obligations = obligations::from_code(index);
    obligations.extend(allium::from_specs(index, &mut warnings));

    let mut findings: Vec<Finding> = obligations
        .iter()
        .filter(|o| !o.covered())
        .map(|o| Finding {
            kind: FindingKind::Untested,
            subject: o.id.clone(),
            detail: untested_detail(o),
        })
        .collect();

    let contexts = layout::contexts(index);
    findings.extend(layout::findings(index, &contexts, config.layout_style));

    let before = findings.len();
    findings.retain(|f| config.dismissal(f.kind, &f.subject).is_none());
    let dismissed = before - findings.len();

    Report {
        project: index.root.display().to_string(),
        skilj_version: index.skilj_version.clone(),
        knowledge_skilj_version: KNOWLEDGE_SKILJ_VERSION,
        version_fit: version_fit(index.skilj_version.as_deref()),
        layout: LayoutReport {
            configured: config.layout_style,
            detected: layout::detected(&contexts),
            contexts,
        },
        contexts: index
            .contexts
            .iter()
            .map(|c| {
                let count = |k| index.types.iter().filter(|t| t.context == c.name && t.kind == k).count();
                ContextSummary {
                    name: c.name.clone(),
                    events: count(TypeKind::Event),
                    commands: count(TypeKind::Command),
                    projections: count(TypeKind::Projection),
                    snapshots: count(TypeKind::Snapshot),
                }
            })
            .collect(),
        obligations,
        findings,
        dismissed,
        warnings,
    }
}

fn untested_detail(o: &Obligation) -> String {
    let wanted = match o.level {
        Level::Unit => "no unit test",
        Level::Integration => "no integration test",
        Level::Compiler => "not covered",
    };
    let detail = match (&o.source, o.at_other_level.as_slice()) {
        (Source::AlliumSpec { spec }, _) => format!("{wanted} marked `// allium: {}` ({spec})", o.id),
        (_, []) => wanted.to_string(),
        (_, [one]) => format!("{wanted}; only tested by {one}"),
        (_, [first, rest @ ..]) => format!("{wanted}; only tested by {first} and {} more", rest.len()),
    };
    if o.via.is_empty() {
        return detail;
    }
    format!("{detail}; only reached through {}, so it may be unreachable from here", o.via.join(" or "))
}

fn version_fit(requirement: Option<&str>) -> VersionFit {
    let Some(req) = requirement.and_then(|r| semver::VersionReq::parse(r).ok()) else {
        return VersionFit::Unknown;
    };
    let Some(c) = req.comparators.first() else {
        return VersionFit::Unknown;
    };
    let project = semver::Version::new(c.major, c.minor.unwrap_or(0), c.patch.unwrap_or(0));
    let knowledge = semver::Version::parse(KNOWLEDGE_SKILJ_VERSION).expect("a valid version");
    match project.cmp(&knowledge) {
        std::cmp::Ordering::Less => VersionFit::Older,
        std::cmp::Ordering::Equal => VersionFit::Same,
        std::cmp::Ordering::Greater => VersionFit::Newer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_fit_compares_with_knowledge() {
        assert_eq!(version_fit(Some("0.0.10")), VersionFit::Same);
        assert_eq!(version_fit(Some("^0.0.8")), VersionFit::Older);
        assert_eq!(version_fit(Some("=0.1.0")), VersionFit::Newer);
        assert_eq!(version_fit(None), VersionFit::Unknown);
    }
}
