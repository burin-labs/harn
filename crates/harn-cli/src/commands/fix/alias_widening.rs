//! Widen the function-type alias that describes a value-referenced callable.
//!
//! A callable whose value is read as a first-class reference keeps its arity
//! (#6146): the reference is dispatched at the declared arity through a call
//! site no static pass can see. When the *only* thing that reads the value is a
//! parameter default governed by a local `type X = fn(...)`, that call site is
//! not invisible at all — the alias is what fixes the arity, and moving both
//! together is a mechanical edit the migration should make rather than refuse
//! (#6153).
//!
//! Every condition below exists to keep this from becoming a half-sound
//! rewriter, which would be worse than the refusal. The analysis permits a
//! widening only when it can see the alias's *entire* use, and anything it
//! cannot account for falls back to freezing the callable.

use std::collections::{BTreeMap, BTreeSet};

use harn_lexer::{FixEdit, Span};
use harn_parser::{visit, Node, SNode, TypeExpr};

use super::signature_threading::{add_call_argument_edit, prepend_list_item};

/// Which callables may be re-signed because their governing alias moves with
/// them, and the edit that moves each alias.
#[derive(Debug, Default)]
pub(super) struct AliasWidening {
    by_callable: BTreeMap<String, String>,
    edits: BTreeMap<String, Vec<FixEdit>>,
}

impl AliasWidening {
    /// Whether this callable's value references are all governed by an alias
    /// the migration may move.
    pub(super) fn covers(&self, callable: &str) -> bool {
        self.by_callable.contains_key(callable)
    }

    /// The edits that must travel with this callable's new parameter: the
    /// alias declaration, and every call dispatched through a value the alias
    /// types. Widening the alias without them leaves a program that only fails
    /// at run time — `harn check` does not report a value call's arity.
    pub(super) fn edits_for(&self, callable: &str) -> &[FixEdit] {
        self.by_callable
            .get(callable)
            .and_then(|alias| self.edits.get(alias))
            .map_or(&[], Vec::as_slice)
    }

    pub(super) fn analyze(
        program: &[SNode],
        source: &str,
        referenced_by_value: &BTreeSet<String>,
    ) -> Self {
        let aliases = local_fn_aliases(program);
        if aliases.is_empty() {
            return Self::default();
        }
        let defaults = parameter_default_sites(program);
        let identifier_spans = identifier_spans(program);

        // A callable qualifies only when every value read of its name is one of
        // its own parameter-default sites, and every one of those sites is
        // annotated with the same local `fn(...)` alias. One unaccounted read
        // — a list entry, a dict field, an argument — is a dispatch this pass
        // cannot see, which is the case #6146 froze.
        let mut by_callable: BTreeMap<String, String> = BTreeMap::new();
        for callable in referenced_by_value {
            let sites: Vec<&DefaultSite> = defaults
                .iter()
                .filter(|site| &site.callable == callable)
                .collect();
            if sites.is_empty() {
                continue;
            }
            let site_spans: BTreeSet<usize> =
                sites.iter().map(|site| site.value_span.start).collect();
            let all_reads_are_defaults = identifier_spans
                .get(callable)
                .is_some_and(|spans| spans.iter().all(|span| site_spans.contains(&span.start)))
                && identifier_spans.get(callable).map_or(0, Vec::len) == sites.len();
            if !all_reads_are_defaults {
                continue;
            }
            let mut governing = None;
            let consistent = sites.iter().all(|site| match &site.declared {
                Some(TypeExpr::Named(alias)) if aliases.contains_key(alias) => {
                    let first = governing.get_or_insert(alias.clone());
                    first == alias
                }
                _ => false,
            });
            if let (true, Some(alias)) = (consistent, governing) {
                by_callable.insert(callable.clone(), alias);
            }
        }

        // The load-bearing guard. The alias may not be named anywhere this pass
        // did not reason about: a variable annotation, a record field, a return
        // position, or another parameter whose default is a callable that is
        // NOT moving. Counting textual occurrences over-counts (a mention in a
        // comment or a string counts too) and therefore only ever refuses.
        let candidates = by_callable.clone();
        by_callable.retain(|_, alias| {
            let Some(decl) = aliases.get(alias) else {
                return false;
            };
            if decl.is_pub {
                // An exported alias can be named by a file this pass never saw.
                return false;
            }
            let governed: Vec<&DefaultSite> = defaults
                .iter()
                .filter(
                    |site| matches!(&site.declared, Some(TypeExpr::Named(name)) if name == alias),
                )
                .collect();
            governed
                .iter()
                .all(|site| candidates.contains_key(&site.callable))
                && word_occurrences(source, alias) == governed.len() + 1
        });

        // Moving the alias without moving the calls dispatched through it
        // leaves a program that type-checks and then fails at run time with
        // `Arity mismatch`, because a value call's arity is not checked
        // statically. Either every such call can be updated, or the alias does
        // not move.
        let edits = by_callable
            .values()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|alias| {
                let decl = aliases.get(alias)?;
                let mut alias_edits = vec![alias_widening_edit(source, decl)?];
                alias_edits.extend(dispatch_argument_edits(program, source, alias)?);
                Some((alias.clone(), alias_edits))
            })
            .collect::<BTreeMap<_, _>>();
        by_callable.retain(|_, alias| edits.contains_key(alias));
        Self { by_callable, edits }
    }
}

/// One `type X = fn(...) -> R` declared in this module.
#[derive(Debug)]
struct AliasDecl {
    span: Span,
    is_pub: bool,
}

fn local_fn_aliases(program: &[SNode]) -> BTreeMap<String, AliasDecl> {
    let mut aliases = BTreeMap::new();
    for node in program {
        let inner = match &node.node {
            Node::AttributedDecl { inner, .. } => inner.as_ref(),
            _ => node,
        };
        // A generic alias would need its parameters bound before the widened
        // shape means anything, so it is not a mechanical edit.
        if let Node::TypeDecl {
            name,
            type_params,
            type_expr: TypeExpr::FnType { .. },
            is_pub,
        } = &inner.node
        {
            if type_params.is_empty() {
                aliases.insert(
                    name.clone(),
                    AliasDecl {
                        span: inner.span,
                        is_pub: *is_pub,
                    },
                );
            }
        }
    }
    aliases
}

/// A parameter whose default value is exactly one callable's name.
#[derive(Debug)]
struct DefaultSite {
    callable: String,
    declared: Option<TypeExpr>,
    value_span: Span,
}

fn parameter_default_sites(program: &[SNode]) -> Vec<DefaultSite> {
    let mut sites = Vec::new();
    visit::walk_program(program, &mut |node| {
        let params = match &node.node {
            Node::FnDecl { params, .. }
            | Node::ToolDecl { params, .. }
            | Node::Pipeline { params, .. }
            | Node::Closure { params, .. } => params,
            _ => return,
        };
        for param in params {
            let Some(default) = &param.default_value else {
                continue;
            };
            let Node::Identifier(callable) = &default.node else {
                continue;
            };
            sites.push(DefaultSite {
                callable: callable.clone(),
                declared: param.type_expr.clone(),
                value_span: default.span,
            });
        }
    });
    sites
}

fn identifier_spans(program: &[SNode]) -> BTreeMap<String, Vec<Span>> {
    let mut spans: BTreeMap<String, Vec<Span>> = BTreeMap::new();
    visit::walk_program(program, &mut |node| {
        if let Node::Identifier(name) = &node.node {
            spans.entry(name.clone()).or_default().push(node.span);
        }
    });
    spans
}

/// Count whole-word occurrences of `name` in the source.
///
/// Deliberately textual: it counts a mention in a comment or a string the same
/// as a type position, so the count can only come out too high. Too high means
/// the widening is refused, which is the safe direction.
fn word_occurrences(source: &str, name: &str) -> usize {
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let bytes = source.as_bytes();
    source
        .match_indices(name)
        .filter(|(index, _)| {
            let before_ok = *index == 0 || !is_word(bytes[index - 1]);
            let after = index + name.len();
            let after_ok = after >= bytes.len() || !is_word(bytes[after]);
            before_ok && after_ok
        })
        .count()
}

/// Insert `Harness` as the alias's first parameter type.
fn alias_widening_edit(source: &str, decl: &AliasDecl) -> Option<FixEdit> {
    let region = source.get(decl.span.start..decl.span.end)?;
    let fn_at = region.find("fn(")?;
    let open_paren = decl.span.start + fn_at + 3;
    let close_paren = region[fn_at + 3..].find(')')? + fn_at + 3 + decl.span.start;
    let has_params = !source.get(open_paren..close_paren)?.trim().is_empty();
    Some(FixEdit {
        span: Span::with_offsets(open_paren, open_paren, decl.span.line, decl.span.column),
        replacement: prepend_list_item(source, open_paren, "Harness", has_params),
    })
}

/// Every call made through a value the alias types, with the capability
/// argument its enclosing scope supplies.
///
/// `None` refuses the whole widening. That happens when a binding of the alias
/// type is read anywhere other than as a call target — passed on, stored,
/// returned — because that read escapes into a shape this pass cannot follow,
/// or when the enclosing callable has no root `Harness` to pass.
fn dispatch_argument_edits(program: &[SNode], source: &str, alias: &str) -> Option<Vec<FixEdit>> {
    let mut edits = Vec::new();
    let mut refused = false;
    visit::walk_program(program, &mut |node| {
        if refused {
            return;
        }
        let (params, body) = match &node.node {
            Node::FnDecl { params, body, .. }
            | Node::ToolDecl { params, body, .. }
            | Node::Pipeline { params, body, .. }
            | Node::Closure { params, body, .. } => (params, body),
            _ => return,
        };
        let bindings: Vec<&str> = params
            .iter()
            .filter(
                |param| matches!(&param.type_expr, Some(TypeExpr::Named(name)) if name == alias),
            )
            .map(|param| param.name.as_str())
            .collect();
        if bindings.is_empty() {
            return;
        }
        let Some(harness) = params
            .iter()
            .find(|param| {
                matches!(&param.type_expr, Some(TypeExpr::Named(name)) if name == "Harness")
            })
            .map(|param| param.name.clone())
        else {
            refused = true;
            return;
        };
        for binding in bindings {
            let mut call_spans = BTreeSet::new();
            let mut reads = Vec::new();
            visit::walk_program(body, &mut |child| {
                match &child.node {
                    // A bare `resolver(...)` parses as a `FunctionCall` whose
                    // callee is a name field, not a child node — so it never
                    // shows up as an identifier read.
                    Node::FunctionCall { name, .. } if name == binding => {
                        edits.push((child.span, harness.clone()));
                    }
                    Node::ValueCall { callee, .. } => {
                        if matches!(&callee.node, Node::Identifier(name) if name == binding) {
                            call_spans.insert(callee.span.start);
                            edits.push((child.span, harness.clone()));
                        }
                    }
                    Node::Identifier(name) if name == binding => reads.push(child.span),
                    _ => {}
                }
            });
            if reads.iter().any(|span| !call_spans.contains(&span.start)) {
                refused = true;
                return;
            }
        }
    });
    if refused {
        return None;
    }
    edits
        .into_iter()
        .map(|(span, harness)| add_call_argument_edit(source, &span, &harness))
        .collect()
}
