//! Items in a file by name, with their line ranges, so a task can show
//! the model exactly the code it needs (spec: ContextIsChosenByCode).

use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use std::collections::BTreeSet;
use syn::spanned::Spanned;
use syn::{Attribute, Item};

#[derive(Debug, Clone)]
pub struct SourceItem {
    /// The struct, enum, fn, const or type name; for an impl block, the
    /// type it is for.
    pub name: String,
    pub kind: ItemKind,
    /// 1-based, including attributes and doc comments.
    pub start: usize,
    pub end: usize,
    /// The `#[cfg(test)]` module the item is in, if any.
    pub test_module: Option<String>,
    /// Enums: each variant with the type names in its fields, so a
    /// variant used in code can pull in just its own payload.
    pub variants: Vec<(String, BTreeSet<String>)>,
    /// Impls: each method with its lines, and whether the impl is of a
    /// trait.
    pub methods: Vec<(String, usize, usize)>,
    pub trait_impl: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Struct,
    Enum,
    Fn,
    Const,
    TypeAlias,
    Impl,
    /// A `#[cfg(test)]` module; its items are listed separately.
    TestModule,
}

/// Every item in the file, nested modules included. Items in a module
/// that isn't `#[cfg(test)]` are listed like top-level ones.
pub fn items(source: &str) -> syn::Result<Vec<SourceItem>> {
    let file = syn::parse_file(source)?;
    let mut out = Vec::new();
    walk(&file.items, None, &mut out);
    Ok(out)
}

fn walk(items: &[Item], test_module: Option<&str>, out: &mut Vec<SourceItem>) {
    for item in items {
        let mut methods = Vec::new();
        let mut trait_impl = false;
        let (name, kind, attrs, variants) = match item {
            Item::Struct(s) => (s.ident.to_string(), ItemKind::Struct, &s.attrs, Vec::new()),
            Item::Enum(e) => (
                e.ident.to_string(),
                ItemKind::Enum,
                &e.attrs,
                e.variants
                    .iter()
                    .map(|v| (v.ident.to_string(), idents(v.fields.to_token_stream())))
                    .collect(),
            ),
            Item::Fn(f) => (f.sig.ident.to_string(), ItemKind::Fn, &f.attrs, Vec::new()),
            Item::Const(c) => (c.ident.to_string(), ItemKind::Const, &c.attrs, Vec::new()),
            Item::Type(t) => (t.ident.to_string(), ItemKind::TypeAlias, &t.attrs, Vec::new()),
            Item::Impl(i) => match crate::rust::last_type_ident(&i.self_ty) {
                Some(name) => {
                    trait_impl = i.trait_.is_some();
                    for ii in &i.items {
                        if let syn::ImplItem::Fn(m) = ii {
                            let start = m.attrs.iter().map(|a| a.span().start().line).chain([m.span().start().line]).min().unwrap_or(1);
                            methods.push((m.sig.ident.to_string(), start, m.span().end().line));
                        }
                    }
                    (name, ItemKind::Impl, &i.attrs, Vec::new())
                }
                None => continue,
            },
            Item::Mod(m) => {
                let Some((_, inner)) = &m.content else { continue };
                if crate::rust::is_cfg_test(&m.attrs) {
                    let name = m.ident.to_string();
                    out.push(SourceItem {
                        name: name.clone(),
                        kind: ItemKind::TestModule,
                        start: start_line(&m.attrs, item),
                        end: item.span().end().line,
                        test_module: test_module.map(String::from),
                        variants: Vec::new(),
                        methods: Vec::new(),
                        trait_impl: false,
                    });
                    walk(inner, Some(&name), out);
                } else {
                    walk(inner, test_module, out);
                }
                continue;
            }
            _ => continue,
        };
        out.push(SourceItem {
            name,
            kind,
            start: start_line(attrs, item),
            end: item.span().end().line,
            test_module: test_module.map(String::from),
            variants,
            methods,
            trait_impl,
        });
    }
}

fn start_line(attrs: &[Attribute], item: &Item) -> usize {
    attrs
        .iter()
        .map(|a| a.span().start().line)
        .chain([item.span().start().line])
        .min()
        .unwrap_or(1)
}

/// Every identifier in a piece of Rust code; empty when it doesn't
/// tokenize.
pub fn idents_in(code: &str) -> BTreeSet<String> {
    code.parse::<TokenStream>().map(idents).unwrap_or_default()
}

fn idents(tokens: TokenStream) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect(tokens, &mut out);
    out
}

fn collect(tokens: TokenStream, out: &mut BTreeSet<String>) {
    for t in tokens {
        match t {
            TokenTree::Ident(i) => {
                out.insert(i.to_string());
            }
            TokenTree::Group(g) => collect(g.stream(), out),
            _ => {}
        }
    }
}

/// Lines `start..=end` (1-based) of `source`.
pub fn lines(source: &str, start: usize, end: usize) -> String {
    source
        .lines()
        .skip(start.saturating_sub(1))
        .take(end + 1 - start.max(1))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"
/// A payload.
#[derive(Debug)]
pub struct WithdrawPayload { pub amount: i64 }

pub enum WalletEvent {
    Deposited(DepositedPayload),
    Withdrawn(WithdrawnPayload),
}

impl CommandType for Withdraw {
    fn decide() {}
}

#[cfg(test)]
mod tests {
    use super::*;
    fn helper() {}
    #[test]
    fn a_test() {}
}
"#;

    #[test]
    fn lists_items_with_their_lines() {
        let items = items(SOURCE).unwrap();
        let payload = items.iter().find(|i| i.name == "WithdrawPayload").unwrap();
        assert_eq!((payload.start, payload.end), (2, 4));
        let event = items.iter().find(|i| i.name == "WalletEvent").unwrap();
        assert_eq!(event.variants[1].0, "Withdrawn");
        assert!(event.variants[1].1.contains("WithdrawnPayload"));
        let imp = items.iter().find(|i| i.kind == ItemKind::Impl).unwrap();
        assert_eq!(imp.name, "Withdraw");
        assert!(imp.trait_impl);
        assert_eq!(imp.methods, vec![("decide".to_string(), 12, 12)]);
        let module = items.iter().find(|i| i.kind == ItemKind::TestModule).unwrap();
        assert_eq!((module.name.as_str(), module.start, module.end), ("tests", 15, 21));
        let helper = items.iter().find(|i| i.name == "helper").unwrap();
        assert_eq!(helper.test_module.as_deref(), Some("tests"));
        assert_eq!(lines(SOURCE, 2, 4).lines().count(), 3);
    }
}
