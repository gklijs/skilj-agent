//! skilj-agent: helps working on applications built on skilj. For now
//! the deterministic part only: `check`.

use clap::{Parser, Subcommand};
use skilj_agent_checks::{
    FindingKind, Level, ProjectConfig, Report, Source, VersionFit, config, layout,
};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(version, about = "An agent for applications built on skilj")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Report what's untested in a skilj project and how it's laid out.
    ///
    /// Uses no model: the same project always gives the same report.
    Check {
        /// The project to check.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Print the full report as JSON, for scripts and other agents.
        #[arg(long)]
        json: bool,
        /// List every finding instead of the first few of each kind.
        #[arg(long)]
        all: bool,
    },
}

/// Findings shown per kind without --all.
const SHOWN_PER_KIND: usize = 10;

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Check { path, json, all } => match run_check(&path, json, all) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        },
    }
}

fn run_check(path: &Path, json: bool, all: bool) -> anyhow::Result<()> {
    let index = skilj_agent_index::index(path)?;
    let config = ProjectConfig::load(&index.root)?;
    let report = skilj_agent_checks::check(&index, &config);
    let mut text = if json {
        serde_json::to_string_pretty(&report)?
    } else {
        render_report(&report, all)
    };
    text.push('\n');
    // A closed pipe (`skilj-agent check | head`) is not an error.
    match std::io::stdout().write_all(text.as_bytes()) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(e.into()),
        _ => Ok(()),
    }
}

/// `writeln!` into the report; writing to a String can't fail.
macro_rules! line {
    ($out:expr) => {{ let _ = writeln!($out); }};
    ($out:expr, $($arg:tt)*) => {{ let _ = writeln!($out, $($arg)*); }};
}

fn render_report(r: &Report, all: bool) -> String {
    let mut out = String::new();
    line!(out, "skilj-agent check: {}", r.project);
    match (&r.skilj_version, r.version_fit) {
        (Some(v), VersionFit::Older) => line!(
            out,
            "skilj {v}: older than the {} this agent knows; upgrading is worth considering",
            r.knowledge_skilj_version
        ),
        (Some(v), VersionFit::Newer) => line!(
            out,
            "skilj {v}: newer than the {} this agent knows; a newer skilj-agent may know more",
            r.knowledge_skilj_version
        ),
        (Some(v), _) => line!(out, "skilj {v}"),
        (None, _) => line!(out, "skilj version unknown (path or git dependency)"),
    }

    line!(out);
    line!(
        out,
        "{:<24} {:>6} {:>8} {:>11} {:>9}  layout",
        "bounded context", "events", "commands", "projections", "snapshots"
    );
    for c in &r.contexts {
        let style = r
            .layout
            .contexts
            .iter()
            .find(|l| l.name == c.name)
            .map(|l| format!("{} ({} lines)", l.style.describe(), l.lines))
            .unwrap_or_default();
        line!(
            out,
            "{:<24} {:>6} {:>8} {:>11} {:>9}  {style}",
            c.name, c.events, c.commands, c.projections, c.snapshots
        );
    }
    match (r.layout.configured, r.layout.detected) {
        (Some(s), _) => line!(out, "layout style: {} (from {})", s.describe(), config::FILE_NAME),
        (None, Some(d)) => line!(
            out,
            "layout style: not set; most contexts are laid out {}. Confirm it with `layout_style = \"{}\"` in {}",
            d.describe(),
            snake_case(&d),
            config::FILE_NAME
        ),
        (None, None) => line!(
            out,
            "layout style: not set, and no style is followed by most contexts. Choose one in {}: by_kind, by_slice or crate_per_context",
            config::FILE_NAME
        ),
    }

    line!(out);
    let covered = |from_spec: bool, level: Option<Level>| {
        let of: Vec<_> = r
            .obligations
            .iter()
            .filter(|o| matches!(o.source, Source::AlliumSpec { .. }) == from_spec)
            .filter(|o| level.is_none_or(|l| o.level == l))
            .collect();
        (of.iter().filter(|o| o.covered()).count(), of.len())
    };
    let (c, t) = covered(false, None);
    let (cu, tu) = covered(false, Some(Level::Unit));
    let (ci, ti) = covered(false, Some(Level::Integration));
    line!(out, "tests: {c} of {t} obligations from the code covered (unit {cu}/{tu}, integration {ci}/{ti})");
    let (cs, ts) = covered(true, None);
    if ts > 0 {
        line!(
            out,
            "       {cs} of {ts} obligations from specs/*.allium covered, counting the ones the compiler guarantees"
        );
    }

    line!(out);
    let dismissed = if r.dismissed > 0 { format!(", {} dismissed", r.dismissed) } else { String::new() };
    line!(out, "findings: {}{dismissed}", r.findings.len());
    let mut by_kind: BTreeMap<FindingKind, Vec<_>> = BTreeMap::new();
    for f in &r.findings {
        by_kind.entry(f.kind).or_default().push(f);
    }
    for (kind, findings) in by_kind {
        line!(out, "  {} ({})", snake_case(&kind), findings.len());
        let shown = if all { findings.len() } else { SHOWN_PER_KIND };
        for f in findings.iter().take(shown) {
            line!(out, "    {}", f.subject);
            line!(out, "        {}", f.detail);
        }
        if findings.len() > shown {
            line!(out, "    ... and {} more (--all lists them)", findings.len() - shown);
        }
    }
    if r.findings.iter().any(|f| f.kind == FindingKind::TooBig) {
        line!(out, "  (a file counts as too big above {} lines)", layout::MAX_CONTEXT_FILE_LINES);
    }

    if !r.warnings.is_empty() {
        line!(out);
        line!(out, "warnings:");
        for w in &r.warnings {
            line!(out, "  {w}");
        }
    }
    out.truncate(out.trim_end().len());
    out
}

/// The name a value has in JSON and in .skilj-agent.toml.
fn snake_case(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}
