//! Pure planning for `code_index.extract_function`: source text in, either a
//! [`Plan`] (edits plus the helper and call text) or a [`Refusal`] out. No
//! I/O, so every refusal path is testable against an in-memory string.
//!
//! The analysis is syntactic and single-file:
//!
//! 1. **Align** the selection to one expression, a run of sibling
//!    statements, or a closure body ([`align`]).
//! 2. **Escapes**: `return`, `break`/`continue` that leave the region, `?`,
//!    `await`, and `yield` refuse, except that `return` and `?` inside an
//!    extracted closure body already belong to the new function.
//! 3. **Flow** ([`flow`]): the language's `ast.undefined_names` profile
//!    supplies every binding and reference in the enclosing function. Inputs
//!    are names the region reads whose value can come from a binding outside
//!    it; outputs are names the region binds or assigns that are read after it.
//! 4. **Copies**: with `all_occurrences`, every other same-file span with the
//!    same token sequence, shape, and flow is replaced by the same call.

use std::collections::HashSet;

use tree_sitter::{Node, Tree};

use crate::ast::{api as ast_api, scan_names, Language};

use super::refactor_core::{first_syntax_error, EditSpan, IdentifierSpan};

mod flow;
mod synth;

use flow::{flow, returns_value, same_flow, Flow};
use synth::{call_for, check_header, parse_function, synthesize, Param};

/// The languages extraction supports, grouped by the syntax they share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Dialect {
    Rust,
    TypeScript,
    JavaScript,
    Python,
}

impl Dialect {
    pub(super) fn of(language: Language) -> Option<Self> {
        match language {
            Language::Rust => Some(Self::Rust),
            Language::TypeScript | Language::Tsx => Some(Self::TypeScript),
            Language::JavaScript | Language::Jsx => Some(Self::JavaScript),
            Language::Python => Some(Self::Python),
            _ => None,
        }
    }

    fn syntax(self) -> &'static Syntax {
        match self {
            Self::Rust => &RUST,
            Self::TypeScript | Self::JavaScript => &ECMASCRIPT,
            Self::Python => &PYTHON,
        }
    }

    fn is_brace(self) -> bool {
        self != Self::Python
    }
}

/// Node-kind tables for one dialect. Data, not branches at call sites.
struct Syntax {
    /// Containers whose named children are statements.
    blocks: &'static [&'static str],
    /// Named functions: the scope whose locals become inputs.
    functions: &'static [&'static str],
    /// Closures and lambdas: a region may be one's body.
    closures: &'static [&'static str],
    /// Nested definitions that `return`, `?`, and `await` cannot cross.
    barriers: &'static [&'static str],
    loops: &'static [&'static str],
    /// Constructs an unlabeled `break` can target besides loops.
    switches: &'static [&'static str],
    returns: &'static [&'static str],
    breaks: &'static [&'static str],
    continues: &'static [&'static str],
    tries: &'static [&'static str],
    suspends: &'static [&'static str],
    /// The receiver keyword a free function cannot see.
    receivers: &'static [&'static str],
    /// Identifier leaves that can bind or read a local.
    locals: &'static [&'static str],
    /// `(parent kind, field)` pairs whose identifier child is assigned.
    assignments: &'static [(&'static str, &'static str)],
    /// Containers of module-level items.
    modules: &'static [&'static str],
    comments: &'static [&'static str],
}

static RUST: Syntax = Syntax {
    blocks: &["block"],
    functions: &["function_item"],
    closures: &["closure_expression"],
    barriers: &[
        "function_item",
        "closure_expression",
        "async_block",
        "impl_item",
    ],
    loops: &["for_expression", "while_expression", "loop_expression"],
    switches: &[],
    returns: &["return_expression"],
    breaks: &["break_expression"],
    continues: &["continue_expression"],
    tries: &["try_expression"],
    suspends: &["await_expression", "yield_expression"],
    receivers: &["self"],
    locals: &["identifier", "shorthand_field_identifier"],
    assignments: &[
        ("assignment_expression", "left"),
        ("compound_assignment_expr", "left"),
    ],
    modules: &["source_file"],
    comments: &["line_comment", "block_comment"],
};

static ECMASCRIPT: Syntax = Syntax {
    blocks: &["statement_block"],
    functions: &[
        "function_declaration",
        "generator_function_declaration",
        "method_definition",
    ],
    closures: &[
        "arrow_function",
        "function_expression",
        "function",
        "generator_function",
    ],
    barriers: &[
        "function_declaration",
        "generator_function_declaration",
        "method_definition",
        "arrow_function",
        "function_expression",
        "function",
        "generator_function",
        "class_declaration",
        "class",
    ],
    loops: &[
        "for_statement",
        "for_in_statement",
        "while_statement",
        "do_statement",
    ],
    switches: &["switch_statement"],
    returns: &["return_statement"],
    breaks: &["break_statement"],
    continues: &["continue_statement"],
    tries: &[],
    suspends: &["await_expression", "yield_expression"],
    receivers: &["this", "super"],
    locals: &["identifier", "shorthand_property_identifier_pattern"],
    assignments: &[
        ("assignment_expression", "left"),
        ("augmented_assignment_expression", "left"),
        ("update_expression", "argument"),
    ],
    modules: &["program"],
    comments: &["comment"],
};

static PYTHON: Syntax = Syntax {
    blocks: &["block"],
    functions: &["function_definition"],
    closures: &["lambda"],
    barriers: &["function_definition", "lambda", "class_definition"],
    loops: &["for_statement", "while_statement"],
    switches: &[],
    returns: &["return_statement"],
    breaks: &["break_statement"],
    continues: &["continue_statement"],
    tries: &[],
    suspends: &["await", "yield"],
    receivers: &[],
    locals: &["identifier"],
    assignments: &[("augmented_assignment", "left")],
    modules: &["module"],
    comments: &["comment"],
};

/// How the caller pointed at the region.
pub(super) enum Selection<'a> {
    /// 1-based, inclusive.
    Lines(usize, usize),
    /// Exact source text.
    Text(&'a str),
}

pub(super) struct PlanInput<'a> {
    pub source: &'a str,
    pub language: Language,
    pub selection: Selection<'a>,
    pub new_name: &'a str,
    pub signature: Option<&'a str>,
    pub helper: Option<&'a str>,
    pub all_occurrences: bool,
    /// The dialect needs parameter types (strict TypeScript).
    pub strict_types: bool,
}

/// A source location reported back to the caller. `line` is 1-based.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Site {
    pub line: usize,
    pub kind: &'static str,
    pub reason: String,
    pub text: String,
}

/// What the analysis learned about the region, reported on success and on
/// any refusal that got far enough to compute it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct FlowSummary {
    pub region_kind: &'static str,
    pub start_line: usize,
    pub end_line: usize,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub returns_value: bool,
}

#[derive(Debug)]
pub(super) struct Refusal {
    pub tag: &'static str,
    pub details: String,
    pub sites: Vec<Site>,
    pub candidates: Vec<Site>,
    pub flow: Option<FlowSummary>,
}

impl Refusal {
    pub(super) fn new(tag: &'static str, details: impl Into<String>) -> Box<Self> {
        Box::new(Self {
            tag,
            details: details.into(),
            sites: Vec::new(),
            candidates: Vec::new(),
            flow: None,
        })
    }

    fn with_flow(mut self: Box<Self>, flow: &FlowSummary) -> Box<Self> {
        self.flow = Some(flow.clone());
        self
    }
}

#[derive(Debug)]
pub(super) struct Plan {
    pub edits: Vec<EditSpan>,
    pub helper_text: String,
    pub call_text: String,
    pub flow: FlowSummary,
    /// 1-based start line of every replaced span, primary first.
    pub replaced_lines: Vec<usize>,
    /// Token-equal copies left alone because their flow differs.
    pub skipped_copies: Vec<Site>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Expression,
    Statements { tail: bool },
    ClosureBody,
}

impl Shape {
    fn as_str(self) -> &'static str {
        match self {
            Self::Expression => "expression",
            Self::Statements { .. } => "statements",
            Self::ClosureBody => "closure_body",
        }
    }
}

/// A region lined up with syntax. `nodes` are the statements of a run, the
/// expression, or the closure; `start..end` is the span the call replaces.
#[derive(Clone)]
struct Target<'t> {
    shape: Shape,
    nodes: Vec<Node<'t>>,
    start: usize,
    end: usize,
}

impl<'t> Target<'t> {
    fn closure_body(&self) -> Option<Node<'t>> {
        (self.shape == Shape::ClosureBody)
            .then(|| self.nodes[0].child_by_field_name("body"))
            .flatten()
    }

    /// The units statement ordering is measured over.
    fn units(&self, syn: &Syntax) -> Vec<Node<'t>> {
        match self.closure_body() {
            Some(body) if syn.blocks.contains(&body.kind()) => named_children(body)
                .into_iter()
                .filter(|n| !syn.comments.contains(&n.kind()))
                .collect(),
            Some(body) => vec![body],
            None => self.nodes.clone(),
        }
    }

    fn overlaps(&self, other: &Target<'_>) -> bool {
        self.outer_start() < other.outer_end() && other.outer_start() < self.outer_end()
    }

    fn outer_start(&self) -> usize {
        self.nodes.first().map_or(self.start, |n| n.start_byte())
    }

    fn outer_end(&self) -> usize {
        self.nodes.last().map_or(self.end, |n| n.end_byte())
    }
}

pub(super) fn plan(input: &PlanInput<'_>) -> Result<Plan, Box<Refusal>> {
    let Some(dialect) = Dialect::of(input.language) else {
        return Err(Refusal::new(
            "unsupported_language",
            format!(
                "extract_function supports rust, typescript, tsx, javascript, jsx, and python; got `{}`",
                input.language.name()
            ),
        ));
    };
    let syn = dialect.syntax();
    let source = input.source;
    let tree = parse(source, input.language)?;
    let root = tree.root_node();

    let (start, end) = select(source, &input.selection, input.all_occurrences)?;
    let Some(primary) = align(root, start, end, syn) else {
        return Err(Refusal::new(
            "no_match",
            format!(
                "lines {}-{} do not line up with one expression, a run of whole statements, or a closure body",
                line_of(source, start),
                line_of(source, end)
            ),
        ));
    };
    let Some(scope) = enclosing_scope(&primary, syn) else {
        return Err(Refusal::new(
            "no_match",
            "the region is not inside a function body",
        ));
    };
    let flow_primary = flow(dialect, input.language, source, scope, &primary);
    let summary = FlowSummary {
        region_kind: primary.shape.as_str(),
        start_line: line_of(source, primary.outer_start()),
        end_line: line_of(
            source,
            primary
                .outer_end()
                .saturating_sub(1)
                .max(primary.outer_start()),
        ),
        inputs: flow_primary.inputs.clone(),
        outputs: flow_primary.outputs.clone(),
        returns_value: returns_value(&primary, &flow_primary, syn),
    };

    if !flow_primary.escapes.is_empty() {
        let mut refusal = Refusal::new(
            "control_flow_escapes",
            "control flow leaves the region (see `sites`); a call cannot reproduce it. \
             Narrow the region so it excludes the jump, or extract a closure body or expression instead",
        )
        .with_flow(&summary);
        refusal.sites = flow_primary.escapes;
        return Err(refusal);
    }
    if let Some(reason) = flow_primary.class_bound.clone() {
        return Err(Refusal::new("unsupported_region", reason).with_flow(&summary));
    }
    if primary.shape == Shape::Expression && !flow_primary.outputs.is_empty() {
        return Err(Refusal::new(
            "unsupported_region",
            format!(
                "the expression assigns {} read after it; extract the enclosing statements instead",
                flow_primary.outputs.join(", ")
            ),
        )
        .with_flow(&summary));
    }
    if dialect.is_brace()
        && !flow_primary.declared.is_empty()
        && flow_primary.declared.len() != flow_primary.outputs.len()
    {
        return Err(Refusal::new(
            "unsupported_region",
            "some outputs are declared inside the region and some are assigned to outer variables; \
             extract a region where they agree",
        )
        .with_flow(&summary));
    }

    let conflicts = name_sites(root, source, input.language, input.new_name);
    if !conflicts.is_empty() {
        let mut refusal = Refusal::new(
            "name_conflict",
            format!("`{}` already names something in this file", input.new_name),
        )
        .with_flow(&summary);
        refusal.sites = conflicts;
        return Err(refusal);
    }

    // The header comes from the helper, the signature, or is synthesized.
    let header = match (input.helper, input.signature) {
        (Some(helper), _) => Some(parse_function(dialect, input.language, helper, false)?),
        (None, Some(signature)) => Some(parse_function(dialect, input.language, signature, true)?),
        (None, None) => None,
    };
    if let Some(header) = &header {
        check_header(dialect, header, input.new_name, &summary)?;
    } else if needs_types(dialect, input.strict_types, &summary) {
        return Err(Refusal::new(
            "types_required",
            format!(
                "{} needs parameter and return types; pass `signature` with parameters named {}",
                if dialect == Dialect::Rust {
                    "Rust"
                } else {
                    "strict TypeScript"
                },
                display_list(&summary.inputs)
            ),
        )
        .with_flow(&summary));
    }

    let params: Vec<Param> = match &header {
        Some(header) => header.params.clone(),
        None => summary
            .inputs
            .iter()
            .map(|name| Param {
                name: name.clone(),
                ty: String::new(),
            })
            .collect(),
    };

    // Copies first, so a refusal never depends on which copy was primary.
    let mut targets = vec![(primary.clone(), flow_primary.clone())];
    let mut skipped = Vec::new();
    if input.all_occurrences {
        for candidate in copies(root, source, &primary, syn) {
            if targets.iter().any(|(t, _)| t.overlaps(&candidate)) {
                continue;
            }
            let Some(copy_scope) = enclosing_scope(&candidate, syn) else {
                continue;
            };
            let copy_flow = flow(dialect, input.language, source, copy_scope, &candidate);
            if same_flow(&copy_flow, &flow_primary) {
                targets.push((candidate, copy_flow));
            } else {
                skipped.push(Site {
                    line: line_of(source, candidate.outer_start()),
                    kind: "copy",
                    reason:
                        "token-equal copy with different bindings or control flow; left unchanged"
                            .into(),
                    text: first_line(slice(
                        source,
                        candidate.outer_start(),
                        candidate.outer_end(),
                    )),
                });
            }
        }
    }
    targets.sort_by_key(|(t, _)| t.outer_start());

    let semicolons = dialect == Dialect::Rust || uses_semicolons(&primary, source, syn);
    let insert_item = module_item(scope, syn);
    let base_indent = line_indent(source, insert_item.start_byte());
    let unit = indent_unit(source, dialect);
    let helper_text = match input.helper {
        Some(helper) => reindent_block(helper, &base_indent),
        None => synthesize(
            dialect,
            source,
            &primary,
            &flow_primary,
            header.as_ref(),
            input.new_name,
            &base_indent,
            &format!("{base_indent}{unit}"),
            semicolons,
            syn,
        ),
    };

    let mut edits = Vec::new();
    let mut call_text = String::new();
    let mut replaced_lines = Vec::new();
    for (target, target_flow) in &targets {
        let call = call_for(
            dialect,
            target,
            target_flow,
            input.new_name,
            &params,
            semicolons,
        );
        if target.start == primary.start {
            call_text = call.clone();
        }
        replaced_lines.push(line_of(source, target.outer_start()));
        edits.push(edit(source, target.start, target.end, call));
    }
    let separator = if dialect == Dialect::Python {
        "\n\n\n"
    } else {
        "\n\n"
    };
    let at = insert_item.end_byte();
    edits.push(edit(source, at, at, format!("{separator}{helper_text}")));

    Ok(Plan {
        edits,
        helper_text,
        call_text,
        flow: summary,
        replaced_lines,
        skipped_copies: skipped,
    })
}

fn parse(source: &str, language: Language) -> Result<Tree, Box<Refusal>> {
    let tree = ast_api::parse_tree(source, language)
        .map_err(|err| Refusal::new("syntax_error", format!("parse failed: {err}")))?;
    if let Some(detail) = first_syntax_error(source, language) {
        return Err(Refusal::new(
            "syntax_error",
            format!("the file does not parse before the edit ({detail}); fix it first"),
        ));
    }
    Ok(tree)
}

/// Byte span of the selection, trimmed of surrounding whitespace.
fn select(
    source: &str,
    selection: &Selection<'_>,
    all_occurrences: bool,
) -> Result<(usize, usize), Box<Refusal>> {
    match selection {
        Selection::Lines(start_line, end_line) => {
            let starts = line_starts(source);
            if *start_line == 0 || end_line < start_line || *end_line > starts.len() {
                return Err(Refusal::new(
                    "no_match",
                    format!(
                        "lines {start_line}-{end_line} are outside the file ({} lines)",
                        starts.len()
                    ),
                ));
            }
            let from = starts[start_line - 1];
            let to = starts
                .get(*end_line)
                .map_or(source.len(), |next| next.saturating_sub(1));
            let text = slice(source, from, to);
            let lead = text.len() - text.trim_start().len();
            let trail = text.len() - text.trim_end().len();
            if lead + trail >= text.len() {
                return Err(Refusal::new("no_match", "the selected lines are blank"));
            }
            Ok((from + lead, to - trail))
        }
        Selection::Text(region) => {
            let needle = region.trim();
            if needle.is_empty() {
                return Err(Refusal::new("no_match", "`region` is empty"));
            }
            let matches: Vec<usize> = source.match_indices(needle).map(|(i, _)| i).collect();
            match matches.as_slice() {
                [] => Err(Refusal::new(
                    "no_match",
                    "`region` does not occur in the file; pass the exact text or use start_line/end_line",
                )),
                [only] => Ok((*only, only + needle.len())),
                // Every match is a copy; the first is primary and the copy
                // search below finds the rest.
                [first, ..] if all_occurrences => Ok((*first, first + needle.len())),
                _ => {
                    let mut refusal = Refusal::new(
                        "ambiguous_symbol",
                        "`region` occurs more than once; pass start_line/end_line or set all_occurrences",
                    );
                    refusal.candidates = matches
                        .iter()
                        .map(|at| Site {
                            line: line_of(source, *at),
                            kind: "region",
                            reason: "matching text".into(),
                            text: first_line(needle),
                        })
                        .collect();
                    Err(refusal)
                }
            }
        }
    }
}

fn align<'t>(root: Node<'t>, start: usize, end: usize, syn: &Syntax) -> Option<Target<'t>> {
    let deepest = root.descendant_for_byte_range(start, end)?;
    if deepest.start_byte() == start && deepest.end_byte() == end {
        let mut chain = vec![deepest];
        while let Some(parent) = chain.last().and_then(|n| n.parent()) {
            if parent.start_byte() != start || parent.end_byte() != end {
                break;
            }
            chain.push(parent);
        }
        for node in chain.iter().rev() {
            if syn.closures.contains(&node.kind()) {
                return closure_target(*node);
            }
            if let Some(parent) = node.parent() {
                if syn.closures.contains(&parent.kind())
                    && parent.child_by_field_name("body").map(|b| b.id()) == Some(node.id())
                {
                    return closure_target(parent);
                }
                if syn.blocks.contains(&parent.kind()) && node.is_named() {
                    return statements_target(parent, vec![*node], syn);
                }
            }
            // A brace-less (Python) block spans exactly its statements.
            if syn.blocks.contains(&node.kind()) {
                let statements = named_children(*node);
                if statements.first().map(|n| n.start_byte()) == Some(node.start_byte()) {
                    return statements_target(*node, statements, syn);
                }
            }
        }
        let expression = chain
            .iter()
            .rev()
            .find(|n| n.is_named() && is_expression_like(**n))?;
        return Some(Target {
            shape: Shape::Expression,
            nodes: vec![*expression],
            start,
            end,
        });
    }
    // A run of statements inside one block.
    let mut container = Some(deepest);
    while let Some(node) = container {
        if syn.blocks.contains(&node.kind()) {
            break;
        }
        container = node.parent();
    }
    let container = container?;
    let children = named_children(container);
    let first = children.iter().position(|c| c.start_byte() == start)?;
    let last = children.iter().position(|c| c.end_byte() == end)?;
    if last < first {
        return None;
    }
    statements_target(container, children[first..=last].to_vec(), syn)
}

fn closure_target(closure: Node<'_>) -> Option<Target<'_>> {
    let body = closure.child_by_field_name("body")?;
    Some(Target {
        shape: Shape::ClosureBody,
        nodes: vec![closure],
        start: body.start_byte(),
        end: body.end_byte(),
    })
}

/// A statement run, or the closure body it spans entirely.
fn statements_target<'t>(
    block: Node<'t>,
    nodes: Vec<Node<'t>>,
    syn: &Syntax,
) -> Option<Target<'t>> {
    let statements: Vec<Node<'t>> = named_children(block)
        .into_iter()
        .filter(|n| !syn.comments.contains(&n.kind()))
        .collect();
    let covered: Vec<Node<'t>> = nodes
        .iter()
        .copied()
        .filter(|n| !syn.comments.contains(&n.kind()))
        .collect();
    if covered.is_empty() {
        return None;
    }
    if let Some(parent) = block.parent() {
        if syn.closures.contains(&parent.kind()) && covered.len() == statements.len() {
            return closure_target(parent);
        }
    }
    let last = *covered.last()?;
    let tail = syn.blocks == RUST.blocks
        && statements.last().map(|n| n.id()) == Some(last.id())
        && !is_rust_statement(last.kind());
    Some(Target {
        shape: Shape::Statements { tail },
        start: nodes[0].start_byte(),
        end: nodes[nodes.len() - 1].end_byte(),
        nodes,
    })
}

fn is_rust_statement(kind: &str) -> bool {
    kind.ends_with("_item")
        || matches!(
            kind,
            "expression_statement" | "let_declaration" | "empty_statement" | "macro_invocation"
        )
}

fn is_expression_like(node: Node<'_>) -> bool {
    let kind = node.kind();
    let structural = [
        "_statement",
        "_declaration",
        "_item",
        "_pattern",
        "_type",
        "_clause",
        "_definition",
        "parameters",
        "block",
        "arguments",
        "argument_list",
    ];
    if structural.iter().any(|s| kind.ends_with(s)) {
        return false;
    }
    // The left side of a binding or assignment is not a value.
    let Some(parent) = node.parent() else {
        return false;
    };
    !["pattern", "left", "name", "type", "parameters"]
        .iter()
        .any(|field| parent.child_by_field_name(field).map(|c| c.id()) == Some(node.id()))
}

/// The function whose locals the region can read: the nearest named
/// function, else the outermost closure.
fn enclosing_scope<'t>(target: &Target<'t>, syn: &Syntax) -> Option<Node<'t>> {
    let mut outermost_closure = None;
    let mut current = target.nodes[0].parent();
    while let Some(node) = current {
        if syn.functions.contains(&node.kind()) {
            return Some(node);
        }
        if syn.closures.contains(&node.kind()) {
            outermost_closure = Some(node);
        }
        current = node.parent();
    }
    outermost_closure
}

/// The module-level item that holds `scope`; the helper goes after it.
fn module_item<'t>(scope: Node<'t>, syn: &Syntax) -> Node<'t> {
    let mut node = scope;
    while let Some(parent) = node.parent() {
        if syn.modules.contains(&parent.kind()) {
            return node;
        }
        // An inline Rust module keeps its helper inside it.
        if parent.kind() == "declaration_list"
            && parent.parent().is_some_and(|p| p.kind() == "mod_item")
        {
            return node;
        }
        node = parent;
    }
    node
}

fn needs_types(dialect: Dialect, strict: bool, summary: &FlowSummary) -> bool {
    match dialect {
        Dialect::Rust => !summary.inputs.is_empty() || summary.returns_value,
        Dialect::TypeScript => strict && !summary.inputs.is_empty(),
        Dialect::JavaScript | Dialect::Python => false,
    }
}

/// Every other span with the primary's shape and token sequence.
fn copies<'t>(root: Node<'t>, source: &str, primary: &Target<'t>, syn: &Syntax) -> Vec<Target<'t>> {
    let wanted = target_tokens(primary, source, syn);
    let mut out = Vec::new();
    match primary.shape {
        Shape::Statements { tail } => {
            let width = primary
                .nodes
                .iter()
                .filter(|n| !syn.comments.contains(&n.kind()))
                .count();
            walk(root, &mut |node| {
                if !syn.blocks.contains(&node.kind()) {
                    return;
                }
                let statements: Vec<Node<'t>> = named_children(node)
                    .into_iter()
                    .filter(|n| !syn.comments.contains(&n.kind()))
                    .collect();
                if statements.len() < width {
                    return;
                }
                for window in statements.windows(width) {
                    let mut tokens = Vec::new();
                    for n in window {
                        collect_tokens(*n, source, syn, &mut tokens);
                    }
                    let is_primary = window[0].start_byte() == primary.outer_start();
                    if is_primary || tokens != wanted {
                        continue;
                    }
                    if let Some(candidate) = statements_target(node, window.to_vec(), syn) {
                        if candidate.shape == (Shape::Statements { tail }) {
                            out.push(candidate);
                        }
                    }
                }
            });
        }
        Shape::Expression => {
            let kind = primary.nodes[0].kind();
            walk(root, &mut |node| {
                if node.kind() == kind
                    && node.start_byte() != primary.start
                    && is_expression_like(node)
                {
                    let mut tokens = Vec::new();
                    collect_tokens(node, source, syn, &mut tokens);
                    if tokens == wanted {
                        out.push(Target {
                            shape: Shape::Expression,
                            nodes: vec![node],
                            start: node.start_byte(),
                            end: node.end_byte(),
                        });
                    }
                }
            });
        }
        Shape::ClosureBody => {
            let kind = primary.nodes[0].kind();
            walk(root, &mut |node| {
                if node.kind() == kind && node.start_byte() != primary.nodes[0].start_byte() {
                    let mut tokens = Vec::new();
                    collect_tokens(node, source, syn, &mut tokens);
                    if tokens == wanted {
                        if let Some(candidate) = closure_target(node) {
                            out.push(candidate);
                        }
                    }
                }
            });
        }
    }
    out.sort_by_key(|t| t.outer_start());
    out
}

fn target_tokens<'s>(target: &Target<'_>, source: &'s str, syn: &Syntax) -> Vec<(u16, &'s str)> {
    let mut tokens = Vec::new();
    for node in &target.nodes {
        if !syn.comments.contains(&node.kind()) {
            collect_tokens(*node, source, syn, &mut tokens);
        }
    }
    tokens
}

fn collect_tokens<'s>(
    node: Node<'_>,
    source: &'s str,
    syn: &Syntax,
    out: &mut Vec<(u16, &'s str)>,
) {
    if syn.comments.contains(&node.kind()) {
        return;
    }
    if node.child_count() == 0 {
        out.push((node.kind_id(), text(node, source)));
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_tokens(child, source, syn, out);
    }
}

/// Every identifier in the file already spelled `name`.
fn name_sites(root: Node<'_>, source: &str, language: Language, name: &str) -> Vec<Site> {
    let kinds = language
        .rename_identifier_kinds()
        .unwrap_or(&["identifier"]);
    let mut sites = Vec::new();
    walk(root, &mut |node| {
        if kinds.contains(&node.kind()) && text(node, source) == name {
            sites.push(Site {
                line: node.start_position().row + 1,
                kind: "identifier",
                reason: "already declared or referenced".into(),
                text: line_text(source, node.start_byte()),
            });
        }
    });
    sites
}

fn uses_semicolons(target: &Target<'_>, source: &str, syn: &Syntax) -> bool {
    let statements: Vec<Node<'_>> = match target.closure_body() {
        Some(body) if syn.blocks.contains(&body.kind()) => named_children(body),
        Some(_) => Vec::new(),
        None => match target.shape {
            Shape::Statements { .. } => target.nodes.clone(),
            _ => Vec::new(),
        },
    };
    if statements.is_empty() {
        return source.lines().any(|l| l.trim_end().ends_with(';'));
    }
    statements
        .iter()
        .any(|n| text(*n, source).trim_end().ends_with(';'))
}

fn edit(source: &str, start: usize, end: usize, after: String) -> EditSpan {
    let (start_row, start_col) = row_col(source, start);
    let (end_row, end_col) = row_col(source, end);
    EditSpan {
        span: IdentifierSpan {
            start_byte: start,
            end_byte: end,
            start_row,
            start_col,
            end_row,
            end_col,
        },
        before: slice(source, start, end).to_string(),
        after,
    }
}

// === Text helpers ===

fn text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    source.get(node.start_byte()..node.end_byte()).unwrap_or("")
}

fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn walk<'t>(node: Node<'t>, visit: &mut impl FnMut(Node<'t>)) {
    visit(node);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, visit);
    }
}

fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' && i + 1 < source.len() {
            starts.push(i + 1);
        }
    }
    starts
}

/// `source[start..end]`, or "" off a char boundary. Every offset here is a
/// tree-sitter node or line boundary, so the fallback never fires in practice.
fn slice(source: &str, start: usize, end: usize) -> &str {
    source.get(start..end).unwrap_or("")
}

/// 1-based line of a byte offset.
fn line_of(source: &str, byte: usize) -> usize {
    slice(source, 0, byte.min(source.len()))
        .matches('\n')
        .count()
        + 1
}

fn row_col(source: &str, byte: usize) -> (usize, usize) {
    let before = slice(source, 0, byte);
    let row = before.matches('\n').count();
    let col = before.rfind('\n').map_or(byte, |nl| byte - nl - 1);
    (row, col)
}

fn line_text(source: &str, byte: usize) -> String {
    let start = slice(source, 0, byte).rfind('\n').map_or(0, |nl| nl + 1);
    let end = slice(source, byte, source.len())
        .find('\n')
        .map_or(source.len(), |nl| byte + nl);
    slice(source, start, end).trim().to_string()
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}

/// Leading whitespace of the line holding `byte`.
fn line_indent(source: &str, byte: usize) -> String {
    let start = slice(source, 0, byte).rfind('\n').map_or(0, |nl| nl + 1);
    slice(source, start, source.len())
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// The file's indentation step: a tab, or its smallest space indent.
fn indent_unit(source: &str, dialect: Dialect) -> String {
    let mut smallest: Option<usize> = None;
    for line in source.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with('\t') {
            return "\t".into();
        }
        let n = line.len() - line.trim_start_matches(' ').len();
        if n > 0 && smallest.is_none_or(|s| n < s) {
            smallest = Some(n);
        }
    }
    let fallback = if matches!(dialect, Dialect::TypeScript | Dialect::JavaScript) {
        2
    } else {
        4
    };
    " ".repeat(smallest.unwrap_or(fallback).min(8))
}

/// Re-home `text` whose first line starts mid-line and whose later lines
/// carry `from` as their base indentation onto `to`.
fn reindent(text: &str, from: &str, to: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(i, line)| {
            let stripped = if i == 0 {
                line.trim_start()
            } else {
                line.strip_prefix(from).unwrap_or_else(|| line.trim_start())
            };
            if stripped.trim().is_empty() {
                String::new()
            } else {
                format!("{to}{stripped}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Indent a caller-supplied helper so its least-indented line sits at `base`.
fn reindent_block(raw: &str, base: &str) -> String {
    let lines: Vec<&str> = raw.lines().skip_while(|l| l.trim().is_empty()).collect();
    let lines = &lines[..lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |i| i + 1)];
    let common = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                format!("{base}{}", slice(line, common, line.len()).trim_end())
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn display_list(names: &[String]) -> String {
    if names.is_empty() {
        "(none)".into()
    } else {
        format!("[{}]", names.join(", "))
    }
}

fn dialect_name(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Rust => "Rust",
        Dialect::TypeScript => "TypeScript",
        Dialect::JavaScript => "JavaScript",
        Dialect::Python => "Python",
    }
}
