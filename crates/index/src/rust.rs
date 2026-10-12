//! One Rust source file, reduced to what the index needs: its
//! `BOUNDED_CONTEXT`, the skilj plugin trait impls, every function with
//! the facts of its body, and the coverage markers in its comments.

use crate::TypeKind;
pub(crate) use crate::facts::Facts;
use crate::facts::Collector;
use std::collections::{BTreeMap, BTreeSet};
use syn::visit::Visit;
use syn::{Attribute, Expr, ImplItem, Item, Lit};

pub(crate) struct FileModel {
    pub bounded_context: Option<String>,
    pub impls: Vec<ImplModel>,
    pub fns: Vec<FnModel>,
    /// (line, obligation id) from `// skilj-agent: covers <id>` and
    /// `// allium: <id>` comments.
    markers: Vec<(usize, String)>,
    pub warnings: Vec<String>,
}

pub(crate) struct ImplModel {
    pub kind: TypeKind,
    pub rust_name: String,
    pub name: Option<String>,
    pub context: ContextRef,
    pub line: usize,
    /// Facts of `decide`, `decide_with` and `decide_from_snapshot`.
    pub decide: Facts,
    /// Literals and `X::NAME` refs from `consumed_event_types()`.
    pub consumed: BTreeSet<String>,
    pub state_type: Option<String>,
}

pub(crate) enum ContextRef {
    /// `#[auto_register("ctx")]` or `const BOUNDED_CONTEXT: &str = "ctx"`
    /// in the impl itself.
    Literal(String),
    /// `#[auto_register(BOUNDED_CONTEXT)]`, or nothing: the module's own.
    Inherited,
}

pub(crate) struct FnModel {
    /// Function or method name; methods of all impls are included so
    /// calls like `self.reject_unless_requester(..)` can be followed.
    pub name: String,
    /// For methods: the type of the impl block.
    pub owner: Option<String>,
    pub line: usize,
    pub end_line: usize,
    pub is_test: bool,
    /// Inside a `#[cfg(test)]` module: a test helper, never followed
    /// from production code.
    pub test_only: bool,
    pub facts: Facts,
}

impl FileModel {
    pub fn markers_between(&self, start: usize, end: usize) -> BTreeSet<String> {
        self.markers
            .iter()
            .filter(|(line, _)| (start..=end).contains(line))
            .map(|(_, id)| id.clone())
            .collect()
    }
}

pub(crate) fn parse(source: &str) -> syn::Result<FileModel> {
    let file = syn::parse_file(source)?;
    let mut consts = BTreeMap::new();
    collect_consts(&file.items, &mut consts);
    let mut model = FileModel {
        bounded_context: consts.get("BOUNDED_CONTEXT").cloned(),
        impls: Vec::new(),
        fns: Vec::new(),
        markers: markers(source),
        warnings: Vec::new(),
    };
    walk(&file.items, &consts, false, &mut model);
    Ok(model)
}

const TRAITS: [(&str, TypeKind); 4] = [
    ("EventType", TypeKind::Event),
    ("CommandType", TypeKind::Command),
    ("Projection", TypeKind::Projection),
    ("Snapshot", TypeKind::Snapshot),
];

const DECIDE_FNS: [&str; 3] = ["decide", "decide_with", "decide_from_snapshot"];

fn walk(items: &[Item], consts: &BTreeMap<String, String>, test_only: bool, model: &mut FileModel) {
    for item in items {
        match item {
            Item::Fn(f) => model.fns.push(FnModel {
                name: f.sig.ident.to_string(),
                owner: None,
                line: f.sig.ident.span().start().line,
                end_line: f.block.brace_token.span.close().end().line,
                is_test: is_test(&f.attrs),
                test_only,
                facts: facts_of(|c| c.visit_block(&f.block), consts),
            }),
            Item::Impl(imp) => {
                let owner = last_type_ident(&imp.self_ty);
                for ii in &imp.items {
                    if let ImplItem::Fn(m) = ii {
                        model.fns.push(FnModel {
                            name: m.sig.ident.to_string(),
                            owner: owner.clone(),
                            line: m.sig.ident.span().start().line,
                            end_line: m.block.brace_token.span.close().end().line,
                            is_test: false,
                            test_only,
                            facts: facts_of(|c| c.visit_block(&m.block), consts),
                        });
                    }
                }
                if let Some(plugin) = plugin_impl(imp, consts) {
                    if plugin.kind == TypeKind::Command && plugin.decide.dynamic_rejection {
                        model.warnings.push(format!(
                            "{}: a Rejected kind that isn't a literal or a string const",
                            plugin.rust_name
                        ));
                    }
                    model.impls.push(plugin);
                }
            }
            Item::Mod(m) => {
                if let Some((_, items)) = &m.content {
                    walk(items, consts, test_only || is_cfg_test(&m.attrs), model);
                }
            }
            _ => {}
        }
    }
}

fn plugin_impl(imp: &syn::ItemImpl, consts: &BTreeMap<String, String>) -> Option<ImplModel> {
    let (_, trait_path, _) = imp.trait_.as_ref()?;
    let trait_name = trait_path.segments.last()?.ident.to_string();
    let kind = TRAITS.iter().find(|(n, _)| *n == trait_name)?.1;
    let rust_name = last_type_ident(&imp.self_ty)?;

    let mut model = ImplModel {
        kind,
        rust_name,
        name: None,
        context: ContextRef::Inherited,
        line: imp.impl_token.span.start().line,
        decide: Facts::default(),
        consumed: BTreeSet::new(),
        state_type: None,
    };
    for attr in &imp.attrs {
        if attr.path().segments.last().is_some_and(|s| s.ident == "auto_register")
            && let Ok(Expr::Lit(lit)) = attr.parse_args::<Expr>()
                && let Lit::Str(s) = lit.lit {
                    model.context = ContextRef::Literal(s.value());
                }
    }
    for ii in &imp.items {
        match ii {
            ImplItem::Const(c) if c.ident == "NAME" => model.name = str_lit(&c.expr),
            ImplItem::Const(c) if c.ident == "BOUNDED_CONTEXT" => {
                if let Some(s) = str_lit(&c.expr) {
                    model.context = ContextRef::Literal(s);
                }
            }
            ImplItem::Type(t) if t.ident == "State" => model.state_type = last_type_ident(&t.ty),
            ImplItem::Fn(m) if DECIDE_FNS.contains(&m.sig.ident.to_string().as_str()) => {
                model.decide.extend(&facts_of(|c| c.visit_block(&m.block), consts));
            }
            ImplItem::Fn(m) if m.sig.ident == "consumed_event_types" => {
                let facts = facts_of(|c| c.visit_block(&m.block), consts);
                model.consumed = facts.literals.into_iter().chain(facts.name_refs).collect();
            }
            _ => {}
        }
    }
    Some(model)
}

fn facts_of(visit: impl FnOnce(&mut Collector), consts: &BTreeMap<String, String>) -> Facts {
    let mut collector = Collector::new(consts);
    visit(&mut collector);
    collector.facts
}

/// Every `const X: &str = "..."`, at any module depth.
fn collect_consts(items: &[Item], consts: &mut BTreeMap<String, String>) {
    for item in items {
        match item {
            Item::Const(c) => {
                if let Some(s) = str_lit(&c.expr) {
                    consts.insert(c.ident.to_string(), s);
                }
            }
            Item::Mod(m) => {
                if let Some((_, items)) = &m.content {
                    collect_consts(items, consts);
                }
            }
            _ => {}
        }
    }
}

fn str_lit(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Lit(syn::ExprLit { lit: Lit::Str(s), .. }) => Some(s.value()),
        _ => None,
    }
}

pub(crate) fn last_type_ident(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

pub(crate) fn is_cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg") && a.parse_args::<syn::Ident>().is_ok_and(|i| i == "test")
    })
}

/// `#[test]`, `#[tokio::test]`, `#[sqlx::test]`, ...
fn is_test(attrs: &[Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| a.path().segments.last().is_some_and(|s| s.ident == "test"))
}

fn markers(source: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (i, line) in source.lines().enumerate() {
        let Some(comment) = line.find("//").map(|at| line[at + 2..].trim()) else {
            continue;
        };
        let id = comment
            .strip_prefix("skilj-agent: covers ")
            .or_else(|| comment.strip_prefix("allium: "));
        if let Some(id) = id.and_then(|rest| rest.split_whitespace().next()) {
            found.push((i + 1, id.to_string()));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMAND: &str = r#"
        pub const BOUNDED_CONTEXT: &str = "courses";
        const FULL: &str = "course_full";

        pub struct EnrollStudent;

        #[auto_register(BOUNDED_CONTEXT)]
        impl CommandType for EnrollStudent {
            type Payload = EnrollStudentPayload;
            type Event = CoursesEvent;
            const NAME: &'static str = "EnrollStudent";

            fn decide(payload: &Self::Payload, events: &[Self::Event]) -> CommandDecision {
                if payload.capacity == 0 {
                    return CommandDecision::Rejected {
                        reason: "capacity must be positive".into(),
                        kind: "invalid_capacity".into(),
                    };
                }
                if let Some(rejection) = check_open(events) {
                    return rejection;
                }
                if full(events) {
                    return CommandDecision::Rejected { reason: format!("full"), kind: FULL.into() };
                }
                CommandDecision::Accepted { events: vec![] }
            }
        }

        fn check_open(events: &[CoursesEvent]) -> Option<CommandDecision> {
            Some(CommandDecision::Rejected { reason: "x".into(), kind: "course_not_found".to_string() })
        }

        pub struct Roster;

        #[auto_register(BOUNDED_CONTEXT)]
        impl Projection for Roster {
            type State = RosterState;
            type Event = CoursesEvent;
            const NAME: &'static str = "Roster";
            fn consumed_event_types() -> Vec<&'static str> {
                vec!["StudentEnrolled", CourseOpened::NAME]
            }
            fn project(state: &mut Self::State, event: &Self::Event, key: &str) {}
        }

        #[cfg(test)]
        mod tests {
            fn decide_for(p: EnrollStudentPayload) -> CommandDecision {
                EnrollStudent::decide(&p, &[])
            }

            #[test]
            fn rejects_zero_capacity() {
                // skilj-agent: covers command.EnrollStudent.rejected.invalid_capacity
                assert_eq!(kind(decide_for(payload(0))), Some("invalid_capacity".into()));
            }
        }
    "#;

    #[test]
    fn finds_context_types_and_direct_rejections() {
        let file = parse(COMMAND).unwrap();
        assert_eq!(file.bounded_context.as_deref(), Some("courses"));
        let command = file.impls.iter().find(|i| i.kind == TypeKind::Command).unwrap();
        assert_eq!(command.rust_name, "EnrollStudent");
        assert_eq!(command.name.as_deref(), Some("EnrollStudent"));
        assert!(matches!(command.context, ContextRef::Inherited));
        // check_open's kind is found by the index following the call,
        // not here: only decide()'s own rejections are in its facts.
        assert_eq!(
            command.decide.rejections,
            BTreeSet::from(["invalid_capacity".to_string(), "course_full".to_string()])
        );
        assert!(command.decide.calls.contains(&crate::facts::Call::Free("check_open".into())));
    }

    #[test]
    fn reads_projection_consumed_events_and_state() {
        let file = parse(COMMAND).unwrap();
        let projection = file.impls.iter().find(|i| i.kind == TypeKind::Projection).unwrap();
        assert_eq!(
            projection.consumed,
            BTreeSet::from(["StudentEnrolled".to_string(), "CourseOpened".to_string()])
        );
        assert_eq!(projection.state_type.as_deref(), Some("RosterState"));
    }

    #[test]
    fn finds_tests_helpers_and_markers() {
        let file = parse(COMMAND).unwrap();
        let test = file.fns.iter().find(|f| f.is_test).unwrap();
        assert_eq!(test.name, "rejects_zero_capacity");
        assert!(test.facts.literals.contains("invalid_capacity"));
        assert!(test.facts.idents.contains("decide_for"));
        assert_eq!(
            file.markers_between(test.line, test.end_line),
            BTreeSet::from(["command.EnrollStudent.rejected.invalid_capacity".to_string()])
        );
    }

    #[test]
    fn literal_context_in_auto_register() {
        let file = parse(
            r#"
            #[skilj::auto_register("wallet")]
            impl EventType for Deposited { const NAME: &'static str = "Deposited"; }
            "#,
        )
        .unwrap();
        assert!(matches!(&file.impls[0].context, ContextRef::Literal(c) if c == "wallet"));
    }
}
