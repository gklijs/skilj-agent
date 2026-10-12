//! Reads a skilj project without a model: its bounded contexts, the
//! plugin types registered in them, and its tests.
//!
//! Everything here is static analysis (cargo metadata, a `syn` parse of
//! every source file, and `*.skilj.toml` files for skilj-codegen), so the
//! same project always gives the same index.

mod cargo;
mod codegen;
mod facts;
mod rust;

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub use cargo::not_a_skilj_project;

/// What `index()` found in one project.
#[derive(Debug, Serialize)]
pub struct ProjectIndex {
    pub root: PathBuf,
    /// The `skilj` (or `skilj-core`) version requirement, as written in
    /// Cargo.toml.
    pub skilj_version: Option<String>,
    pub files: Vec<SourceFile>,
    pub contexts: Vec<BoundedContext>,
    pub types: Vec<PluginType>,
    pub tests: Vec<TestFn>,
    /// Things the index couldn't read, e.g. a file `syn` can't parse.
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SourceFile {
    /// Relative to the project root, with `/` separators.
    pub path: String,
    pub package: String,
    pub lines: usize,
    pub kind: FileKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    /// Under a package's `src/`.
    Src,
    /// Under a package's `tests/`: integration tests and their helpers.
    Tests,
    /// A `*.skilj.toml` declaration file.
    SkiljToml,
}

#[derive(Debug, Serialize)]
pub struct BoundedContext {
    pub name: String,
    pub package: String,
    /// The file declaring `pub const BOUNDED_CONTEXT` (or the
    /// `.skilj.toml` declaring `bounded_context`).
    pub declared_in: String,
    /// Every file holding one of this context's plugin types, sorted.
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeKind {
    Event,
    Command,
    Projection,
    Snapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclaredIn {
    Rust,
    SkiljToml,
}

#[derive(Debug, Serialize)]
pub struct PluginType {
    pub context: String,
    pub kind: TypeKind,
    /// The Rust type implementing the trait, e.g. `WithdrawMoney`.
    pub rust_name: String,
    /// The `NAME` constant, e.g. `"WithdrawMoney"`.
    pub name: String,
    pub file: String,
    pub line: usize,
    pub declared_in: DeclaredIn,
    /// Commands: every `kind` a `CommandDecision::Rejected` reachable from
    /// `decide()` can carry, following calls to helpers in the project.
    pub rejection_kinds: BTreeSet<String>,
    /// Projections: the event `NAME`s from `consumed_event_types()`.
    pub consumed_event_types: BTreeSet<String>,
    /// Projections and snapshots: the `State` type.
    pub state_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestLevel {
    /// A `#[test]` under `src/`.
    Unit,
    /// A `#[test]` under `tests/`.
    Integration,
}

#[derive(Debug, Serialize)]
pub struct TestFn {
    pub name: String,
    pub file: String,
    pub line: usize,
    pub level: TestLevel,
    /// String literals in the test and in the same-file helpers it calls,
    /// including inside macros like `assert_eq!` and `json!`.
    pub literals: BTreeSet<String>,
    /// Identifiers in the test and those helpers: types, functions,
    /// methods.
    pub idents: BTreeSet<String>,
    /// `Type::function` for every call on a type, e.g.
    /// `WithdrawMoney::decide`, including through same-file helpers.
    pub type_calls: BTreeSet<String>,
    /// Obligation ids from `// skilj-agent: covers <id>` and
    /// `// allium: <id>` comments inside the test.
    pub markers: BTreeSet<String>,
}

impl TestFn {
    /// Calls this command's `decide()` (or `decide_with`,
    /// `decide_from_snapshot`) itself: a unit test of its decision.
    pub fn decides(&self, command: &PluginType) -> bool {
        ["decide", "decide_with", "decide_from_snapshot"]
            .iter()
            .any(|f| self.type_calls.contains(&format!("{}::{f}", command.rust_name)))
    }

    /// Calls this projection's `project()` itself: a unit test of its fold.
    pub fn folds(&self, projection: &PluginType) -> bool {
        self.type_calls.contains(&format!("{}::project", projection.rust_name))
    }

    /// Whether the test names this type, by its Rust name or its `NAME`.
    pub fn mentions(&self, ty: &PluginType) -> bool {
        self.idents.contains(&ty.rust_name)
            || self.literals.contains(&ty.name)
            || ty.state_type.as_ref().is_some_and(|s| self.idents.contains(s))
    }
}

/// Builds the index for the project at `root`. Fails when cargo can't
/// read the project; returns `Err(not_a_skilj_project)` when nothing in
/// it depends on skilj.
pub fn index(root: &Path) -> anyhow::Result<ProjectIndex> {
    let root = root
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("{}: {e}", root.display()))?;
    let workspace = cargo::read(&root)?;
    let mut index = ProjectIndex {
        root: root.clone(),
        skilj_version: workspace.skilj_version.clone(),
        files: Vec::new(),
        contexts: Vec::new(),
        types: Vec::new(),
        tests: Vec::new(),
        warnings: Vec::new(),
    };

    let mut parsed = Vec::new();
    for package in &workspace.packages {
        for (path, kind) in cargo::source_files(package) {
            let rel = relative(&root, &path);
            let source = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    index.warnings.push(format!("{rel}: {e}"));
                    continue;
                }
            };
            index.files.push(SourceFile {
                path: rel.clone(),
                package: package.name.clone(),
                lines: source.lines().count(),
                kind,
            });
            match kind {
                FileKind::SkiljToml => match codegen::parse(&source) {
                    Ok(decl) => parsed.push(Parsed::Toml { rel, package: package.name.clone(), decl }),
                    Err(e) => index.warnings.push(format!("{rel}: {e}")),
                },
                _ => match rust::parse(&source) {
                    Ok(file) => parsed.push(Parsed::Rust { rel, package: package.name.clone(), kind, file }),
                    Err(e) => index.warnings.push(format!("{rel}: could not parse: {e}")),
                },
            }
        }
    }

    resolve(&mut index, parsed);
    Ok(index)
}

enum Parsed {
    Rust { rel: String, package: String, kind: FileKind, file: rust::FileModel },
    Toml { rel: String, package: String, decl: codegen::Declaration },
}

fn resolve(index: &mut ProjectIndex, parsed: Vec<Parsed>) {
    // Which context each Rust file belongs to: its own BOUNDED_CONTEXT,
    // or the nearest one up the module tree (a file in src/helpdesk/
    // picks up src/helpdesk/mod.rs's), the way `use super::*` sees it.
    let mut declared: BTreeMap<String, (String, String)> = BTreeMap::new(); // file -> (context, package)
    let mut contexts: BTreeMap<String, BoundedContext> = BTreeMap::new();
    for p in &parsed {
        let (rel, package, name) = match p {
            Parsed::Rust { rel, package, file, .. } => match &file.bounded_context {
                Some(name) => (rel, package, name.clone()),
                None => continue,
            },
            Parsed::Toml { rel, package, decl } => (rel, package, decl.bounded_context.clone()),
        };
        declared.insert(rel.clone(), (name.clone(), package.clone()));
        contexts.entry(name.clone()).or_insert_with(|| BoundedContext {
            name,
            package: package.clone(),
            declared_in: rel.clone(),
            files: Vec::new(),
        });
    }
    let context_of = |rel: &str| -> Option<String> {
        if let Some((name, _)) = declared.get(rel) {
            return Some(name.clone());
        }
        // src/banking.rs next to src/banking.skilj.toml: its
        // BOUNDED_CONTEXT comes from the code generated from the TOML.
        if let Some(stem) = rel.strip_suffix(".rs")
            && let Some((name, _)) = declared.get(&format!("{stem}.skilj.toml")) {
                return Some(name.clone());
            }
        let mut dir = Path::new(rel).parent();
        while let Some(d) = dir {
            let mod_rs = format!("{}/mod.rs", d.display());
            if let Some((name, _)) = declared.get(&mod_rs) {
                return Some(name.clone());
            }
            let sibling = format!("{}.rs", d.display());
            if let Some((name, _)) = declared.get(&sibling) {
                return Some(name.clone());
            }
            dir = d.parent();
        }
        None
    };

    // All functions by name, per context (None: files outside any
    // context), for following calls out of decide().
    let mut fns_by_context: BTreeMap<Option<String>, BTreeMap<String, Vec<&rust::FnModel>>> = BTreeMap::new();
    for p in &parsed {
        if let Parsed::Rust { rel, kind: FileKind::Src, file, .. } = p {
            let ctx = context_of(rel);
            let map = fns_by_context.entry(ctx).or_default();
            for f in &file.fns {
                map.entry(f.name.clone()).or_default().push(f);
            }
        }
    }

    let mut types = Vec::new();
    for p in &parsed {
        match p {
            Parsed::Rust { rel, kind: FileKind::Src, file, .. } => {
                for imp in &file.impls {
                    let context = match &imp.context {
                        rust::ContextRef::Literal(name) => name.clone(),
                        rust::ContextRef::Inherited => context_of(rel).unwrap_or_else(|| "default".into()),
                    };
                    let mut rejection_kinds = BTreeSet::new();
                    if imp.kind == TypeKind::Command {
                        let fns = [fns_by_context.get(&Some(context.clone())), fns_by_context.get(&None)];
                        collect_rejections(&imp.decide, Some(&imp.rust_name), &fns, &mut rejection_kinds, &mut BTreeSet::new(), 0);
                    }
                    types.push(PluginType {
                        context,
                        kind: imp.kind,
                        rust_name: imp.rust_name.clone(),
                        name: imp.name.clone().unwrap_or_else(|| imp.rust_name.clone()),
                        file: rel.clone(),
                        line: imp.line,
                        declared_in: DeclaredIn::Rust,
                        rejection_kinds,
                        consumed_event_types: imp.consumed.clone(),
                        state_type: imp.state_type.clone(),
                    });
                }
            }
            Parsed::Toml { rel, decl, .. } => {
                let fns = [fns_by_context.get(&Some(decl.bounded_context.clone())), fns_by_context.get(&None)];
                for (kind, name) in decl.types() {
                    let mut rejection_kinds = BTreeSet::new();
                    if kind == TypeKind::Command {
                        // skilj-codegen's generated decide() delegates to a
                        // hand-written decide_<snake_case name>.
                        let start = rust::Facts::calling(&codegen::decide_fn_name(&name));
                        collect_rejections(&start, None, &fns, &mut rejection_kinds, &mut BTreeSet::new(), 0);
                    }
                    types.push(PluginType {
                        context: decl.bounded_context.clone(),
                        kind,
                        rust_name: name.clone(),
                        name,
                        file: rel.clone(),
                        line: 0,
                        declared_in: DeclaredIn::SkiljToml,
                        rejection_kinds,
                        consumed_event_types: BTreeSet::new(),
                        state_type: None,
                    });
                }
            }
            _ => {}
        }
    }

    // Projections may name consumed events as `SomeEvent::NAME`.
    let event_names: BTreeMap<String, String> = types
        .iter()
        .filter(|t| t.kind == TypeKind::Event)
        .map(|t| (t.rust_name.clone(), t.name.clone()))
        .collect();
    for t in types.iter_mut().filter(|t| t.kind == TypeKind::Projection) {
        t.consumed_event_types = t
            .consumed_event_types
            .iter()
            .map(|c| event_names.get(c).cloned().unwrap_or_else(|| c.clone()))
            .collect();
    }

    for t in &types {
        if let Some(ctx) = contexts.get_mut(&t.context) {
            if !ctx.files.contains(&t.file) {
                ctx.files.push(t.file.clone());
            }
        } else {
            contexts.insert(
                t.context.clone(),
                BoundedContext {
                    name: t.context.clone(),
                    package: index
                        .files
                        .iter()
                        .find(|f| f.path == t.file)
                        .map(|f| f.package.clone())
                        .unwrap_or_default(),
                    declared_in: t.file.clone(),
                    files: vec![t.file.clone()],
                },
            );
        }
    }
    for ctx in contexts.values_mut() {
        ctx.files.sort();
    }

    // Tests: facts of the test plus the same-file helpers it calls.
    for p in &parsed {
        if let Parsed::Rust { rel, kind, file, .. } = p {
            let level = if *kind == FileKind::Tests { TestLevel::Integration } else { TestLevel::Unit };
            // Only test code is followed: helpers in #[cfg(test)] modules,
            // or anything in a tests/ file. Following into production code
            // would make every test look like it checks every branch.
            let local: BTreeMap<&str, &rust::FnModel> = file
                .fns
                .iter()
                .filter(|f| f.test_only || *kind == FileKind::Tests)
                .map(|f| (f.name.as_str(), f))
                .collect();
            for t in file.fns.iter().filter(|f| f.is_test) {
                let mut facts = rust::Facts::default();
                follow_local(t, &local, &mut facts, &mut BTreeSet::new(), 0);
                index.tests.push(TestFn {
                    name: t.name.clone(),
                    file: rel.clone(),
                    line: t.line,
                    level,
                    literals: facts.literals,
                    idents: facts.idents,
                    type_calls: facts
                        .calls
                        .iter()
                        .filter_map(|c| match c {
                            crate::facts::Call::OnType(t, f) => Some(format!("{t}::{f}")),
                            _ => None,
                        })
                        .collect(),
                    markers: file.markers_between(t.line, t.end_line),
                });
            }
        }
    }

    index.contexts = contexts.into_values().collect();
    types.sort_by(|a, b| (&a.context, a.kind, &a.name).cmp(&(&b.context, b.kind, &b.name)));
    index.types = types;
    for p in &parsed {
        if let Parsed::Rust { rel, file, .. } = p {
            for w in &file.warnings {
                index.warnings.push(format!("{rel}: {w}"));
            }
        }
    }
}

const MAX_CALL_DEPTH: usize = 4;

type FnsByName<'a> = BTreeMap<String, Vec<&'a rust::FnModel>>;

/// Follows `decide()` into the helpers it calls, collecting every
/// rejection kind on the way. Calls are resolved by owner where syntax
/// allows; a method call on an unknown receiver is only followed when
/// one type defines that method, so a miss is likelier than a kind that
/// isn't really there.
fn collect_rejections(
    facts: &rust::Facts,
    owner: Option<&str>,
    fns: &[Option<&FnsByName>],
    kinds: &mut BTreeSet<String>,
    seen: &mut BTreeSet<(Option<String>, String)>,
    depth: usize,
) {
    use crate::facts::Call;
    kinds.extend(facts.rejections.iter().cloned());
    if depth >= MAX_CALL_DEPTH {
        return;
    }
    for call in &facts.calls {
        let (name, wanted_owner): (&str, Option<Option<&str>>) = match call {
            Call::Free(n) => (n, Some(None)),
            Call::OnSelf(n) => (n, Some(owner)),
            Call::OnType(t, n) => (n, Some(Some(t.as_str()))),
            Call::Method(n) => (n, None),
        };
        let candidates: Vec<&rust::FnModel> = fns
            .iter()
            .flatten()
            .flat_map(|m| m.get(name).into_iter().flatten().copied())
            .filter(|f| !f.is_test && !f.test_only)
            .filter(|f| match wanted_owner {
                Some(o) => f.owner.as_deref() == o,
                None => f.owner.is_some(),
            })
            .collect();
        if wanted_owner.is_none() {
            let owners: BTreeSet<_> = candidates.iter().map(|f| &f.owner).collect();
            if owners.len() != 1 {
                continue;
            }
        }
        for f in candidates {
            if seen.insert((f.owner.clone(), f.name.clone())) {
                collect_rejections(&f.facts, f.owner.as_deref(), fns, kinds, seen, depth + 1);
            }
        }
    }
}

fn follow_local(
    f: &rust::FnModel,
    local: &BTreeMap<&str, &rust::FnModel>,
    facts: &mut rust::Facts,
    seen: &mut BTreeSet<String>,
    depth: usize,
) {
    facts.literals.extend(f.facts.literals.iter().cloned());
    facts.idents.extend(f.facts.idents.iter().cloned());
    facts.calls.extend(f.facts.calls.iter().cloned());
    if depth >= MAX_CALL_DEPTH {
        return;
    }
    for ident in &f.facts.idents {
        if let Some(callee) = local.get(ident.as_str())
            && !callee.is_test && seen.insert(ident.clone()) {
                follow_local(callee, local, facts, seen, depth + 1);
            }
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
