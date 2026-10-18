//! skilj-agent: helps working on applications built on skilj. `check`
//! and `diagnose` use no model; `write-test` asks one for a single test
//! and verifies it before showing it.

mod issue;

use clap::{Parser, Subcommand};
use skilj_agent_checks::diagnose::{Diagnosis, Knowledge, MatchedBy};
use skilj_agent_issues::{Occurrence, Suggestion};
use skilj_agent_llm::{ModelConfig, Thinking};
use skilj_agent_tasks::write_test::{self, Progress, RunOutcome};
use skilj_agent_checks::{
    FindingKind, LayoutStyle, Level, ProjectConfig, Report, Source, VersionFit, config, layout,
};
use std::io::Read as _;
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
    /// Recognise skilj errors in output and show the known cause and fix.
    ///
    /// Reads a file, or standard input: `cargo test 2>&1 | skilj-agent diagnose`.
    /// Works on test output, server logs and REST or GraphQL responses.
    Diagnose {
        /// The file holding the output; standard input when left out.
        file: Option<PathBuf>,
        /// Print the diagnosis as JSON, for scripts and other agents.
        #[arg(long)]
        json: bool,
    },
    /// Write a unit test for one untested obligation, with a model.
    ///
    /// The test is built and run in a scratch copy of the project; only a
    /// test that compiles, passes and covers the obligation is shown, and
    /// it's only added to your project when you say so.
    WriteTest {
        /// The obligation, as `check --all` lists it, e.g.
        /// `command.Withdraw.rejected.insufficient_funds`.
        obligation: String,
        /// The project.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// The Ollama model [env: SKILJ_AGENT_MODEL; default: qwen3:14b].
        #[arg(long)]
        model: Option<String>,
        /// When the model reasons before answering. `fallback`: the first
        /// attempt without (fast, enough for simple cases), every retry
        /// with.
        #[arg(long, value_enum, default_value_t = ThinkingArg::Fallback)]
        thinking: ThinkingArg,
        /// Short for `--thinking never`.
        #[arg(long, conflicts_with = "thinking")]
        no_think: bool,
        /// Add the verified test to the project without asking.
        #[arg(long)]
        apply: bool,
        /// Print what the model would be sent, and stop.
        #[arg(long)]
        show_prompt: bool,
        /// Write everything sent to and received from the model to this
        /// file.
        #[arg(long)]
        transcript: Option<PathBuf>,
    },
    /// Offer the suggested issues on skilj-agent that are still waiting.
    ///
    /// Each failure of the agent suggests an issue, so what it can't do yet
    /// becomes knowledge it ships. Nothing is posted without your yes, and
    /// a suggestion never holds your project's code.
    Issues,
    /// Start a new skilj project from the bundled template.
    ///
    /// Copies the template, names the package, and renames the `wallet`
    /// example context to the first context. No model involved.
    New {
        /// Where to create the project.
        path: PathBuf,
        /// The package name, e.g. `my-shop`.
        #[arg(long)]
        name: String,
        /// The first bounded context, e.g. `orders`.
        #[arg(long)]
        context: String,
        /// How contexts are laid out; remembered in `.skilj-agent.toml`.
        #[arg(long, value_enum, default_value_t = StyleArg::BySlice)]
        style: StyleArg,
    },
    /// Split one bounded context into per-style files, verified first.
    ///
    /// Mechanical, no model: moves the context's types into the shape
    /// the project's layout style prescribes, builds the result in a
    /// scratch copy, and only changes your files when you say so.
    SplitContext {
        /// The bounded context to split.
        context: String,
        /// The project.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// The style to split into; defaults to the project's own.
        #[arg(long, value_enum)]
        style: Option<StyleArg>,
        /// Apply without asking.
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum StyleArg {
    ByKind,
    BySlice,
    CratePerContext,
}

impl From<StyleArg> for LayoutStyle {
    fn from(s: StyleArg) -> Self {
        match s {
            StyleArg::ByKind => LayoutStyle::ByKind,
            StyleArg::BySlice => LayoutStyle::BySlice,
            StyleArg::CratePerContext => LayoutStyle::CratePerContext,
        }
    }
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum ThinkingArg {
    Always,
    Never,
    Fallback,
}

impl From<ThinkingArg> for Thinking {
    fn from(t: ThinkingArg) -> Self {
        match t {
            ThinkingArg::Always => Thinking::Always,
            ThinkingArg::Never => Thinking::Never,
            ThinkingArg::Fallback => Thinking::Fallback,
        }
    }
}

/// `writeln!` into the report; writing to a String can't fail.
macro_rules! line {
    ($out:expr) => {{ let _ = writeln!($out); }};
    ($out:expr, $($arg:tt)*) => {{ let _ = writeln!($out, $($arg)*); }};
}

/// Findings shown per kind without --all.
const SHOWN_PER_KIND: usize = 10;

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::Check { path, json, all } => run_check(&path, json, all),
        Command::Diagnose { file, json } => run_diagnose(file.as_deref(), json),
        Command::WriteTest { obligation, path, model, thinking, no_think, apply, show_prompt, transcript } => {
            let mut config = ModelConfig::from_env();
            if let Some(model) = model {
                config.model = model;
            }
            config.thinking = if no_think { Thinking::Never } else { thinking.into() };
            run_write_test(&path, &obligation, &config, apply, show_prompt, transcript.as_deref())
        }
        Command::Issues => issue::run_pending(),
        Command::New { path, name, context, style } => run_new(&path, &name, &context, style.into()),
        Command::SplitContext { context, path, style, apply } => run_split(&path, &context, style.map(LayoutStyle::from), apply),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

fn run_new(path: &Path, name: &str, context: &str, style: LayoutStyle) -> anyhow::Result<()> {
    skilj_agent_new::create(path, name, context, style)?;
    println!("created {name} at {} (first context: {context})", path.display());
    Ok(())
}

/// Splits one context (spec: RestructureContext): plans the move,
/// replays it in a scratch copy, `cargo check`s it there, and applies
/// it to the working tree only on confirmation.
fn run_split(path: &Path, context: &str, style: Option<LayoutStyle>, apply: bool) -> anyhow::Result<()> {
    let index = skilj_agent_index::index(path)?;
    let config = ProjectConfig::load(&index.root)?;
    let style = match style {
        Some(s) => s,
        None => config.layout_style.ok_or_else(|| {
            anyhow::anyhow!(
                "no layout style set; pass --style or choose one in {} (by_kind, by_slice, crate_per_context)",
                config::FILE_NAME
            )
        })?,
    };
    let plan = skilj_agent_split::plan(&index, context, style)?;
    println!("splitting {context} into {} ({}):", plan.dir, style.describe());
    for f in &plan.files {
        println!("  {}", f.path);
    }
    // Verify in a scratch copy first.
    let scratch = skilj_agent_verify::Scratch::of(&index.root)?;
    let scratch_index = skilj_agent_index::index(scratch.path())?;
    let scratch_plan = skilj_agent_split::plan(&scratch_index, context, style)?;
    skilj_agent_split::apply(scratch.path(), &scratch_index, &scratch_plan)?;
    println!("verifying with `cargo check` in a scratch copy...");
    match skilj_agent_verify::check(&scratch)? {
        skilj_agent_verify::CheckOutcome::Passed => {}
        skilj_agent_verify::CheckOutcome::Failed(errors) => {
            println!("the split doesn't compile; nothing was changed. First error:");
            println!("{}", errors.first().map(String::as_str).unwrap_or_default());
            return Ok(());
        }
    }
    if apply || confirm(&format!("Apply the split of {context}?"))? {
        skilj_agent_split::apply(&index.root, &index, &plan)?;
        println!("split {context} into {}.", plan.dir);
    } else {
        println!("Not applied.");
    }
    Ok(())
}

fn run_check(path: &Path, json: bool, all: bool) -> anyhow::Result<()> {
    let index = skilj_agent_index::index(path)?;
    let config = ProjectConfig::load(&index.root)?;
    let report = skilj_agent_checks::check(&index, &config);
    if json {
        print(serde_json::to_string_pretty(&report)?)
    } else {
        print(render_report(&report, all))
    }
}

fn run_diagnose(file: Option<&Path>, json: bool) -> anyhow::Result<()> {
    let output = match file {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?,
        None => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            text
        }
    };
    let diagnosis = skilj_agent_checks::diagnose::diagnose(&Knowledge::bundled(), &output);
    if json {
        return print(serde_json::to_string_pretty(&diagnosis)?);
    }
    print(render_diagnosis(&diagnosis))?;
    for suggestion in diagnosis_suggestions(&diagnosis, &output) {
        issue::suggest(suggestion)?;
    }
    Ok(())
}

/// What `diagnose` couldn't help with: unrecognised output that mentions
/// skilj, and errors without a recorded fix. Only skilj's own error codes
/// go in.
fn diagnosis_suggestions(d: &Diagnosis, output: &str) -> Vec<Suggestion> {
    let occurrence = Occurrence {
        outcome: Some(format!("knowledge of skilj {}", d.knowledge_skilj_version)),
        ..Default::default()
    };
    if d.recognised.is_empty() {
        if !output.to_lowercase().contains("skilj") {
            return Vec::new();
        }
        return vec![Suggestion::new(
            "diagnose/unrecognised",
            "diagnose: skilj output it doesn't recognise",
            "diagnose recognised no skilj error in output that did come from skilj.\n\n\
             The error's code or message, without anything from your project: <add it here>",
            occurrence,
        )];
    }
    let mut out: Vec<Suggestion> = Vec::new();
    for r in &d.recognised {
        if let (None, Some(e)) = (&r.fix, &r.error)
            && !out.iter().any(|s| s.key.ends_with(&format!("/{}", e.code)))
        {
            out.push(Suggestion::new(
                format!("diagnose/no-fix/{}", e.code),
                format!("diagnose: no cause or fix for skilj's `{}`", e.code),
                format!(
                    "skilj's {} error `{}` ({}) has no recorded cause or fix in knowledge/error-fixes.toml yet.\n\n\
                     What caused it and what fixed it, if you know: <add it here>",
                    e.module, e.code, e.variant
                ),
                occurrence.clone(),
            ));
        }
    }
    out
}

fn run_write_test(
    path: &Path,
    obligation: &str,
    config: &ModelConfig,
    apply: bool,
    show_prompt: bool,
    transcript: Option<&Path>,
) -> anyhow::Result<()> {
    let index = skilj_agent_index::index(path)?;
    let project_config = ProjectConfig::load(&index.root)?;
    let plan = write_test::plan(&index, &project_config, obligation)?;
    let kind = obligation.split('.').next().unwrap_or("unit");
    let occurrence = |outcome: String| Occurrence {
        skilj: index.skilj_version.clone(),
        model: Some(format!("{}, {}", config.model, config.thinking.describe())),
        outcome: Some(outcome),
    };
    if show_prompt {
        return print(format!(
            "--- system ---\n{}\n--- task ---\n{}",
            plan.system_prompt(),
            plan.task_prompt()
        ));
    }
    eprintln!("{}", plan.goal_text());
    eprintln!(
        "context: {} lines of code{}; the test goes into {}",
        plan.context_lines(),
        if plan.example.is_some() { " and an example test" } else { "" },
        match &plan.placement {
            write_test::Placement::Module { name, .. } => format!("mod {name} in {}", plan.file),
            write_test::Placement::NewModule { name } => format!("a new mod {name} in {}", plan.file),
        }
    );
    eprintln!("model: {}, {}", config.describe(), config.thinking.describe());

    let mut log = String::new();
    if transcript.is_some() {
        line!(log, "=== system\n{}", plan.system_prompt());
    }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let outcome = runtime.block_on(write_test::run(&index, &plan, config, |p| match p {
        Progress::Asking { attempt, think, message } => {
            if transcript.is_some() {
                line!(log, "=== attempt {attempt} ({}): to the model\n{message}", if think { "thinking" } else { "not thinking" });
            }
            eprint!(
                "attempt {attempt}/{}: asking the model{}... ",
                write_test::attempts(config.thinking),
                if think { "" } else { ", without thinking" }
            )
        }
        Progress::Answered { reply, .. } => {
            if transcript.is_some() {
                line!(log, "=== from the model\n{}", reply.text);
            }
            eprintln!(
            "answered in {:.0}s ({} tokens in, {} out)",
            reply.took.as_secs_f64(),
            reply.input_tokens,
            reply.output_tokens
        )
        }
        Progress::Building => eprintln!("  covers the obligation; building and running it in a scratch copy..."),
        Progress::Problem { problem, .. } => eprintln!("  {}", problem.summary()),
    }));
    if let Some(path) = transcript {
        std::fs::write(path, &log).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    }
    let outcome = outcome?;

    match outcome {
        RunOutcome::Verified(proposal) => {
            let mut out = String::new();
            line!(out);
            line!(
                out,
                "Verified after {} attempt{}: it compiles, passes, and covers {obligation}.",
                proposal.attempts,
                if proposal.attempts == 1 { "" } else { "s" }
            );
            line!(out, "It goes into {}:", proposal.where_);
            line!(out);
            line!(out, "{}", proposal.test);
            print(out)?;
            if apply || confirm("Add it to your project?")? {
                proposal.apply(&index.root)?;
                eprintln!("Added {} to {}.", proposal.test_name, proposal.file);
                return Ok(());
            }
            eprintln!("Not added.");
            // Discarding a verified test means it is wrong in a way the
            // compiler couldn't see (spec: DiscardVerifiedProposal).
            if let Some(why) = ask_line("What's wrong with it? One line, or Enter to skip:")? {
                issue::suggest(Suggestion::new(
                    format!("write-test/discarded/{kind}"),
                    format!("write-test: verified {kind} tests that are wrong"),
                    format!(
                        "write-test proposed a {kind} test that compiled, passed and covered the obligation, but was \
                         wrong. In the developer's words:\n\n> {why}"
                    ),
                    occurrence(format!("verified after {} attempts", proposal.attempts)),
                ))?;
            }
            Ok(())
        }
        RunOutcome::GaveUp { attempts, last, unreachable } => {
            eprintln!();
            eprintln!("Gave up after {attempts} attempts; the last one: {}", last.summary());
            let outcome = occurrence(format!("gave up after {attempts} attempts"));
            let suggestion = match &unreachable {
                Some(hint) => {
                    eprintln!();
                    eprintln!("{hint}");
                    if plan.via.is_empty() {
                        Suggestion::new(
                            format!("write-test/gave-up/{kind}/rejected-earlier"),
                            "write-test: every test for a rejection was rejected earlier",
                            "write-test gave up on a rejection: every test that ran was rejected with another of \
                             the command's kinds. The rejection may be unreachable, or the model couldn't find input \
                             that gets past the earlier checks.",
                            outcome,
                        )
                    } else {
                        Suggestion::new(
                            "check/inherited-rejection-may-be-unreachable",
                            "check: a rejection reached only through another command's logic may be unreachable",
                            "check counted a rejection kind that a command only reaches by calling another \
                             command's logic (`Other::decide_on`), and write-test couldn't make the command reject \
                             with it. The calling command may pass arguments that rule it out, which the index \
                             can't see.",
                            outcome,
                        )
                    }
                }
                None => Suggestion::new(
                    format!("write-test/gave-up/{kind}/{}", last.category().replace(' ', "-")),
                    format!("write-test gives up on {kind} obligations: {}", last.category()),
                    format!("write-test gave up on a {kind} obligation; the last problem: {}.", last.category()),
                    outcome,
                ),
            };
            issue::suggest(suggestion)
        }
        RunOutcome::ProjectBroken(errors) => {
            eprintln!();
            eprintln!("The project doesn't build as it is, so no test can be verified. First error:");
            eprintln!("{}", errors.first().map(String::as_str).unwrap_or_default());
            Ok(())
        }
    }
}

/// Asks on a terminal; anything but yes, or no terminal, is no.
/// A line typed in answer, when there is a terminal and it isn't empty.
fn ask_line(question: &str) -> anyhow::Result<Option<String>> {
    use std::io::IsTerminal as _;
    if !std::io::stdin().is_terminal() {
        return Ok(None);
    }
    eprint!("{question} ");
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    let answer = answer.trim();
    Ok((!answer.is_empty()).then(|| answer.to_string()))
}

fn confirm(question: &str) -> anyhow::Result<bool> {
    use std::io::IsTerminal as _;
    if !std::io::stdin().is_terminal() {
        return Ok(false);
    }
    eprint!("{question} [y/N] ");
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

fn print(mut text: String) -> anyhow::Result<()> {
    text.push('\n');
    // A closed pipe (`skilj-agent check | head`) is not an error.
    match std::io::stdout().write_all(text.as_bytes()) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(e.into()),
        _ => Ok(()),
    }
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

fn render_diagnosis(d: &Diagnosis) -> String {
    let mut out = String::new();
    if d.recognised.is_empty() {
        line!(out, "No skilj error recognised (knowledge of skilj {}).", d.knowledge_skilj_version);
        return out;
    }
    for (i, r) in d.recognised.iter().enumerate() {
        if i > 0 {
            line!(out);
        }
        match &r.error {
            Some(e) => {
                let when = r.when.as_ref().map(|w| format!(", at {w}")).unwrap_or_default();
                line!(out, "line {}: {} ({} error `{}`{when})", r.line, e.variant, e.module, e.code);
            }
            None => line!(out, "line {}: not an error, but worth knowing", r.line),
        }
        line!(out, "  > {}", r.line_text);
        if r.matched_by == MatchedBy::Variant {
            line!(out, "  (recognised by its name only)");
        }
        match (&r.cause, &r.fix, &r.error) {
            (Some(cause), Some(fix), _) => {
                line!(out, "  cause: {cause}");
                line!(out, "  fix:   {fix}");
            }
            (_, _, Some(e)) => {
                line!(out, "  skilj says: {}", e.message);
                line!(out, "  No cause or fix is recorded for this error yet.");
            }
            _ => {}
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
