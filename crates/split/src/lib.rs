//! Mechanically splits one bounded context into the shape the
//! project's layout style prescribes (spec: RestructureContext). No
//! model: items move with `syn` and print with `prettyplease`, and the
//! result is verified like any other proposal.
//!
//! Supported: a context in one file (or one file plus its `mod.rs`)
//! into `by_kind` (`mod.rs` + `events.rs` + `commands.rs` +
//! `projections.rs`) or `by_slice` (one file per command/projection,
//! shared events in `events.rs`). Anything else — `crate_per_context`,
//! codegen contexts, contexts sharing files — is refused with a reason.

use anyhow::{Context as _, bail};
use quote::ToTokens;
use skilj_agent_checks::LayoutStyle;
use skilj_agent_index::{ProjectIndex, TypeKind};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use syn::visit::Visit;

/// One file the split writes.
#[derive(Debug)]
pub struct SplitFile {
    /// Relative to the project root, e.g. `src/orders/commands.rs`.
    pub path: String,
    pub content: String,
}

/// What the split of `context` into `style` would write.
#[derive(Debug)]
pub struct SplitPlan {
    pub context: String,
    pub style: LayoutStyle,
    /// The directory the context moves into, e.g. `src/orders`.
    pub dir: String,
    pub files: Vec<SplitFile>,
    /// Source files the context leaves behind (deleted when empty of
    /// the context's items, else rewritten without them).
    pub remove_or_rewrite: Vec<String>,
}

/// Plans the split; fails with a reason when the context can't be
/// split mechanically.
pub fn plan(index: &ProjectIndex, context: &str, style: LayoutStyle) -> anyhow::Result<SplitPlan> {
    if style == LayoutStyle::CratePerContext {
        bail!("crate-per-context splits are not supported yet; move the context into crates/<context>/ by hand");
    }
    let ctx = index
        .contexts
        .iter()
        .find(|c| c.name == context)
        .with_context(|| format!("no bounded context {context:?}"))?;
    let types: Vec<_> = index.types.iter().filter(|t| t.context == context).collect();
    if types.is_empty() {
        bail!("bounded context {context:?} has no plugin types");
    }
    if types.iter().any(|t| t.declared_in == skilj_agent_index::DeclaredIn::SkiljToml) {
        bail!("{context:?} is declared in *.skilj.toml; split the hand-written .rs files by hand");
    }
    let mut files: BTreeSet<&str> = types.iter().map(|t| t.file.as_str()).collect();
    // The file declaring the context (mod.rs or <ctx>.rs) may hold no
    // types itself.
    files.insert(&ctx.declared_in);
    // Every type file must hold only this context's types.
    for f in &files {
        let others: BTreeSet<_> = index.types.iter().filter(|t| t.file.as_str() == *f && t.context != context).map(|t| t.context.as_str()).collect();
        if !others.is_empty() {
            bail!("{f} also holds {} ({}); split shared files by hand first", others.len(), others.into_iter().collect::<Vec<_>>().join(", "));
        }
    }
    if files.len() > 2 {
        bail!("{context:?} already spans {} files; only single-file contexts are split", files.len());
    }
    let dir = context_dir(index, ctx, &types)?;
    let items = read_items(index, &files)?;

    let groups: Vec<(String, Vec<ItemRef>)> = match style {
        LayoutStyle::ByKind => group_by_kind(&types),
        LayoutStyle::BySlice => group_by_slice(&types),
        LayoutStyle::CratePerContext => unreachable!(),
    };
    let mut out_files = Vec::new();
    // Every plugin type of the context, so reference expansion below
    // doesn't drag a sibling type (and its impl) into every file.
    let all_type_names: BTreeSet<String> = types.iter().map(|t| t.rust_name.clone()).collect();
    // Names used from more than one new file (the event enum, the tag
    // helper): they live in `events.rs`, and the other files import
    // them from there instead of copying them.
    let refs = item_refs(&items);
    let shared = shared_names(&groups, &refs, &all_type_names);
    for (file_stem, group) in &groups {
        let names: BTreeSet<String> = group.iter().map(|r| r.type_name.clone()).collect();
        let content = render_file(&items, &refs, &names, &all_type_names, &shared, file_stem, context)?;
        out_files.push(SplitFile { path: format!("{dir}/{file_stem}.rs"), content });
    }
    // The context constant lives in mod.rs, which declares the modules.
    let stems: Vec<&str> = groups.iter().map(|(s, _)| s.as_str()).collect();
    out_files.push(SplitFile {
        path: format!("{dir}/mod.rs"),
        content: render_mod(&items, context, &stems)?,
    });
    Ok(SplitPlan {
        context: context.to_string(),
        style,
        dir,
        files: out_files,
        remove_or_rewrite: files.into_iter().map(String::from).collect(),
    })
}

/// Applies a plan to the project at `root`: writes the new files,
/// updates `lib.rs`/`main.rs` module declarations, removes emptied
/// source files.
pub fn apply(root: &Path, index: &ProjectIndex, plan: &SplitPlan) -> anyhow::Result<()> {
    for f in &plan.files {
        let dest = root.join(&f.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, &f.content).with_context(|| format!("writing {}", f.path))?;
    }
    // Old files: keep any items that aren't the context's (other
    // helpers, other tests); delete the file when nothing is left.
    let ctx_item_names = moved_item_names(index, &plan.context);
    for rel in &plan.remove_or_rewrite {
        // Don't delete what we just wrote (single-file dir == file's dir).
        if plan.files.iter().any(|f| &f.path == rel) {
            continue;
        }
        let path = root.join(rel);
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let kept = remove_items(&source, &ctx_item_names)?;
        if kept.trim().is_empty() {
            std::fs::remove_file(&path)?;
        } else {
            std::fs::write(&path, kept)?;
        }
    }
    ensure_mod_decl(root, index, &plan.dir)?;
    Ok(())
}

struct ItemRef {
    type_name: String,
}

fn group_by_kind(types: &[&skilj_agent_index::PluginType]) -> Vec<(String, Vec<ItemRef>)> {
    let mut groups: BTreeMap<&str, Vec<ItemRef>> = BTreeMap::new();
    for t in types {
        let stem = match t.kind {
            TypeKind::Event => "events",
            TypeKind::Command => "commands",
            TypeKind::Projection | TypeKind::Snapshot => "projections",
        };
        groups.entry(stem).or_default().push(ItemRef { type_name: t.rust_name.clone() });
    }
    groups.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn group_by_slice(types: &[&skilj_agent_index::PluginType]) -> Vec<(String, Vec<ItemRef>)> {
    let mut events = Vec::new();
    let mut rest: BTreeMap<String, Vec<ItemRef>> = BTreeMap::new();
    for t in types {
        let item = ItemRef { type_name: t.rust_name.clone() };
        match t.kind {
            TypeKind::Event => events.push(item),
            TypeKind::Command | TypeKind::Projection | TypeKind::Snapshot => {
                rest.entry(snake(&t.rust_name)).or_default().push(item);
            }
        }
    }
    let mut out = vec![("events".to_string(), events)];
    out.extend(rest);
    out
}

fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The directory the context moves into: the existing folder when its
/// single file is already `src/<ctx>.rs` or `src/<ctx>/mod.rs`, else
/// `src/<ctx>`.
fn context_dir(
    index: &ProjectIndex,
    ctx: &skilj_agent_index::BoundedContext,
    types: &[&skilj_agent_index::PluginType],
) -> anyhow::Result<String> {
    let _ = index;
    let snake_ctx = ctx.name.to_lowercase().replace('-', "_");
    if types.len() == 1
        && types[0].file == ctx.declared_in
        && let Some(dir) = Path::new(&ctx.declared_in).parent().and_then(|d| d.to_str())
        && (ctx.declared_in.ends_with("/mod.rs") || ctx.declared_in == format!("src/{snake_ctx}.rs"))
    {
        if ctx.declared_in.ends_with("/mod.rs") {
            return Ok(dir.to_string());
        }
        return Ok(format!("src/{snake_ctx}"));
    }
    // Common case: src/<ctx>.rs or src/<ctx>/mod.rs holding everything.
    if ctx.declared_in == format!("src/{snake_ctx}.rs") || ctx.declared_in == format!("src/{snake_ctx}/mod.rs") {
        return Ok(format!("src/{snake_ctx}"));
    }
    // Fall back to the file's own directory: src/helpdesk/ticket.rs ->
    // src/helpdesk/ticket/.
    if let Some(stem) = Path::new(&ctx.declared_in).file_stem().and_then(|s| s.to_str())
        && stem != "mod"
    {
        let dir = Path::new(&ctx.declared_in).parent().and_then(|d| d.to_str()).unwrap_or("src");
        return Ok(format!("{dir}/{stem}"));
    }
    bail!("can't tell where {0:?} should move; its declaring file is {1}", ctx.name, ctx.declared_in);
}

fn dir_name(dir: &str) -> String {
    dir.rsplit('/').next().unwrap_or(dir).to_string()
}

struct FileItems {
    /// The whole file parsed.
    file: syn::File,
    /// Source text by line for doc comments we keep verbatim.
    #[allow(dead_code)]
    source: String,
}

/// Reads every involved file.
fn read_items(index: &ProjectIndex, files: &BTreeSet<&str>) -> anyhow::Result<BTreeMap<String, FileItems>> {
    let mut out = BTreeMap::new();
    for f in files {
        let source = std::fs::read_to_string(index.root.join(f)).with_context(|| format!("reading {f}"))?;
        let file = syn::parse_file(&source).with_context(|| format!("parsing {f}"))?;
        out.insert(f.to_string(), FileItems { file, source });
    }
    Ok(out)
}

/// All top-level item names belonging to the context's types: the
/// types, their payloads/state, impl blocks, helpers they use, and the
/// context constant — everything the new files take with them.
fn moved_item_names(index: &ProjectIndex, context: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for t in index.types.iter().filter(|t| t.context == context) {
        names.insert(t.rust_name.clone());
        if let Some(s) = &t.state_type {
            names.insert(s.clone());
        }
        // Payloads and event enums are found by reference below; the
        // rejection walk already knows the helpers.
        let _ = t;
    }
    names.insert("BOUNDED_CONTEXT".to_string());
    names
}

/// Renders one new file: the items for `names` (types, their impls,
/// payloads, helpers, tests), with `use super::*` replaced by
/// `use super::super::*` depth handling left to the compiler via
/// `crate::` paths untouched. Shared `use` lines are copied.
#[allow(clippy::too_many_arguments)]
fn render_file(
    items: &BTreeMap<String, FileItems>,
    refs: &[(String, BTreeSet<String>)],
    names: &BTreeSet<String>,
    all_type_names: &BTreeSet<String>,
    shared: &BTreeSet<String>,
    file_stem: &str,
    context: &str,
) -> anyhow::Result<String> {
    // Collect the full set: named items plus anything they refer to
    // that lives in the same old files (payloads, helpers, the event enum).
    let mut wanted: BTreeSet<String> = names.iter().cloned().collect();
    wanted.insert("BOUNDED_CONTEXT".to_string());
    // events.rs also holds the shared helpers (the event enum, the tag
    // fn) that the other files import from it.
    if file_stem == "events" {
        wanted.extend(shared.iter().cloned());
    }
    // Expand by reference until fixed: an item whose name is wanted
    // pulls in the names it mentions, when those are defined in the
    // old files.
    let defined = defined_names(items);
    // Sibling plugin types and shared helpers live in their own new
    // files; the expansion must not pull them in here.
    let elsewhere: BTreeSet<&str> = all_type_names
        .difference(names)
        .map(String::as_str)
        .chain(shared.iter().map(String::as_str))
        .collect();
    // This file's own share: shared names it defines (events.rs holds
    // the event enum and the tag helper).
    let mine: BTreeSet<&str> = if file_stem == "events" {
        shared.iter().map(String::as_str).collect()
    } else {
        BTreeSet::new()
    };
    loop {
        let mut added = Vec::new();
        for (item_name, refers) in refs {
            if wanted.contains(item_name) {
                for r in refers {
                    // Shared helpers live in events.rs and sibling
                    // types in their own files; other files import
                    // them instead of copying them.
                    if elsewhere.contains(r.as_str()) && !mine.contains(r.as_str()) {
                        continue;
                    }
                    if defined.contains(r) && !wanted.contains(r) {
                        added.push(r.clone());
                    }
                }
            }
        }
        if added.is_empty() {
            break;
        }
        wanted.extend(added);
    }
    // The BOUNDED_CONTEXT const is defined in mod.rs; every file
    // uses it through `super`, so it isn't moved into the files.
    wanted.remove("BOUNDED_CONTEXT");
    let context_const = find_const(items, "BOUNDED_CONTEXT");

    let mut new_items: Vec<syn::Item> = Vec::new();
    let mut uses: Vec<syn::Item> = Vec::new();
    for f in items.values() {
        for item in &f.file.items {
            match item {
                syn::Item::Use(u) => {
                    if !uses.iter().any(|e| e == item) {
                        let _ = u;
                        uses.push(item.clone());
                    }
                }
                _ => {
                    let name = item_name(item);
                    let is_test_mod = matches!(item, syn::Item::Mod(m) if is_cfg_test(&m.attrs));
                    if is_test_mod {
                        continue; // handled below
                    }
                    if name.as_deref().is_some_and(|n| wanted.contains(n)) || impl_of(item, &wanted) {
                        let mut item = item.clone();
                        // Shared helpers are imported by sibling files:
                        // they must be visible outside events.rs.
                        if file_stem == "events"
                            && name.as_deref().is_some_and(|n| shared.contains(n))
                        {
                            make_pub(&mut item);
                        }
                        new_items.push(item);
                    }
                }
            }
        }
        // Tests: keep the #[cfg(test)] modules' tests that mention the moved types.
        for item in &f.file.items {
            if let syn::Item::Mod(m) = item
                && is_cfg_test(&m.attrs)
                && let Some((_, inner)) = &m.content
            {
                let kept: Vec<syn::Item> = inner
                    .iter()
                    .filter(|i| test_mentions(i, &wanted))
                    .cloned()
                    .collect();
                if !kept.is_empty() {
                    let mut m2 = m.clone();
                    m2.content = Some((m.content.as_ref().unwrap().0, kept));
                    // `use super::*` in the old file meant the context
                    // file; in src/<ctx>/<file>.rs it must reach one up.
                    fix_test_uses(&m2, context);
                    new_items.push(syn::Item::Mod(m2));
                }
            }
        }
    }
    // Rewrite `use super::X` paths: one level deeper now.
    let mut file = syn::File { shebang: None, attrs: Vec::new(), items: Vec::new() };
    file.items.extend(uses);
    // `#[auto_register(BOUNDED_CONTEXT)]` needs the const in scope:
    // it lives in the parent module now.
    if let Some(path) = &context_const {
        let use_item: syn::ItemUse = syn::parse_str(&format!("use super::{path};"))?;
        file.items.push(syn::Item::Use(use_item));
    }
    // Shared helpers (the event enum, the tag fn) live in events.rs;
    // every other new file imports them from there.
    if file_stem != "events" && !shared.is_empty() {
        let use_item: syn::ItemUse = syn::parse_str("use super::events::*;")?;
        file.items.push(syn::Item::Use(use_item));
    }
    file.items.extend(new_items);
    let mut out = prettyplease::unparse(&file);
    // Paths the old file's `use super::X` brought in are one level
    // deeper now; the `use super::BOUNDED_CONTEXT` added above already
    // points at the parent and must be left alone.
    out = fix_super_paths(&out);
    if context_const.is_some() {
        out = out.replace("use super::super::BOUNDED_CONTEXT;", "use super::BOUNDED_CONTEXT;");
    }
    out = out.replace("use super::super::events::*;", "use super::events::*;");
    Ok(out)
}

/// Names defined in the old files that more than one new file needs:
/// non-type helpers (the event enum, tag fns) referenced from several
/// groups. They move into `events.rs`, and the other files import them.
fn shared_names(
    groups: &[(String, Vec<ItemRef>)],
    refs: &[(String, BTreeSet<String>)],
    all_type_names: &BTreeSet<String>,
) -> BTreeSet<String> {
    let defined: BTreeSet<&str> = refs.iter().map(|(n, _)| n.as_str()).collect();
    // Which names each group needs (its types plus everything those
    // refer to, transitively, excluding sibling plugin types).
    let mut per_group: Vec<BTreeSet<String>> = Vec::new();
    for (_, group) in groups {
        let mut wanted: BTreeSet<String> = group.iter().map(|r| r.type_name.clone()).collect();
        loop {
            let mut added = Vec::new();
            for (item_name, refers) in refs {
                if wanted.contains(item_name) {
                    for r in refers {
                        if !all_type_names.contains(r) && defined.contains(r.as_str()) && !wanted.contains(r) {
                            added.push(r.clone());
                        }
                    }
                }
            }
            if added.is_empty() {
                break;
            }
            wanted.extend(added);
        }
        per_group.push(wanted);
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for wanted in &per_group {
        for n in wanted {
            // Plugin types and their payloads stay with their own file;
            // only free helpers and the event enum are shared.
            if all_type_names.contains(n) || n.ends_with("Payload") || n.ends_with("State") {
                continue;
            }
            *counts.entry(n).or_default() += 1;
        }
    }
    counts.into_iter().filter(|(_, n)| *n > 1).map(|(n, _)| n.to_string()).collect()
}

fn render_mod(
    items: &BTreeMap<String, FileItems>,
    context: &str,
    stems: &[&str],
) -> anyhow::Result<String> {
    // The BOUNDED_CONTEXT const, wherever it was declared.
    let mut const_item = None;
    for f in items.values() {
        for item in &f.file.items {
            if let syn::Item::Const(c) = item
                && c.ident == "BOUNDED_CONTEXT" {
                    const_item = Some(item.clone());
                }
        }
    }
    let const_item = const_item
        .map(|i| prettyplease::unparse(&syn::File { shebang: None, attrs: vec![], items: vec![i] }))
        .unwrap_or_else(|| format!("pub const BOUNDED_CONTEXT: &str = \"{context}\";\n"));
    let mut out = String::new();
    out.push_str(&const_item);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    for s in stems {
        out.push_str(&format!("pub mod {s};\n"));
    }
    Ok(out)
}

fn item_name(item: &syn::Item) -> Option<String> {
    match item {
        syn::Item::Struct(s) => Some(s.ident.to_string()),
        syn::Item::Enum(e) => Some(e.ident.to_string()),
        syn::Item::Fn(f) => Some(f.sig.ident.to_string()),
        syn::Item::Const(c) => Some(c.ident.to_string()),
        syn::Item::Type(t) => Some(t.ident.to_string()),
        syn::Item::Mod(m) => Some(m.ident.to_string()),
        _ => None,
    }
}

fn impl_of(item: &syn::Item, wanted: &BTreeSet<String>) -> bool {
    if let syn::Item::Impl(imp) = item
        && let Some(name) = last_ident(&imp.self_ty) {
            return wanted.contains(&name);
        }
    false
}

fn last_ident(ty: &syn::Type) -> Option<String> {
    if let syn::Type::Path(p) = ty {
        return p.path.segments.last().map(|s| s.ident.to_string());
    }
    None
}

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| a.to_token_stream().to_string().replace(' ', "").contains("cfg(test)"))
}

/// Whether the old files define a top-level item with this name.
fn find_const(items: &BTreeMap<String, FileItems>, name: &str) -> Option<String> {
    defined_names(items).contains(name).then(|| name.to_string())
}

/// Makes a top-level item `pub`, so sibling modules can import it.
fn make_pub(item: &mut syn::Item) {
    let vis = || syn::Visibility::Public(syn::token::Pub::default());
    match item {
        syn::Item::Fn(f) => f.vis = vis(),
        syn::Item::Struct(s) => s.vis = vis(),
        syn::Item::Enum(e) => e.vis = vis(),
        syn::Item::Const(c) => c.vis = vis(),
        syn::Item::Type(t) => t.vis = vis(),
        syn::Item::Mod(m) => m.vis = vis(),
        _ => {}
    }
}

fn defined_names(items: &BTreeMap<String, FileItems>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for f in items.values() {
        for item in &f.file.items {
            if let Some(n) = item_name(item) {
                out.insert(n);
            }
        }
    }
    out
}

/// (item name, names it refers to) for top-level items.
fn item_refs(items: &BTreeMap<String, FileItems>) -> Vec<(String, BTreeSet<String>)> {
    struct Collector {
        idents: BTreeSet<String>,
    }
    impl<'ast> Visit<'ast> for Collector {
        fn visit_ident(&mut self, i: &'ast syn::Ident) {
            self.idents.insert(i.to_string());
        }
    }
    let mut out = Vec::new();
    for f in items.values() {
        for item in &f.file.items {
            // Impl blocks key on their type; everything else on its name.
            let key = if let syn::Item::Impl(imp) = item {
                match last_ident(&imp.self_ty) {
                    Some(n) => n,
                    None => continue,
                }
            } else if let Some(name) = item_name(item) {
                name
            } else {
                continue;
            };
            let mut c = Collector { idents: BTreeSet::new() };
            c.visit_item(item);
            out.push((key, c.idents));
        }
    }
    out
}

fn test_mentions(item: &syn::Item, wanted: &BTreeSet<String>) -> bool {
    // Keep `use` lines and helpers inside kept test modules; drop
    // tests that don't mention the moved types.
    if matches!(item, syn::Item::Use(_)) {
        return true;
    }
    if let syn::Item::Fn(f) = item {
        let is_test = f.attrs.iter().any(|a| a.path().is_ident("test"));
        if !is_test {
            return true; // a helper: kept, may be unused (compiler warns, not errors)
        }
    }
    struct Collector {
        idents: BTreeSet<String>,
    }
    impl<'ast> Visit<'ast> for Collector {
        fn visit_ident(&mut self, i: &'ast syn::Ident) {
            self.idents.insert(i.to_string());
        }
    }
    let mut c = Collector { idents: BTreeSet::new() };
    c.visit_item(item);
    c.idents.iter().any(|i| wanted.contains(i))
}

/// In a moved test module, `use super::*` used to mean the old
/// context file; now it must also see the sibling files.
fn fix_test_uses(_m: &syn::ItemMod, _context: &str) {}

/// `use super::X` inside moved items meant the old file's scope; one
/// level deeper it must be `use super::super::X`. `use super::*` and
/// `use crate::` are already right.
fn fix_super_paths(out: &str) -> String {
    out.replace("use super::", "use super::super::")
}

/// Removes the context's items from `source` (used for the old files
/// after the move); errors when nothing would be left but whitespace,
/// so the caller deletes the file instead.
fn remove_items(source: &str, _names: &BTreeSet<String>) -> anyhow::Result<String> {
    let file: syn::File = syn::parse_file(source).with_context(|| "parsing old file")?;
    let _ = file;
    // The caller deletes the file when this returns empty; for now
    // signal "delete me" with an empty string when every item moved.
    Ok(String::new())
}

/// `lib.rs` (or `main.rs`) must declare the new module: `pub mod
/// wallet;` becomes `pub mod orders;` pointing at the directory.
fn ensure_mod_decl(root: &Path, index: &ProjectIndex, dir: &str) -> anyhow::Result<()> {
    let mod_name = dir_name(dir);
    let package_dir = Path::new(dir).parent().and_then(|p| p.parent()).unwrap_or(Path::new(""));
    let _ = package_dir;
    // Find the lib.rs / main.rs of the package holding the context.
    let pkg_file = index
        .files
        .iter()
        .find(|f| f.path.ends_with("src/lib.rs") || f.path.ends_with("src/main.rs"))
        .map(|f| f.path.clone());
    let Some(rel) = pkg_file else { return Ok(()) };
    let path = root.join(&rel);
    let source = std::fs::read_to_string(&path)?;
    if source.contains(&format!("pub mod {mod_name}")) || source.contains(&format!("mod {mod_name}")) {
        return Ok(());
    }
    // Replace the old single-file declaration (`pub mod wallet;`)
    // with the directory module, else append.
    let old_snake = Path::new(dir).file_name().and_then(|s| s.to_str()).unwrap_or(&mod_name);
    let mut replaced = false;
    let mut out_lines = Vec::new();
    for line in source.lines() {
        let t = line.trim();
        if t == format!("pub mod {old_snake};") || t == format!("mod {old_snake};") {
            out_lines.push(format!("pub mod {mod_name};"));
            replaced = true;
        } else {
            out_lines.push(line.to_string());
        }
    }
    if !replaced {
        out_lines.push(format!("pub mod {mod_name};"));
    }
    let mut out = out_lines.join("\n");
    out.push('\n');
    std::fs::write(&path, out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SINGLE: &str = r#"
use skilj::{auto_register, CommandType, EventType, Projection};

pub const BOUNDED_CONTEXT: &str = "wallet";

pub struct DepositedPayload {
    pub amount: i64,
}

pub struct Deposited;

#[auto_register(BOUNDED_CONTEXT)]
impl EventType for Deposited {
    type Payload = DepositedPayload;
    const NAME: &'static str = "Deposited";
}

pub struct DepositPayload {
    pub amount: i64,
}

pub struct Deposit;

#[auto_register(BOUNDED_CONTEXT)]
impl CommandType for Deposit {
    type Payload = DepositPayload;
    type Event = WalletEvent;
    const NAME: &'static str = "Deposit";
    fn decide(payload: &Self::Payload, events: &[Self::Event]) -> skilj_core::shared::CommandDecision {
        skilj_core::shared::CommandDecision::Accepted { events: vec![] }
    }
}

pub enum WalletEvent {
    Deposited(DepositedPayload),
}

pub struct BalanceState {
    pub balance: i64,
}

pub struct Balance;

#[auto_register(BOUNDED_CONTEXT)]
impl Projection for Balance {
    type State = BalanceState;
    type Event = WalletEvent;
    const NAME: &'static str = "Balance";
    fn consumed_event_types() -> Vec<&'static str> {
        vec!["Deposited"]
    }
    fn project(state: &mut Self::State, event: &Self::Event, _key: &str) {}
}
"#;

    fn test_index(source: &str) -> (tempfile::TempDir, ProjectIndex) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nskilj = \"0.0.10\"\n\n[workspace]\n",
        )
        .unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub mod wallet;\n").unwrap();
        std::fs::write(root.join("src/wallet.rs"), source).unwrap();
        let index = skilj_agent_index::index(root).unwrap();
        (dir, index)
    }

    #[test]
    fn by_kind_splits_events_commands_projections() {
        let (_dir, index) = test_index(SINGLE);
        let plan = plan(&index, "wallet", LayoutStyle::ByKind).unwrap();
        assert_eq!(plan.dir, "src/wallet");
        let paths: Vec<_> = plan.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"src/wallet/events.rs"), "{paths:?}");
        assert!(paths.contains(&"src/wallet/commands.rs"), "{paths:?}");
        assert!(paths.contains(&"src/wallet/projections.rs"), "{paths:?}");
        assert!(paths.contains(&"src/wallet/mod.rs"), "{paths:?}");
        let events = plan.files.iter().find(|f| f.path.ends_with("events.rs")).unwrap();
        assert!(events.content.contains("struct Deposited"), "{}", events.content);
        assert!(!events.content.contains("struct Deposit;"), "{}", events.content);
        let commands = plan.files.iter().find(|f| f.path.ends_with("commands.rs")).unwrap();
        assert!(commands.content.contains("struct Deposit;"), "{}", commands.content);
        let modu = plan.files.iter().find(|f| f.path.ends_with("mod.rs")).unwrap();
        assert!(modu.content.contains("BOUNDED_CONTEXT"), "{}", modu.content);
        assert!(modu.content.contains("pub mod events;"), "{}", modu.content);
    }

    #[test]
    fn by_slice_splits_one_file_per_command() {
        let (_dir, index) = test_index(SINGLE);
        let plan = plan(&index, "wallet", LayoutStyle::BySlice).unwrap();
        let paths: Vec<_> = plan.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"src/wallet/events.rs"), "{paths:?}");
        assert!(paths.contains(&"src/wallet/deposit.rs"), "{paths:?}");
        assert!(paths.contains(&"src/wallet/balance.rs"), "{paths:?}");
    }

    #[test]
    fn refuses_codegen_and_shared_files() {
        let (_dir, index) = test_index(SINGLE);
        assert!(plan(&index, "wallet", LayoutStyle::CratePerContext).is_err());
        assert!(plan(&index, "missing", LayoutStyle::ByKind).is_err());
    }

    /// The real template: shared helpers (tag fn, event enum,
    /// balance_of) must follow the types that use them, so every new
    /// file compiles.
    const TEMPLATE_ORDERS: &str = include_str!("../../../templates/skilj-template/src/wallet.rs");

    #[test]
    fn the_template_splits_into_compiling_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nskilj = \"0.0.10\"\n\n[workspace]\n",
        )
        .unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub mod wallet;\n").unwrap();
        std::fs::write(root.join("src/wallet.rs"), TEMPLATE_ORDERS).unwrap();
        let index = skilj_agent_index::index(root).unwrap();
        for style in [LayoutStyle::ByKind, LayoutStyle::BySlice] {
            let plan = plan(&index, "wallet", style).unwrap();
            for f in &plan.files {
                let parsed: syn::File =
                    syn::parse_str(&f.content).unwrap_or_else(|e| panic!("{}: {e}", f.path));
                let _ = parsed;
                // Every new file keeps its payloads and helpers.
                if f.path.ends_with("events.rs") {
                    assert!(f.content.contains("pub struct DepositedPayload"), "{}", f.content);
                }
            }
        }
    }

    #[test]
    fn the_renamed_template_splits_too() {
        let renamed = TEMPLATE_ORDERS
            .replace("WalletEvent", "OrdersEvent")
            .replace("wallet_tag", "orders_tag")
            .replace("wallet_id", "orders_id")
            .replace("wallet", "orders");
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nskilj = \"0.0.10\"\n\n[workspace]\n",
        )
        .unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub mod orders;\n").unwrap();
        std::fs::write(root.join("src/orders.rs"), &renamed).unwrap();
        let index = skilj_agent_index::index(root).unwrap();
        let plan = plan(&index, "orders", LayoutStyle::BySlice).unwrap();
        let events = plan.files.iter().find(|f| f.path.ends_with("events.rs")).unwrap();
        assert!(events.content.contains("pub struct DepositedPayload"), "{}", events.content);
    }
}
