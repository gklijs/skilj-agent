//! What a piece of Rust code mentions: string literals, identifiers,
//! `SomeType::NAME` references and `CommandDecision::Rejected` kinds.
//! Looks inside macro calls too (`assert_eq!`, `json!`, `vec!`, ...),
//! because that's where tests keep most of their names.

use proc_macro2::{TokenStream, TokenTree};
use std::collections::{BTreeMap, BTreeSet};
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{Expr, LitStr, Token};

#[derive(Debug, Default, Clone)]
pub(crate) struct Facts {
    pub literals: BTreeSet<String>,
    pub idents: BTreeSet<String>,
    /// `X` for every `X::NAME` path.
    pub name_refs: BTreeSet<String>,
    /// The `kind` of every `CommandDecision::Rejected { .. }` built here.
    pub rejections: BTreeSet<String>,
    /// A `Rejected` whose kind isn't a literal or a string const.
    pub dynamic_rejection: bool,
    /// Every function or method this code calls.
    pub calls: BTreeSet<Call>,
}

/// A call, as precisely as syntax tells: the index resolves these to
/// functions in the project to follow `decide()` into its helpers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Call {
    /// `f(..)`, or `module::f(..)`.
    Free(String),
    /// `Self::f(..)` or `self.f(..)`.
    OnSelf(String),
    /// `Type::f(..)`.
    OnType(String, String),
    /// `x.f(..)`: the receiver's type is unknown.
    Method(String),
}

impl Facts {
    /// Facts that only say "this calls `function`", to start following
    /// calls from a name.
    pub fn calling(function: &str) -> Self {
        let mut facts = Facts::default();
        facts.calls.insert(Call::Free(function.to_string()));
        facts
    }

    pub fn extend(&mut self, other: &Facts) {
        self.literals.extend(other.literals.iter().cloned());
        self.idents.extend(other.idents.iter().cloned());
        self.name_refs.extend(other.name_refs.iter().cloned());
        self.rejections.extend(other.rejections.iter().cloned());
        self.dynamic_rejection |= other.dynamic_rejection;
        self.calls.extend(other.calls.iter().cloned());
    }
}

pub(crate) struct Collector<'a> {
    pub facts: Facts,
    /// `const X: &str = "..."` in the file, to resolve `kind: X.into()`.
    pub consts: &'a BTreeMap<String, String>,
}

impl<'a> Collector<'a> {
    pub fn new(consts: &'a BTreeMap<String, String>) -> Self {
        Collector { facts: Facts::default(), consts }
    }

    fn scan_tokens(&mut self, tokens: TokenStream) {
        for tree in tokens {
            match tree {
                TokenTree::Group(g) => self.scan_tokens(g.stream()),
                TokenTree::Ident(i) => {
                    self.facts.idents.insert(i.to_string());
                }
                TokenTree::Literal(l) => {
                    if let Ok(s) = syn::parse2::<LitStr>(TokenTree::Literal(l).into()) {
                        self.facts.literals.insert(s.value());
                    }
                }
                TokenTree::Punct(_) => {}
            }
        }
    }

    fn rejection_kind(&self, expr: &Expr) -> Option<String> {
        let mut first = FirstLitStr(None);
        first.visit_expr(expr);
        if let Some(s) = first.0 {
            return Some(s);
        }
        let mut ident = SingleIdentPath(None);
        ident.visit_expr(expr);
        ident.0.and_then(|i| self.consts.get(&i).cloned())
    }
}

impl<'ast> Visit<'ast> for Collector<'_> {
    fn visit_lit_str(&mut self, lit: &'ast LitStr) {
        self.facts.literals.insert(lit.value());
    }

    fn visit_ident(&mut self, ident: &'ast proc_macro2::Ident) {
        self.facts.idents.insert(ident.to_string());
    }

    fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
        let segments: Vec<_> = path.path.segments.iter().collect();
        if segments.len() >= 2 && segments[segments.len() - 1].ident == "NAME" {
            self.facts.name_refs.insert(segments[segments.len() - 2].ident.to_string());
        }
        visit::visit_expr_path(self, path);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let Expr::Path(p) = &*call.func {
            let segments: Vec<String> = p.path.segments.iter().map(|s| s.ident.to_string()).collect();
            let name = segments[segments.len() - 1].clone();
            let call = match segments.len() {
                1 => Call::Free(name),
                n => match segments[n - 2].as_str() {
                    "Self" => Call::OnSelf(name),
                    t if t.starts_with(char::is_uppercase) => Call::OnType(t.to_string(), name),
                    _ => Call::Free(name),
                },
            };
            self.facts.calls.insert(call);
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        let on_self = matches!(&*call.receiver, Expr::Path(p) if p.path.is_ident("self"));
        let name = call.method.to_string();
        self.facts.calls.insert(if on_self { Call::OnSelf(name) } else { Call::Method(name) });
        visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_struct(&mut self, s: &'ast syn::ExprStruct) {
        if s.path.segments.last().is_some_and(|seg| seg.ident == "Rejected") {
            for field in &s.fields {
                if matches!(&field.member, syn::Member::Named(n) if n == "kind") {
                    match self.rejection_kind(&field.expr) {
                        Some(kind) => {
                            self.facts.rejections.insert(kind);
                        }
                        None => self.facts.dynamic_rejection = true,
                    }
                }
            }
        }
        visit::visit_expr_struct(self, s);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.scan_tokens(mac.tokens.clone());
        // Comma-separated expressions (vec!, assert_eq!, format!, ...)
        // are visited properly too, to find Rejected and X::NAME in them.
        if let Ok(exprs) = mac.parse_body_with(Punctuated::<Expr, Token![,]>::parse_terminated) {
            for e in &exprs {
                self.visit_expr(e);
            }
        }
        visit::visit_macro(self, mac);
    }
}

struct FirstLitStr(Option<String>);

impl<'ast> Visit<'ast> for FirstLitStr {
    fn visit_lit_str(&mut self, lit: &'ast LitStr) {
        if self.0.is_none() {
            self.0 = Some(lit.value());
        }
    }
}

/// `X` in `X`, `X.into()`, `X.to_string()`, `String::from(X)`.
struct SingleIdentPath(Option<String>);

impl<'ast> Visit<'ast> for SingleIdentPath {
    fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
        if self.0.is_none() && path.path.segments.len() == 1 {
            self.0 = Some(path.path.segments[0].ident.to_string());
        }
    }
}
