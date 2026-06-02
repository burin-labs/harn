//! The relational + composite matching algebra (#2833).
//!
//! A [`crate::model::RuleNode`] compiles to a [`CompiledNode`] tree, which
//! the evaluator walks against a parsed source tree. A node matches a
//! tree-sitter node iff its **atomic** leaf matches *and* every
//! **relational** (`inside` / `has` / `follows` / `precedes`) and
//! **composite** (`all` / `any` / `not` / `matches`) part holds — every set
//! key is ANDed.
//!
//! Candidates for the top rule are seeded cheaply (the atomic pattern query
//! in one pass, or a kind/regex/whole-tree walk); each candidate is then
//! checked in full. Relational sub-rules run their own atomic match rooted
//! at the ancestor/descendant/sibling under test.

use std::collections::BTreeMap;

use harn_hostlib::ast::{api, Language};
use regex::Regex;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor};

use crate::engine::{Binding, Span};
use crate::error::RulesError;
use crate::model::{AtomicMatcher, RuleNode, StopBy, StopKeyword};
use crate::pattern::{compile_pattern, ROOT_CAPTURE};

/// Metavar bindings accumulated while matching a node.
type Bindings = BTreeMap<String, Binding>;

/// One node that matched the top rule, with its bindings.
pub struct EvalMatch {
    /// The matched node's span.
    pub span: Span,
    /// The matched node's text.
    pub text: String,
    /// Metavar bindings (captured + threaded from relational sub-matches).
    pub bindings: Bindings,
}

/// A compiled rule tree: the top node plus the utility rules `matches`
/// references.
pub struct CompiledRuleTree {
    top: CompiledNode,
    utils: BTreeMap<String, CompiledNode>,
}

struct CompiledNode {
    atomic: Option<CompiledAtomic>,
    inside: Option<Box<CompiledRel>>,
    has: Option<Box<CompiledRel>>,
    follows: Option<Box<CompiledRel>>,
    precedes: Option<Box<CompiledRel>>,
    all: Vec<CompiledNode>,
    any: Vec<CompiledNode>,
    not: Option<Box<CompiledNode>>,
    matches: Option<String>,
}

struct CompiledRel {
    node: CompiledNode,
    stop_by: CompiledStopBy,
    field: Option<String>,
}

enum CompiledStopBy {
    Neighbor,
    End,
    Rule(Box<CompiledNode>),
}

enum CompiledAtomic {
    Query { query: Query, metavars: Vec<String> },
    Kind(String),
    Regex(Regex),
}

impl CompiledRuleTree {
    /// Compile a rule's `[rule]` node and its `[utils]` into a runnable
    /// tree.
    pub fn compile(
        rule_id: &str,
        language: Language,
        top: &RuleNode,
        utils: &BTreeMap<String, RuleNode>,
    ) -> Result<Self, RulesError> {
        if top.is_empty() {
            return Err(RulesError::PatternCompile {
                rule: rule_id.to_string(),
                message: "rule node is empty (no atomic / relational / composite key)".into(),
            });
        }
        let compiled_utils = utils
            .iter()
            .map(|(id, node)| Ok((id.clone(), compile_node(rule_id, language, node)?)))
            .collect::<Result<BTreeMap<_, _>, RulesError>>()?;
        Ok(CompiledRuleTree {
            top: compile_node(rule_id, language, top)?,
            utils: compiled_utils,
        })
    }

    /// Find every node matching the top rule, in document order.
    pub fn find(
        &self,
        rule_id: &str,
        language: Language,
        source: &str,
    ) -> Result<Vec<EvalMatch>, RulesError> {
        let tree = api::parse_tree(source, language).map_err(|err| RulesError::SourceParse {
            rule: rule_id.to_string(),
            message: err.to_string(),
        })?;
        let ctx = Ctx {
            source,
            utils: &self.utils,
        };
        let root = tree.root_node();

        let mut seen: BTreeMap<(usize, usize), EvalMatch> = BTreeMap::new();
        for node in seed_candidates(&self.top, &ctx, root) {
            if let Some(bindings) = node_satisfies(&self.top, node, &ctx) {
                let key = (node.start_byte(), node.end_byte());
                seen.entry(key).or_insert_with(|| EvalMatch {
                    span: Span::of(node),
                    text: ctx.text(node),
                    bindings,
                });
            }
        }
        Ok(seen.into_values().collect())
    }
}

/// Per-run evaluation context.
struct Ctx<'a> {
    source: &'a str,
    utils: &'a BTreeMap<String, CompiledNode>,
}

impl Ctx<'_> {
    fn text(&self, node: Node<'_>) -> String {
        self.source[node.start_byte()..node.end_byte()].to_string()
    }
}

// ---------------------------------------------------------------------------
// Compilation
// ---------------------------------------------------------------------------

fn compile_node(
    rule_id: &str,
    language: Language,
    node: &RuleNode,
) -> Result<CompiledNode, RulesError> {
    let mkerr = |message: String| RulesError::PatternCompile {
        rule: rule_id.to_string(),
        message,
    };

    let atomic = match node.atomic().map_err(mkerr)? {
        None => None,
        Some(AtomicMatcher::Pattern(snippet)) => {
            let ts_language = language
                .ts_language()
                .ok_or_else(|| mkerr(format!("grammar for `{}` unavailable", language.name())))?;
            let compiled =
                compile_pattern(&snippet, language).map_err(|m| mkerr(format!("pattern: {m}")))?;
            let query = Query::new(&ts_language, &compiled.query).map_err(|e| {
                RulesError::QueryRejected {
                    rule: rule_id.to_string(),
                    message: e.to_string(),
                    query: compiled.query.clone(),
                }
            })?;
            Some(CompiledAtomic::Query {
                query,
                metavars: compiled.metavars,
            })
        }
        Some(AtomicMatcher::Kind(kind)) => Some(CompiledAtomic::Kind(kind)),
        Some(AtomicMatcher::Regex(re)) => Some(CompiledAtomic::Regex(
            Regex::new(&re).map_err(|e| mkerr(format!("regex `{re}` invalid: {e}")))?,
        )),
    };

    let rel = |sub: &Option<Box<RuleNode>>| -> Result<Option<Box<CompiledRel>>, RulesError> {
        match sub {
            None => Ok(None),
            Some(n) => Ok(Some(Box::new(compile_rel(rule_id, language, n)?))),
        }
    };

    let compile_list = |list: &Option<Vec<RuleNode>>| -> Result<Vec<CompiledNode>, RulesError> {
        list.iter()
            .flatten()
            .map(|n| compile_node(rule_id, language, n))
            .collect()
    };

    Ok(CompiledNode {
        atomic,
        inside: rel(&node.inside)?,
        has: rel(&node.has)?,
        follows: rel(&node.follows)?,
        precedes: rel(&node.precedes)?,
        all: compile_list(&node.all)?,
        any: compile_list(&node.any)?,
        not: match &node.not {
            None => None,
            Some(n) => Some(Box::new(compile_node(rule_id, language, n)?)),
        },
        matches: node.matches.clone(),
    })
}

fn compile_rel(
    rule_id: &str,
    language: Language,
    node: &RuleNode,
) -> Result<CompiledRel, RulesError> {
    let stop_by = match &node.stop_by {
        None | Some(StopBy::Keyword(StopKeyword::Neighbor)) => CompiledStopBy::Neighbor,
        Some(StopBy::Keyword(StopKeyword::End)) => CompiledStopBy::End,
        Some(StopBy::Rule(r)) => {
            CompiledStopBy::Rule(Box::new(compile_node(rule_id, language, r)?))
        }
    };
    Ok(CompiledRel {
        node: compile_node(rule_id, language, node)?,
        stop_by,
        field: node.field.clone(),
    })
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

/// Seed candidate nodes for the top rule. The atomic pattern query runs in
/// one pass; kind/regex/composite-only rules fall back to a tree walk.
fn seed_candidates<'t>(top: &CompiledNode, ctx: &Ctx<'_>, root: Node<'t>) -> Vec<Node<'t>> {
    match &top.atomic {
        Some(CompiledAtomic::Query { query, .. }) => {
            let mut out = Vec::new();
            let mut seen = std::collections::HashSet::new();
            let root_index = root_capture_index(query);
            let mut cursor = QueryCursor::new();
            let mut it = cursor.matches(query, root, ctx.source.as_bytes());
            while let Some(m) = it.next() {
                for cap in m.captures {
                    if Some(cap.index) == root_index && seen.insert(cap.node.id()) {
                        out.push(cap.node);
                    }
                }
            }
            out
        }
        Some(CompiledAtomic::Kind(kind)) => {
            let mut out = Vec::new();
            for_each_named_descendant(root, &mut |n| {
                if n.kind() == kind {
                    out.push(n);
                }
            });
            out
        }
        Some(CompiledAtomic::Regex(_)) | None => {
            let mut out = Vec::new();
            for_each_named_descendant(root, &mut |n| out.push(n));
            out
        }
    }
}

/// Full match check for a specific `node`: atomic + relational + composite.
fn node_satisfies(cnode: &CompiledNode, node: Node<'_>, ctx: &Ctx<'_>) -> Option<Bindings> {
    let mut bindings = Bindings::new();

    if let Some(atomic) = &cnode.atomic {
        merge(&mut bindings, atomic_match(atomic, node, ctx)?);
    }
    if let Some(rel) = &cnode.inside {
        merge(&mut bindings, eval_inside(rel, node, ctx)?);
    }
    if let Some(rel) = &cnode.has {
        merge(&mut bindings, eval_has(rel, node, ctx)?);
    }
    if let Some(rel) = &cnode.follows {
        merge(&mut bindings, eval_sibling(rel, node, ctx, Dir::Before)?);
    }
    if let Some(rel) = &cnode.precedes {
        merge(&mut bindings, eval_sibling(rel, node, ctx, Dir::After)?);
    }
    for sub in &cnode.all {
        merge(&mut bindings, node_satisfies(sub, node, ctx)?);
    }
    if !cnode.any.is_empty() {
        let matched = cnode
            .any
            .iter()
            .find_map(|sub| node_satisfies(sub, node, ctx));
        merge(&mut bindings, matched?);
    }
    if let Some(not) = &cnode.not {
        if node_satisfies(not, node, ctx).is_some() {
            return None;
        }
    }
    if let Some(id) = &cnode.matches {
        let util = ctx.utils.get(id)?;
        merge(&mut bindings, node_satisfies(util, node, ctx)?);
    }

    Some(bindings)
}

fn atomic_match(atomic: &CompiledAtomic, node: Node<'_>, ctx: &Ctx<'_>) -> Option<Bindings> {
    match atomic {
        CompiledAtomic::Kind(kind) => (node.kind() == kind).then(Bindings::new),
        CompiledAtomic::Regex(re) => re.is_match(&ctx.text(node)).then(Bindings::new),
        CompiledAtomic::Query { query, metavars } => {
            let root_index = root_capture_index(query);
            let names: Vec<&str> = query.capture_names().to_vec();
            let mut cursor = QueryCursor::new();
            let mut it = cursor.matches(query, node, ctx.source.as_bytes());
            while let Some(m) = it.next() {
                // The pattern must match `node` itself, not a descendant.
                let roots_here = m
                    .captures
                    .iter()
                    .any(|c| Some(c.index) == root_index && c.node.id() == node.id());
                if !roots_here {
                    continue;
                }
                let mut bindings = Bindings::new();
                for cap in m.captures {
                    let name = names[cap.index as usize];
                    if metavars.iter().any(|mv| mv == name) {
                        bindings.entry(name.to_string()).or_insert_with(|| {
                            Binding::new(ctx.text(cap.node), Span::of(cap.node))
                        });
                    }
                }
                return Some(bindings);
            }
            None
        }
    }
}

fn eval_inside(rel: &CompiledRel, node: Node<'_>, ctx: &Ctx<'_>) -> Option<Bindings> {
    let mut current = node.parent();
    let mut child = node;
    while let Some(ancestor) = current {
        if let CompiledStopBy::Rule(stop) = &rel.stop_by {
            if node_satisfies(stop, ancestor, ctx).is_some()
                && node_satisfies(&rel.node, ancestor, ctx).is_none()
            {
                // Hit the stop boundary without matching.
                return None;
            }
        }
        if let Some(b) = node_satisfies(&rel.node, ancestor, ctx) {
            if field_ok(rel.field.as_deref(), ancestor, child) {
                return Some(b);
            }
        }
        if matches!(rel.stop_by, CompiledStopBy::Neighbor) {
            return None;
        }
        child = ancestor;
        current = ancestor.parent();
    }
    None
}

fn eval_has(rel: &CompiledRel, node: Node<'_>, ctx: &Ctx<'_>) -> Option<Bindings> {
    let neighbor = matches!(rel.stop_by, CompiledStopBy::Neighbor);
    let mut found: Option<Bindings> = None;
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    for child in children {
        if let Some(b) = node_satisfies(&rel.node, child, ctx) {
            if field_ok(rel.field.as_deref(), node, child) {
                found = Some(b);
                break;
            }
        }
        if !neighbor {
            if let Some(b) = eval_has(rel, child, ctx) {
                found = Some(b);
                break;
            }
        }
    }
    found
}

enum Dir {
    Before,
    After,
}

fn eval_sibling(rel: &CompiledRel, node: Node<'_>, ctx: &Ctx<'_>, dir: Dir) -> Option<Bindings> {
    let neighbor = matches!(rel.stop_by, CompiledStopBy::Neighbor);
    let mut sib = match dir {
        Dir::Before => node.prev_named_sibling(),
        Dir::After => node.next_named_sibling(),
    };
    while let Some(s) = sib {
        if let Some(b) = node_satisfies(&rel.node, s, ctx) {
            return Some(b);
        }
        if neighbor {
            return None;
        }
        sib = match dir {
            Dir::Before => s.prev_named_sibling(),
            Dir::After => s.next_named_sibling(),
        };
    }
    None
}

/// When a relation names a `field`, the related child must sit in that
/// field of the parent. `parent` is the ancestor; `child` is the node on
/// the path toward the matched node.
fn field_ok(field: Option<&str>, parent: Node<'_>, child: Node<'_>) -> bool {
    match field {
        None => true,
        Some(name) => parent
            .child_by_field_name(name)
            .is_some_and(|f| f.id() == child.id()),
    }
}

fn merge(into: &mut Bindings, from: Bindings) {
    for (k, v) in from {
        into.entry(k).or_insert(v);
    }
}

fn root_capture_index(query: &Query) -> Option<u32> {
    query
        .capture_names()
        .iter()
        .position(|n| *n == ROOT_CAPTURE)
        .map(|i| i as u32)
}

fn for_each_named_descendant<'t>(node: Node<'t>, f: &mut impl FnMut(Node<'t>)) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        f(child);
        for_each_named_descendant(child, f);
    }
}
