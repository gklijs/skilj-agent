//! Writing one unit test for one untested obligation (spec: WriteTest,
//! VerifyProposal, ProposalRetries, ProposalGivesUp).
//!
//! The code decides everything around the test: which obligation, what
//! code the model sees, where the test goes. The model writes one
//! `#[test]` function. An answer is only a proposal once
//!
//! 1. it is one `#[test]` function with a name that's free,
//! 2. the index, rerun on the changed copy, counts it as covering the
//!    obligation (the same matching `check` uses), and
//! 3. `cargo test` in a scratch copy compiles it and it passes.
//!
//! Anything that fails goes back to the model, up to `MAX_ATTEMPTS`.

use anyhow::{Context as _, bail};
use skilj_agent_checks::{Level, Source, code_obligations};
use skilj_agent_index::source::{self, ItemKind, SourceItem};
use skilj_agent_index::{DeclaredIn, FileKind, PluginType, ProjectIndex, TestLevel, TypeKind};
use skilj_agent_llm::{Conversation, ModelConfig, Reply, code_block};
use skilj_agent_verify::{Outcome, Scratch, Target, Touched};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Spec: config.max_attempts.
pub const MAX_ATTEMPTS: usize = 3;
/// The code shown besides the type itself, in lines.
const CONTEXT_BUDGET: usize = 500;
/// How deep references are followed from the type.
const CONTEXT_DEPTH: usize = 3;
const MAX_ERRORS_SHOWN: usize = 3;
const MAX_ERROR_LINES: usize = 40;

const GUIDE: &str = include_str!("../../../knowledge/testing.md");

/// What one obligation asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Goal {
    Accepted,
    Rejected(String),
    Folds(String),
}

/// Everything decided before the model is asked.
#[derive(Debug)]
pub struct Plan {
    pub obligation: String,
    pub goal: Goal,
    pub type_name: String,
    pub rust_name: String,
    /// The function a test calls: `decide`, or `decide_with` /
    /// `decide_from_snapshot` when the command overrides it; `project`.
    pub call: String,
    pub kind: TypeKind,
    /// Rejection kinds of the command, for the accepted path.
    pub rejection_kinds: BTreeSet<String>,
    /// Where the test goes, relative to the project root.
    pub file: String,
    pub package: String,
    pub placement: Placement,
    /// The code the model sees, by file.
    pub context: Vec<Excerpt>,
    pub example: Option<Excerpt>,
    /// Names already taken in the module the test goes into.
    pub taken: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub enum Placement {
    /// Before the closing brace (`line`) of an existing `#[cfg(test)]`
    /// module.
    Module { name: String, line: usize, indent: String },
    /// A new `#[cfg(test)] mod <name>` at the end of the file.
    NewModule { name: String },
}

impl Placement {
    fn describe(&self, file: &str) -> String {
        match self {
            Placement::Module { name, .. } => format!("the existing `#[cfg(test)] mod {name}` in {file}"),
            Placement::NewModule { name } => {
                format!("a new `#[cfg(test)] mod {name} {{ use super::*; ... }}` at the end of {file}")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Excerpt {
    pub title: String,
    pub code: String,
    pub lines: usize,
}

/// What the run reports while it works.
#[derive(Debug)]
pub enum Progress<'a> {
    Asking { attempt: usize, message: &'a str },
    Answered { attempt: usize, reply: &'a Reply },
    Building,
    Problem { attempt: usize, problem: &'a Problem },
}

/// Why an answer isn't a proposal yet; the text goes back to the model.
#[derive(Debug, Clone)]
pub enum Problem {
    /// The answer hit the token or time limit.
    CutOff,
    NotOneTest(String),
    NameTaken(String),
    DoesNotCover(String),
    CompileErrors(Vec<String>),
    TestFailed(String),
    TestNotRun,
}

impl Problem {
    /// One line for the developer.
    pub fn summary(&self) -> String {
        match self {
            Problem::CutOff => "the answer was cut off: too long, or took too long".into(),
            Problem::NotOneTest(why) => format!("not a single test function: {why}"),
            Problem::NameTaken(name) => format!("the name `{name}` is already taken"),
            Problem::DoesNotCover(_) => "doesn't test what was asked".into(),
            Problem::CompileErrors(e) => {
                let first = e.first().and_then(|e| e.lines().next()).unwrap_or_default();
                format!("doesn't compile ({} error{}): {first}", e.len(), if e.len() == 1 { "" } else { "s" })
            }
            Problem::TestFailed(out) => {
                let first = out.lines().find(|l| !l.trim().is_empty()).unwrap_or_default();
                format!("the test fails: {first}")
            }
            Problem::TestNotRun => "cargo ran no test by that name".into(),
        }
    }

    /// A short category for an issue: never project code.
    pub fn category(&self) -> &'static str {
        match self {
            Problem::CutOff => "answer was cut off",
            Problem::NotOneTest(_) => "answer was not one test function",
            Problem::NameTaken(_) => "test name already taken",
            Problem::DoesNotCover(_) => "test did not cover the obligation",
            Problem::CompileErrors(_) => "test did not compile",
            Problem::TestFailed(_) => "test failed",
            Problem::TestNotRun => "test did not run",
        }
    }

    fn feedback(&self) -> String {
        let fix = "Answer with the corrected test function only, in one ```rust block.";
        match self {
            Problem::CutOff => format!("Your answer was cut off before the test was complete. Think briefly, then write the test.\n{fix}"),
            Problem::NotOneTest(why) => format!("{why}\n{fix}"),
            Problem::NameTaken(name) => {
                format!("A function named `{name}` already exists in that module. Choose another name.\n{fix}")
            }
            Problem::DoesNotCover(what) => format!("{what}\n{fix}"),
            Problem::CompileErrors(errors) => {
                let shown: Vec<_> = errors.iter().take(MAX_ERRORS_SHOWN).map(|e| head(e, MAX_ERROR_LINES)).collect();
                format!("It doesn't compile:\n\n{}\n\n{fix}", shown.join("\n\n"))
            }
            Problem::TestFailed(out) => format!(
                "It compiles, but the test fails:\n\n{}\n\nRead decide()/project() again and fix the test's input or its assertion so it checks what the code does.\n{fix}",
                head(out, MAX_ERROR_LINES)
            ),
            Problem::TestNotRun => format!("cargo ran no test with that name. Make sure it has `#[test]`.\n{fix}"),
        }
    }
}

#[derive(Debug)]
pub enum RunOutcome {
    /// Verified: compiles, passes, covers the obligation.
    Verified(Proposal),
    /// Every attempt had a problem; the last one.
    GaveUp { attempts: usize, last: Problem },
    /// The project doesn't build as it is: nothing the model can fix.
    ProjectBroken(Vec<String>),
}

#[derive(Debug)]
pub struct Proposal {
    pub file: String,
    pub test_name: String,
    /// The test as inserted, indented.
    pub test: String,
    pub where_: String,
    pub attempts: usize,
    original: String,
    changed: String,
}

impl Proposal {
    /// Writes the test into the project (spec: ApplyProposal). Refuses
    /// when the file changed since the proposal was made.
    pub fn apply(&self, root: &Path) -> anyhow::Result<()> {
        let path = root.join(&self.file);
        let now = std::fs::read_to_string(&path).with_context(|| self.file.clone())?;
        if now != self.original {
            bail!("{} changed since the test was written; run write-test again", self.file);
        }
        std::fs::write(&path, &self.changed).with_context(|| self.file.clone())
    }
}

/// Picks the obligation's type, the code to show and where the test
/// goes. No model involved.
pub fn plan(index: &ProjectIndex, obligation: &str) -> anyhow::Result<Plan> {
    let found = code_obligations(index).into_iter().find(|o| o.id == obligation);
    let Some(found) = found else {
        bail!("no obligation `{obligation}` in this project; `skilj-agent check --all` lists them");
    };
    if !matches!(found.source, Source::CodeIndex) || found.level != Level::Unit {
        bail!("`{obligation}` is not a unit test obligation; only those can be written for now");
    }
    if let Some(by) = found.covered_by.first() {
        bail!("`{obligation}` is already covered by {by}");
    }
    let (kind, type_name, goal) = parse_obligation(obligation)?;
    let ty = index
        .types
        .iter()
        .find(|t| t.kind == kind && t.name == type_name)
        .with_context(|| format!("no {kind:?} named {type_name}"))?;

    let file = test_file(index, ty)?;
    let package = index
        .files
        .iter()
        .find(|f| f.path == file)
        .map(|f| f.package.clone())
        .with_context(|| format!("{file} is not in the index"))?;

    let sources = read_package(index, &package)?;
    let call = match kind {
        TypeKind::Command => {
            let methods: BTreeSet<&str> = sources
                .values()
                .flat_map(|(_, items)| items)
                .filter(|i| i.kind == ItemKind::Impl && i.trait_impl && i.name == ty.rust_name)
                .flat_map(|i| i.methods.iter().map(|m| m.0.as_str()))
                .collect();
            ["decide_with", "decide_from_snapshot"]
                .into_iter()
                .find(|m| methods.contains(m))
                .unwrap_or("decide")
                .to_string()
        }
        _ => "project".to_string(),
    };
    let context = select_context(index, ty, &goal, &sources);

    let (example, example_module) = pick_example(index, ty, &file, &sources);
    let target_items = &sources.get(&file).context("the test file is not readable")?.1;
    let placement = placement(&sources[&file].0, target_items, example_module.as_deref());
    let module = match &placement {
        Placement::Module { name, .. } | Placement::NewModule { name } => name.clone(),
    };
    let taken = target_items
        .iter()
        .filter(|i| i.test_module.as_deref() == Some(module.as_str()))
        .map(|i| i.name.clone())
        .collect();

    Ok(Plan {
        obligation: obligation.to_string(),
        goal,
        type_name: ty.name.clone(),
        rust_name: ty.rust_name.clone(),
        call,
        kind,
        rejection_kinds: ty.rejection_kinds.clone(),
        file,
        package,
        placement,
        context,
        example,
        taken,
    })
}

fn parse_obligation(id: &str) -> anyhow::Result<(TypeKind, String, Goal)> {
    let parts: Vec<&str> = id.splitn(4, '.').collect();
    match parts.as_slice() {
        ["command", name, "accepted"] => Ok((TypeKind::Command, name.to_string(), Goal::Accepted)),
        ["command", name, "rejected", kind] => Ok((TypeKind::Command, name.to_string(), Goal::Rejected(kind.to_string()))),
        ["projection", name, "folds", event] => {
            Ok((TypeKind::Projection, name.to_string(), Goal::Folds(event.to_string())))
        }
        _ => bail!("`{id}` is not an obligation write-test knows how to write"),
    }
}

/// The hand-written file of the type: for a codegen type, the `.rs`
/// next to its `.skilj.toml`, where `decide_<name>` lives.
fn test_file(index: &ProjectIndex, ty: &PluginType) -> anyhow::Result<String> {
    match ty.declared_in {
        DeclaredIn::Rust => Ok(ty.file.clone()),
        DeclaredIn::SkiljToml => {
            let rs = format!("{}.rs", ty.file.trim_end_matches(".skilj.toml"));
            if index.files.iter().any(|f| f.path == rs) {
                Ok(rs)
            } else {
                bail!("{} is declared in {}, but there is no {rs} next to it", ty.rust_name, ty.file)
            }
        }
    }
}

type Sources = BTreeMap<String, (String, Vec<SourceItem>)>;

fn read_package(index: &ProjectIndex, package: &str) -> anyhow::Result<Sources> {
    let mut out = BTreeMap::new();
    for f in index.files.iter().filter(|f| f.package == package && f.kind == FileKind::Src) {
        let text = std::fs::read_to_string(index.root.join(&f.path)).with_context(|| f.path.clone())?;
        if let Ok(items) = source::items(&text) {
            out.insert(f.path.clone(), (text, items));
        }
    }
    Ok(out)
}

/// One piece of shown code: a whole item, or one method of an impl.
type Piece = (String, usize, Option<usize>); // (file, item index, method index)

/// The type's own code, then what it refers to, following names through
/// the package up to `CONTEXT_DEPTH` steps and `CONTEXT_BUDGET` lines:
/// items by name, and methods of the package's own impls by method name.
/// An enum's variants pull in only the payloads of variants that are
/// used, so a large event enum doesn't bring every payload with it. For
/// a rejection, the code holding its kind is always shown.
fn select_context(index: &ProjectIndex, ty: &PluginType, goal: &Goal, sources: &Sources) -> Vec<Excerpt> {
    let mut chosen: BTreeSet<Piece> = BTreeSet::new();
    let mut excerpts = Vec::new();

    if ty.declared_in == DeclaredIn::SkiljToml
        && let Ok(text) = std::fs::read_to_string(index.root.join(&ty.file))
    {
        excerpts.push(Excerpt {
            title: format!("{} (skilj-codegen generates the payload structs, the event enum and the impls from it)", ty.file),
            lines: text.lines().count(),
            code: text,
        });
    }

    let decide_fn = skilj_agent_index::decide_fn_name(&ty.name);
    let mut seeds: Vec<Piece> = Vec::new();
    for (file, (_, items)) in sources {
        for (i, item) in items.iter().enumerate() {
            if item.test_module.is_some() {
                continue;
            }
            let own = (item.name == ty.rust_name && matches!(item.kind, ItemKind::Impl | ItemKind::Struct))
                || (ty.declared_in == DeclaredIn::SkiljToml && item.kind == ItemKind::Fn && item.name == decide_fn);
            if own {
                seeds.push((file.clone(), i, None));
            }
        }
    }
    let home: BTreeSet<String> = seeds.iter().map(|(f, _, _)| f.clone()).collect();
    if let Goal::Rejected(kind) = goal
        && let Some(piece) = holding_literal(sources, &home, kind)
        && !seeds.iter().any(|s| s.0 == piece.0 && s.1 == piece.1)
    {
        seeds.push(piece);
    }
    let mut idents = BTreeSet::new();
    if let Goal::Folds(event) = goal {
        idents.insert(event.clone());
        if let Some(e) = index.types.iter().find(|t| t.kind == TypeKind::Event && &t.name == event) {
            idents.insert(e.rust_name.clone());
        }
    }
    let context_files: BTreeSet<String> = index
        .contexts
        .iter()
        .filter(|c| c.name == ty.context)
        .flat_map(|c| c.files.iter().cloned().chain([c.declared_in.clone()]))
        .collect();
    for seed in &seeds {
        idents.extend(match seed.2 {
            None => seed_idents(sources, &(seed.0.clone(), seed.1)),
            Some(_) => piece_idents(sources, seed),
        });
        chosen.insert(seed.clone());
    }

    let mut used = 0;
    let mut enums: Vec<(String, usize)> = Vec::new();
    for _ in 0..CONTEXT_DEPTH {
        // Payloads of enum variants that are named somewhere.
        for e in &enums {
            for (variant, fields) in &sources[&e.0].1[e.1].variants {
                if idents.contains(variant) {
                    idents.extend(fields.iter().cloned());
                }
            }
        }
        let mut next: Vec<Piece> = Vec::new();
        for (file, (_, items)) in sources {
            // A binary's code is invisible to the library and to other
            // binaries.
            if is_bin(file) && !home.iter().any(|h| same_bin(h, file)) {
                continue;
            }
            for (i, item) in items.iter().enumerate() {
                if item.test_module.is_some() || chosen.contains(&(file.clone(), i, None)) {
                    continue;
                }
                let by_name = matches!(
                    item.kind,
                    ItemKind::Struct | ItemKind::Enum | ItemKind::Fn | ItemKind::Const | ItemKind::TypeAlias
                ) && idents.contains(&item.name);
                if by_name {
                    next.push((file.clone(), i, None));
                }
                // A method only when its type is in play too: method
                // names like `new` or `of` are on many types.
                if item.kind == ItemKind::Impl && !item.trait_impl && idents.contains(&item.name) {
                    for (m, (name, _, _)) in item.methods.iter().enumerate() {
                        let piece = (file.clone(), i, Some(m));
                        if idents.contains(name) && !chosen.contains(&piece) {
                            next.push(piece);
                        }
                    }
                }
            }
        }
        // A name defined in the type's own file means that one, the way
        // Rust resolves it; other files only for names it doesn't define.
        // Not defined there: prefer the files of the same bounded context
        // (a `mod.rs` the file inherits from with `use super::*`).
        let name = |p: &Piece| piece_name(sources, p).to_string();
        let at_home: BTreeSet<String> = next.iter().filter(|p| home.contains(&p.0)).map(name).collect();
        next.retain(|p| home.contains(&p.0) || (!at_home.contains(&name(p)) && !home_defines(sources, &home, &name(p))));
        let in_context: BTreeSet<String> = next.iter().filter(|p| context_files.contains(&p.0)).map(name).collect();
        next.retain(|p| context_files.contains(&p.0) || !in_context.contains(&name(p)));
        // A name already shown is resolved: don't add other definitions
        // of it in later rounds.
        let shown: BTreeSet<String> = chosen.iter().map(|p| qualified_name(sources, p)).collect();
        next.retain(|p| !shown.contains(&qualified_name(sources, p)));
        if next.is_empty() {
            break;
        }
        for piece in next {
            let (start, end) = piece_lines(sources, &piece);
            if used + end + 1 - start > CONTEXT_BUDGET {
                continue;
            }
            used += end + 1 - start;
            let item = &sources[&piece.0].1[piece.1];
            if item.kind == ItemKind::Enum {
                enums.push((piece.0.clone(), piece.1));
            } else {
                idents.extend(piece_idents(sources, &piece));
            }
            chosen.insert(piece);
        }
    }
    // Variants named by the last round.
    for e in &enums {
        for (variant, fields) in &sources[&e.0].1[e.1].variants {
            if idents.contains(variant) {
                for (file, (_, items)) in sources {
                    for (i, item) in items.iter().enumerate() {
                        if item.kind == ItemKind::Struct && item.test_module.is_none() && fields.contains(&item.name) {
                            chosen.insert((file.clone(), i, None));
                        }
                    }
                }
            }
        }
    }
    render(sources, &chosen, &mut excerpts);
    excerpts
}

fn is_bin(file: &str) -> bool {
    file.contains("src/bin/") || file.ends_with("src/main.rs")
}

/// Whether two files are in the same binary target.
fn same_bin(a: &str, b: &str) -> bool {
    let target = |f: &str| match f.find("src/bin/") {
        Some(at) => {
            let rest = &f[at + "src/bin/".len()..];
            f[..at].to_string() + rest.split('/').next().unwrap_or(rest).trim_end_matches(".rs")
        }
        None => f.to_string(),
    };
    is_bin(a) && target(a) == target(b)
}

/// The function, or method, whose body holds `"literal"`: preferring
/// the type's own file.
fn holding_literal(sources: &Sources, home: &BTreeSet<String>, literal: &str) -> Option<Piece> {
    let needle = format!("\"{literal}\"");
    let mut found = Vec::new();
    for (file, (text, items)) in sources {
        let lines: Vec<&str> = text.lines().collect();
        let holds = |start: usize, end: usize| lines[start - 1..end.min(lines.len())].iter().any(|l| l.contains(&needle));
        for (i, item) in items.iter().enumerate() {
            if item.test_module.is_some() {
                continue;
            }
            match item.kind {
                ItemKind::Fn | ItemKind::Const if holds(item.start, item.end) => found.push((file.clone(), i, None)),
                ItemKind::Impl => {
                    for (m, (_, start, end)) in item.methods.iter().enumerate() {
                        if holds(*start, *end) {
                            found.push((file.clone(), i, Some(m)));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    found.iter().find(|p| home.contains(&p.0)).or(found.first()).cloned()
}

fn piece_name<'a>(sources: &'a Sources, p: &Piece) -> &'a str {
    let item = &sources[&p.0].1[p.1];
    match p.2 {
        Some(m) => &item.methods[m].0,
        None => &item.name,
    }
}

/// `Type::method` for a method, the item's name otherwise.
fn qualified_name(sources: &Sources, p: &Piece) -> String {
    let item = &sources[&p.0].1[p.1];
    match p.2 {
        Some(m) => format!("{}::{}", item.name, item.methods[m].0),
        None if item.kind == ItemKind::Impl => format!("impl {}", item.name),
        None => item.name.clone(),
    }
}

fn piece_lines(sources: &Sources, p: &Piece) -> (usize, usize) {
    let item = &sources[&p.0].1[p.1];
    match p.2 {
        Some(m) => (item.methods[m].1, item.methods[m].2),
        None => (item.start, item.end),
    }
}

fn piece_idents(sources: &Sources, p: &Piece) -> BTreeSet<String> {
    let (start, end) = piece_lines(sources, p);
    source::idents_in(&source::lines(&sources[&p.0].0, start, end))
}

/// The chosen pieces by file, in source order; methods inside their
/// impl's first line, so the model sees what type they belong to.
fn render(sources: &Sources, chosen: &BTreeSet<Piece>, excerpts: &mut Vec<Excerpt>) {
    let mut by_file: BTreeMap<&String, Vec<&Piece>> = BTreeMap::new();
    for piece in chosen {
        by_file.entry(&piece.0).or_default().push(piece);
    }
    for (file, mut pieces) in by_file {
        let (text, items) = &sources[file];
        pieces.sort_by_key(|p| piece_lines(sources, p));
        let mut parts: Vec<String> = Vec::new();
        let mut lines = 0;
        let mut last_end = 0;
        let mut open_impl: Option<usize> = None;
        for p in pieces {
            let (start, end) = piece_lines(sources, p);
            if start <= last_end {
                continue; // inside something already shown
            }
            match p.2 {
                None => {
                    if open_impl.take().is_some() {
                        parts.last_mut().expect("an open impl").push_str("\n}");
                    }
                    parts.push(source::lines(text, start, end));
                }
                Some(_) => {
                    let method = source::lines(text, start, end);
                    if open_impl == Some(p.1) {
                        let last = parts.last_mut().expect("an open impl");
                        last.push_str("\n\n    // ...\n\n");
                        last.push_str(&method);
                    } else {
                        if open_impl.take().is_some() {
                            parts.last_mut().expect("an open impl").push_str("\n}");
                        }
                        let item = &items[p.1];
                        let header = text
                            .lines()
                            .skip(item.start - 1)
                            .take(item.end + 1 - item.start)
                            .find(|l| l.trim_start().starts_with("impl"))
                            .unwrap_or("impl {");
                        parts.push(format!("{header}\n    // ...\n\n{method}"));
                        open_impl = Some(p.1);
                        lines += 1;
                    }
                }
            }
            lines += end + 1 - start;
            last_end = end;
        }
        if open_impl.is_some() {
            parts.last_mut().expect("an open impl").push_str("\n}");
        }
        excerpts.push(Excerpt { title: file.clone(), code: parts.join("\n\n// ...\n\n"), lines });
    }
}

/// The methods of a plugin trait impl worth following: the logic. The
/// declarative ones (`tag_mappings`, `consistency_query`, ...) only pull
/// in code a unit test doesn't need.
const LOGIC_METHODS: [&str; 5] = ["decide", "decide_with", "decide_from_snapshot", "project", "keys"];

/// A seed's identifiers, leaving out the bodies of a trait impl's
/// declarative methods.
fn seed_idents(sources: &Sources, key: &(String, usize)) -> BTreeSet<String> {
    let (text, items) = &sources[&key.0];
    let item = &items[key.1];
    if !item.trait_impl {
        return item_idents(sources, key);
    }
    let skipped: Vec<(usize, usize)> = item
        .methods
        .iter()
        .filter(|(name, _, _)| !LOGIC_METHODS.contains(&name.as_str()))
        .map(|(_, start, end)| (*start, *end))
        .collect();
    let kept: Vec<&str> = text
        .lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l))
        .filter(|(n, _)| (item.start..=item.end).contains(n) && !skipped.iter().any(|(s, e)| (s..=e).contains(&n)))
        .map(|(_, l)| l)
        .collect();
    // Whole methods are cut out, so the rest still tokenizes.
    source::idents_in(&kept.join("\n"))
}

fn home_defines(sources: &Sources, home: &BTreeSet<String>, name: &str) -> bool {
    home.iter().any(|f| sources[f].1.iter().any(|i| i.test_module.is_none() && i.name == name && i.kind != ItemKind::Impl))
}

fn item_idents(sources: &Sources, key: &(String, usize)) -> BTreeSet<String> {
    let (text, items) = &sources[&key.0];
    let item = &items[key.1];
    source::idents_in(&source::lines(text, item.start, item.end))
}

/// A unit test from the project to imitate: one of this type if there
/// is one, else one of the same kind, preferring the test file. Shown
/// with the helpers from its module it uses. Returns the module name
/// when the example is in the test file itself.
fn pick_example(index: &ProjectIndex, ty: &PluginType, file: &str, sources: &Sources) -> (Option<Excerpt>, Option<String>) {
    let same_kind = |t: &skilj_agent_index::TestFn| {
        t.type_calls.iter().any(|c| match ty.kind {
            TypeKind::Command => c.ends_with("::decide") || c.ends_with("::decide_with") || c.ends_with("::decide_from_snapshot"),
            _ => c.ends_with("::project"),
        })
    };
    let units: Vec<_> = index.tests.iter().filter(|t| t.level == TestLevel::Unit).collect();
    let best = units
        .iter()
        .find(|t| match ty.kind {
            TypeKind::Command => t.decides(ty),
            _ => t.folds(ty),
        })
        .or_else(|| units.iter().find(|t| same_kind(t) && t.file == file))
        .or_else(|| units.iter().find(|t| same_kind(t)));
    let Some(test) = best else { return (None, None) };
    let Some((text, items)) = sources.get(&test.file) else { return (None, None) };
    let Some(item) = items.iter().find(|i| i.kind == ItemKind::Fn && i.name == test.name && i.test_module.is_some()) else {
        return (None, None);
    };
    let module = item.test_module.clone();
    let mut code = source::lines(text, item.start, item.end);
    let used = source::idents_in(&code);
    let helpers: Vec<_> = items
        .iter()
        .filter(|i| i.test_module == module && i.kind == ItemKind::Fn && i.name != test.name && used.contains(&i.name))
        .map(|i| source::lines(text, i.start, i.end))
        .collect();
    if !helpers.is_empty() {
        code = format!("{}\n\n{code}", helpers.join("\n\n"));
    }
    let in_target = test.file == file;
    let title = format!(
        "{}, in mod {}{}",
        test.file,
        module.as_deref().unwrap_or("?"),
        if in_target { " (the module your test goes into: you can use its helpers)" } else { " (another file: its helpers are not available to you)" }
    );
    let lines = code.lines().count();
    (Some(Excerpt { title, code, lines }), if in_target { module } else { None })
}

fn placement(text: &str, items: &[SourceItem], preferred: Option<&str>) -> Placement {
    let modules: Vec<_> = items.iter().filter(|i| i.kind == ItemKind::TestModule).collect();
    let module = preferred
        .and_then(|p| modules.iter().find(|m| m.name == p))
        .or_else(|| modules.iter().find(|m| m.test_module.is_none()));
    match module {
        Some(m) => {
            let closing = text.lines().nth(m.end - 1).unwrap_or("}");
            let indent: String = closing.chars().take_while(|c| c.is_whitespace()).collect();
            Placement::Module { name: m.name.clone(), line: m.end, indent: format!("{indent}    ") }
        }
        None => {
            let name = if items.iter().any(|i| i.name == "tests") { "skilj_agent_tests" } else { "tests" };
            Placement::NewModule { name: name.into() }
        }
    }
}

impl Plan {
    pub fn system_prompt(&self) -> String {
        format!(
            "You write one Rust unit test for an application built on skilj, an event-sourcing library.\n\
             Answer with exactly one ```rust code block holding exactly one #[test] function, and nothing else.\n\
             No `use` lines outside the function, no helper functions outside it, no explanation.\n\n{GUIDE}"
        )
    }

    pub fn goal_text(&self) -> String {
        match &self.goal {
            Goal::Accepted => format!(
                "Write a unit test showing that command `{}` (Rust type `{}`) accepts valid input: call `{}::{}(...)` and check the decision is `CommandDecision::Accepted`, with the events it returns.",
                self.type_name, self.rust_name, self.rust_name, self.call
            ),
            Goal::Rejected(kind) => format!(
                "Write a unit test showing that command `{}` (Rust type `{}`) rejects with kind \"{kind}\": call `{}::{}(...)` with input that triggers exactly that rejection, and assert the kind is \"{kind}\".",
                self.type_name, self.rust_name, self.rust_name, self.call
            ),
            Goal::Folds(event) => format!(
                "Write a unit test showing how projection `{}` (Rust type `{}`) folds the event `{event}`: call `{}::project(...)` with a `{event}` event and assert on the resulting state.",
                self.type_name, self.rust_name, self.rust_name
            ),
        }
    }

    pub fn task_prompt(&self) -> String {
        let mut out = format!("{}\n\n", self.goal_text());
        out.push_str("## The code\n\n");
        for e in &self.context {
            out.push_str(&format!("// {}\n```rust\n{}\n```\n\n", e.title, e.code));
        }
        if let Some(e) = &self.example {
            out.push_str(&format!("## An existing test in this project, to follow its style\n\n// {}\n```rust\n{}\n```\n\n", e.title, e.code));
        }
        out.push_str(&format!(
            "## Where it goes\n\nThe test goes into {}, which starts with `use super::*;`, so everything in that file is in scope.\n",
            self.placement.describe(&self.file)
        ));
        if !self.taken.is_empty() {
            let names: Vec<_> = self.taken.iter().map(|n| format!("`{n}`")).collect();
            out.push_str(&format!("These names are taken there: {}.\n", names.join(", ")));
        }
        out
    }

    pub fn context_lines(&self) -> usize {
        self.context.iter().map(|e| e.lines).sum::<usize>() + self.example.as_ref().map_or(0, |e| e.lines)
    }

    /// The file with `test` inserted, and the line range it occupies.
    fn insert(&self, original: &str, test: &str) -> (String, usize, usize, String) {
        let mut lines: Vec<String> = original.lines().map(String::from).collect();
        match &self.placement {
            Placement::Module { line, indent, .. } => {
                let indented = indent_block(test, indent);
                let at = line - 1; // index of the closing brace
                let mut added = vec![String::new()];
                added.extend(indented.lines().map(String::from));
                let start = at + 2; // 1-based line after the blank one
                let end = at + added.len();
                lines.splice(at..at, added);
                (join(lines, original), start, end, indented)
            }
            Placement::NewModule { name } => {
                let indented = indent_block(test, "    ");
                lines.push(String::new());
                lines.push("#[cfg(test)]".into());
                lines.push(format!("mod {name} {{"));
                lines.push("    use super::*;".into());
                lines.push(String::new());
                let start = lines.len() + 1;
                lines.extend(indented.lines().map(String::from));
                let end = lines.len();
                lines.push("}".into());
                (join(lines, original), start, end, indented)
            }
        }
    }

    fn coverage_hint(&self) -> String {
        match &self.goal {
            Goal::Accepted => {
                let kinds: Vec<_> = self.rejection_kinds.iter().map(|k| format!("\"{k}\"")).collect();
                let mut s = format!(
                    "The test must call `{}::{}(` itself (or through a helper in the test module).",
                    self.rust_name, self.call
                );
                if !kinds.is_empty() {
                    s.push_str(&format!(
                        " A test of the accepted path must not contain any rejection kind string: {}.",
                        kinds.join(", ")
                    ));
                }
                s
            }
            Goal::Rejected(kind) => format!(
                "The test must call `{}::{}(` itself (or through a helper in the test module) and contain the string literal \"{kind}\".",
                self.rust_name, self.call
            ),
            Goal::Folds(event) => format!(
                "The test must call `{}::project(` itself and use the event `{event}`.",
                self.rust_name
            ),
        }
    }
}

fn indent_block(code: &str, indent: &str) -> String {
    code.lines()
        .map(|l| if l.trim().is_empty() { String::new() } else { format!("{indent}{l}") })
        .collect::<Vec<_>>()
        .join("\n")
}

fn join(lines: Vec<String>, original: &str) -> String {
    let mut s = lines.join("\n");
    if original.ends_with('\n') {
        s.push('\n');
    }
    s
}

fn head(text: &str, n: usize) -> String {
    let lines: Vec<_> = text.lines().collect();
    if lines.len() <= n {
        text.to_string()
    } else {
        format!("{}\n... ({} more lines)", lines[..n].join("\n"), lines.len() - n)
    }
}

/// The test function in a model's answer, normalised by `prettyplease`
/// so indentation is predictable, with its name.
fn extract_test(reply: &str, taken: &BTreeSet<String>) -> Result<(String, String), Problem> {
    use syn::spanned::Spanned as _;
    let code = code_block(reply);
    let file = syn::parse_file(&code)
        .map_err(|e| Problem::NotOneTest(format!("The answer isn't valid Rust: {e}.")))?;
    // Forgiven, because the meaning is clear: the test wrapped in a
    // `mod tests`, and `use` lines next to it, which move into its body.
    let mut items: Vec<&syn::Item> = file.items.iter().collect();
    if let [syn::Item::Mod(m)] = items.as_slice()
        && let Some((_, inner)) = &m.content
    {
        items = inner.iter().collect();
    }
    let mut fns = Vec::new();
    let mut uses = Vec::new();
    let mut others = 0;
    for item in items {
        match item {
            syn::Item::Fn(f) => fns.push(f),
            syn::Item::Use(u) => uses.push(u),
            _ => others += 1,
        }
    }
    let [f] = fns.as_slice() else {
        return Err(Problem::NotOneTest(format!(
            "The answer has {} functions; it must be exactly one #[test] function. Put any helper code inside it.",
            fns.len()
        )));
    };
    if others > 0 {
        return Err(Problem::NotOneTest(
            "The answer has items besides the test function (a struct, const or module). Answer with the test function only."
                .into(),
        ));
    }
    if !f.attrs.iter().any(|a| a.path().is_ident("test")) {
        return Err(Problem::NotOneTest("The function has no #[test] attribute.".into()));
    }
    if f.sig.asyncness.is_some() {
        return Err(Problem::NotOneTest("The test is async; decide() and project() are synchronous, so it must not be.".into()));
    }
    let name = f.sig.ident.to_string();
    if taken.contains(&name) {
        return Err(Problem::NameTaken(name));
    }
    // Keep the model's own formatting: cut the function's lines out of
    // the answer.
    let lines: Vec<&str> = code.lines().collect();
    let text = |start: usize, end: usize| lines[start - 1..end].join("\n");
    let start = f.attrs.iter().map(|a| a.span().start().line).chain([f.span().start().line]).min().unwrap_or(1);
    let open = f.block.brace_token.span.open().start().line;
    let end = f.block.brace_token.span.close().end().line;
    let body_indent: String = lines
        .get(open..end - 1)
        .and_then(|body| body.iter().find(|l| !l.trim().is_empty()))
        .map(|l| l.chars().take_while(|c| c.is_whitespace()).collect())
        .unwrap_or_else(|| "    ".into());
    let moved: Vec<String> = uses
        .iter()
        .map(|u| dedent(&text(u.span().start().line, u.span().end().line)))
        .filter(|u| u.trim() != "use super::*;")
        .map(|u| indent_block(&u, &body_indent))
        .collect();
    let test = if moved.is_empty() || open == end {
        text(start, end)
    } else {
        format!("{}\n{}\n{}", text(start, open), moved.join("\n"), text(open + 1, end))
    };
    Ok((with_skilj_core_imports(&dedent(&test)), name))
}

/// skilj doesn't re-export these, and models forget the import: added
/// to the test's body when it uses them without one.
const SKILJ_CORE_PLUGIN: [&str; 2] = ["Decision", "SnapshotState"];

fn with_skilj_core_imports(test: &str) -> String {
    let used = source::idents_in(test);
    let imported = |name: &str| test.lines().any(|l| l.trim_start().starts_with("use ") && l.contains(name));
    let missing: Vec<&str> = SKILJ_CORE_PLUGIN.into_iter().filter(|n| used.contains(*n) && !imported(n)).collect();
    if missing.is_empty() {
        return test.to_string();
    }
    let Ok(f) = syn::parse_str::<syn::ItemFn>(test) else { return test.to_string() };
    let open = f.block.brace_token.span.open().start().line;
    let mut lines: Vec<String> = test.lines().map(String::from).collect();
    lines.insert(open, format!("    use skilj_core::plugin::{{{}}};", missing.join(", ")));
    lines.join("\n")
}

fn dedent(code: &str) -> String {
    let indent = code
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    code.lines()
        .map(|l| if l.len() >= indent { &l[indent..] } else { l.trim_start() })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Asks, checks and retries until a test is verified or `MAX_ATTEMPTS`
/// answers had a problem.
pub async fn run(
    index: &ProjectIndex,
    plan: &Plan,
    model: &ModelConfig,
    mut progress: impl FnMut(Progress<'_>),
) -> anyhow::Result<RunOutcome> {
    let original = std::fs::read_to_string(index.root.join(&plan.file)).with_context(|| plan.file.clone())?;
    let scratch = Scratch::of(&index.root)?;
    let mut conversation = Conversation::new(model, &plan.system_prompt())?;
    let mut message = plan.task_prompt();
    let mut last = None;
    let package_rel = plan.file.find("src/").map_or(plan.file.as_str(), |at| &plan.file[at..]);
    let package_dir = &plan.file[..plan.file.len() - package_rel.len()];
    let target = match Target::of_file(package_rel) {
        Target::Lib if !scratch.path().join(package_dir).join("src/lib.rs").exists() => Target::Bin(plan.package.clone()),
        t => t,
    };

    for attempt in 1..=MAX_ATTEMPTS {
        progress(Progress::Asking { attempt, message: &message });
        let reply = conversation.ask(&message).await?;
        progress(Progress::Answered { attempt, reply: &reply });

        let extracted = if reply.cut_off { Err(Problem::CutOff) } else { extract_test(&reply.text, &plan.taken) };
        let problem = match extracted {
            Err(p) => p,
            Ok((test, name)) => {
                let (changed, start, end, indented) = plan.insert(&original, &test);
                scratch.write(&plan.file, &changed)?;
                match covers(scratch.path(), plan, &name) {
                    Err(p) => p,
                    Ok(()) => {
                        progress(Progress::Building);
                        let touched = Touched { file: plan.file.clone(), start, end };
                        // rustc reports paths relative to the package for
                        // a member crate, to the root for a single one.
                        let touched_pkg = Touched { file: package_rel.to_string(), ..touched.clone() };
                        let outcome = scratch.run_test(&plan.package, &target, &name, &touched)?;
                        let outcome = match outcome {
                            Outcome::BrokenElsewhere(_) if !package_dir.is_empty() => {
                                scratch.run_test(&plan.package, &target, &name, &touched_pkg)?
                            }
                            o => o,
                        };
                        match outcome {
                            Outcome::Passed => {
                                return Ok(RunOutcome::Verified(Proposal {
                                    file: plan.file.clone(),
                                    test_name: name,
                                    test: indented,
                                    where_: plan.placement.describe(&plan.file),
                                    attempts: attempt,
                                    original,
                                    changed,
                                }));
                            }
                            Outcome::BrokenElsewhere(errors) => return Ok(RunOutcome::ProjectBroken(errors)),
                            Outcome::CompileErrors(e) => Problem::CompileErrors(e),
                            Outcome::TestFailed(out) => Problem::TestFailed(out),
                            Outcome::TestNotRun => Problem::TestNotRun,
                        }
                    }
                }
            }
        };
        progress(Progress::Problem { attempt, problem: &problem });
        message = problem.feedback();
        last = Some(problem);
    }
    Ok(RunOutcome::GaveUp { attempts: MAX_ATTEMPTS, last: last.expect("at least one attempt") })
}

/// Whether the index, rerun on the changed copy, counts the new test as
/// covering the obligation.
fn covers(scratch: &Path, plan: &Plan, test: &str) -> Result<(), Problem> {
    let index = skilj_agent_index::index(scratch)
        .map_err(|e| Problem::NotOneTest(format!("The changed project can't be read: {e}")))?;
    let covered = code_obligations(&index)
        .into_iter()
        .find(|o| o.id == plan.obligation)
        .is_some_and(|o| o.covered_by.iter().any(|c| c == &format!("{}::{test}", plan.file)));
    if covered { Ok(()) } else { Err(Problem::DoesNotCover(plan.coverage_hint())) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_obligation_ids() {
        assert_eq!(
            parse_obligation("command.Withdraw.rejected.insufficient_funds").unwrap(),
            (TypeKind::Command, "Withdraw".into(), Goal::Rejected("insufficient_funds".into()))
        );
        assert_eq!(
            parse_obligation("projection.Balance.folds.Deposited").unwrap(),
            (TypeKind::Projection, "Balance".into(), Goal::Folds("Deposited".into()))
        );
        assert!(parse_obligation("command.Withdraw.integration").is_err());
    }

    #[test]
    fn takes_one_test_function_from_an_answer() {
        let taken = BTreeSet::from(["existing".to_string()]);
        let ok = "Sure:\n```rust\n    #[test]\n    fn rejects() {\n        assert!(true);\n    }\n```";
        let (code, name) = extract_test(ok, &taken).unwrap();
        assert_eq!(name, "rejects");
        assert!(code.starts_with("#[test]\nfn rejects() {\n    assert!(true);"));
        assert!(matches!(extract_test("```rust\nstruct A;\n#[test]\nfn x() {}\n```", &taken), Err(Problem::NotOneTest(_))));
        assert!(matches!(extract_test("```rust\nfn x() {}\n```", &taken), Err(Problem::NotOneTest(_))));
        assert!(matches!(extract_test("```rust\n#[test]\nfn existing() {}\n```", &taken), Err(Problem::NameTaken(_))));
    }

    #[test]
    fn unwraps_a_test_module_and_moves_use_lines_into_the_test() {
        let answer = "```rust\nmod tests {\n    use super::*;\n    use skilj_core::plugin::Decision;\n\n    #[test]\n    fn x() {\n        let a = 1;\n    }\n}\n```";
        let (code, name) = extract_test(answer, &BTreeSet::new()).unwrap();
        assert_eq!(name, "x");
        assert_eq!(code, "#[test]\nfn x() {\n    use skilj_core::plugin::Decision;\n    let a = 1;\n}");
    }

    #[test]
    fn adds_the_skilj_core_imports_a_test_forgot() {
        let test = "#[test]\nfn x() {\n    let d = Decision::new(&[], &[]);\n}";
        assert_eq!(
            with_skilj_core_imports(test),
            "#[test]\nfn x() {\n    use skilj_core::plugin::{Decision};\n    let d = Decision::new(&[], &[]);\n}"
        );
        let imported = "#[test]\nfn x() {\n    use skilj_core::plugin::Decision;\n    let d = Decision::new(&[], &[]);\n}";
        assert_eq!(with_skilj_core_imports(imported), imported);
    }

    #[test]
    fn inserts_before_the_closing_brace_of_the_module() {
        let original = "fn a() {}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n}\n";
        let items = source::items(original).unwrap();
        let mut plan = test_plan();
        plan.placement = placement(original, &items, None);
        let (changed, start, end, _) = plan.insert(original, "#[test]\nfn b() {\n    a();\n}");
        assert_eq!(
            changed,
            "fn a() {}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn b() {\n        a();\n    }\n}\n"
        );
        assert_eq!((start, end), (7, 10));
        assert_eq!(changed.lines().nth(start - 1), Some("    #[test]"));
    }

    #[test]
    fn adds_a_test_module_when_there_is_none() {
        let original = "fn a() {}\n";
        let mut plan = test_plan();
        plan.placement = placement(original, &source::items(original).unwrap(), None);
        let (changed, start, end, _) = plan.insert(original, "#[test]\nfn b() {}");
        assert_eq!(changed, "fn a() {}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn b() {}\n}\n");
        assert_eq!((start, end), (7, 8));
    }

    fn test_plan() -> Plan {
        Plan {
            obligation: "command.A.accepted".into(),
            goal: Goal::Accepted,
            type_name: "A".into(),
            rust_name: "A".into(),
            call: "decide".into(),
            kind: TypeKind::Command,
            rejection_kinds: BTreeSet::new(),
            file: "src/a.rs".into(),
            package: "a".into(),
            placement: Placement::NewModule { name: "tests".into() },
            context: Vec::new(),
            example: None,
            taken: BTreeSet::new(),
        }
    }
}
