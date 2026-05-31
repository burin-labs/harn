use std::collections::BTreeMap;

use crate::runtime_limits::RuntimeLimits;
use crate::value::{values_equal, VmValue};

use super::ast::{BinOp, Expr, IfBranch, Node, PathSeg, UnOp};
use super::error::TemplateError;
use super::filters::apply_filter;
use super::{BranchDecision, BranchKind, PromptSourceSpan, PromptSpanKind, TemplateAsset};

#[derive(Default, Debug, Clone)]
pub(super) struct Scope<'a> {
    /// Root bindings passed by the caller.
    root: Option<&'a BTreeMap<String, VmValue>>,
    /// Override stack — pushed for `for`-loop variables and `include with`.
    overrides: Vec<BTreeMap<String, VmValue>>,
}

impl<'a> Scope<'a> {
    pub(super) fn new(root: Option<&'a BTreeMap<String, VmValue>>) -> Self {
        Self {
            root,
            overrides: Vec::new(),
        }
    }

    fn lookup(&self, name: &str) -> Option<VmValue> {
        for layer in self.overrides.iter().rev() {
            if let Some(v) = layer.get(name) {
                return Some(v.clone());
            }
        }
        self.root.and_then(|m| m.get(name)).cloned()
    }

    fn push(&mut self, layer: BTreeMap<String, VmValue>) {
        self.overrides.push(layer);
    }

    fn pop(&mut self) {
        self.overrides.pop();
    }

    /// Materialize a flat BTreeMap merging root + all overrides. Used when
    /// passing a fresh snapshot into an included partial.
    fn flatten(&self) -> BTreeMap<String, VmValue> {
        let mut out = BTreeMap::new();
        if let Some(r) = self.root {
            for (k, v) in r.iter() {
                out.insert(k.clone(), v.clone());
            }
        }
        for layer in &self.overrides {
            for (k, v) in layer {
                out.insert(k.clone(), v.clone());
            }
        }
        out
    }
}

pub(super) struct RenderCtx {
    pub(super) current_asset: TemplateAsset,
    pub(super) include_stack: Vec<String>,
    /// When inside an `{% include %}`, this holds the include-call's
    /// span (in the *parent* template). Every span emitted during the
    /// recursive render points at this as its `parent_span`, so the
    /// IDE can walk a breadcrumb back through nested includes
    /// (#96). `None` at the top level.
    pub(super) current_include_parent: Option<Box<PromptSourceSpan>>,
    /// When set, every conditional / section node appends a
    /// [`BranchDecision`] here so callers can reconstruct which
    /// capability-adaptive branches fired. Only populated when the
    /// top-level render entry-point opts in.
    pub(super) branch_trace: Option<Vec<BranchDecision>>,
}

/// Template URI reported alongside every span. Filesystem templates use
/// their path string, embedded stdlib templates use `std://...`, and
/// inline templates without a source path use an empty string.
fn current_template_uri(rc: &RenderCtx) -> String {
    rc.current_asset.uri.clone()
}

pub(super) fn render_nodes(
    nodes: &[Node],
    scope: &mut Scope<'_>,
    rc: &mut RenderCtx,
    out: &mut String,
    mut spans: Option<&mut Vec<PromptSourceSpan>>,
) -> Result<(), TemplateError> {
    for n in nodes {
        render_node(n, scope, rc, out, spans.as_deref_mut())?;
    }
    Ok(())
}

fn render_node(
    node: &Node,
    scope: &mut Scope<'_>,
    rc: &mut RenderCtx,
    out: &mut String,
    mut spans: Option<&mut Vec<PromptSourceSpan>>,
) -> Result<(), TemplateError> {
    let start = out.len();
    match node {
        Node::Text(s) => {
            out.push_str(s);
            if let Some(spans) = spans.as_deref_mut() {
                spans.push(PromptSourceSpan {
                    template_line: 0,
                    template_col: 0,
                    output_start: start,
                    output_end: out.len(),
                    kind: PromptSpanKind::Text,
                    parent_span: rc.current_include_parent.clone(),
                    template_uri: current_template_uri(rc),
                    bound_value: None,
                });
            }
        }
        Node::Expr { expr, line, col } => {
            let v = eval_expr(expr, scope, *line, *col)?;
            let rendered = display_value(&v);
            out.push_str(&rendered);
            if let Some(spans) = spans.as_deref_mut() {
                spans.push(PromptSourceSpan {
                    template_line: *line,
                    template_col: *col,
                    output_start: start,
                    output_end: out.len(),
                    kind: PromptSpanKind::Expr,
                    parent_span: rc.current_include_parent.clone(),
                    template_uri: current_template_uri(rc),
                    bound_value: Some(truncate_for_preview(&rendered)),
                });
            }
        }
        Node::LegacyBareInterp { ident } => {
            let (rendered, preview) = match scope.lookup(ident) {
                Some(v) => {
                    let s = display_value(&v);
                    (s.clone(), Some(truncate_for_preview(&s)))
                }
                None => (format!("{{{{{ident}}}}}"), None),
            };
            out.push_str(&rendered);
            if let Some(spans) = spans.as_deref_mut() {
                spans.push(PromptSourceSpan {
                    template_line: 0,
                    template_col: 0,
                    output_start: start,
                    output_end: out.len(),
                    kind: PromptSpanKind::LegacyBareInterp,
                    parent_span: rc.current_include_parent.clone(),
                    template_uri: current_template_uri(rc),
                    bound_value: preview,
                });
            }
        }
        Node::If {
            branches,
            else_branch,
            line,
            col,
        } => {
            let mut matched: Option<usize> = None;
            for (idx, branch) in branches.iter().enumerate() {
                let v = eval_expr(&branch.cond, scope, branch.line, branch.col)?;
                if truthy(&v) {
                    render_nodes(&branch.body, scope, rc, out, spans.as_deref_mut())?;
                    matched = Some(idx);
                    break;
                }
            }
            if matched.is_none() {
                if let Some(eb) = else_branch {
                    render_nodes(eb, scope, rc, out, spans.as_deref_mut())?;
                }
            }
            record_branch_if(rc, *line, *col, branches, else_branch.is_some(), matched);
            if let Some(spans) = spans.as_deref_mut() {
                spans.push(PromptSourceSpan {
                    template_line: *line,
                    template_col: *col,
                    output_start: start,
                    output_end: out.len(),
                    kind: PromptSpanKind::If,
                    parent_span: rc.current_include_parent.clone(),
                    template_uri: current_template_uri(rc),
                    bound_value: None,
                });
            }
        }
        Node::For {
            value_var,
            key_var,
            iter,
            body,
            empty,
            line,
            col,
        } => {
            let v = eval_expr(iter, scope, *line, *col)?;
            let items: Vec<(VmValue, VmValue)> =
                iterable_items(&v).map_err(|m| TemplateError::new(*line, *col, m))?;
            if items.is_empty() {
                if let Some(eb) = empty {
                    render_nodes(eb, scope, rc, out, spans.as_deref_mut())?;
                }
            } else {
                let length = items.len() as i64;
                for (idx, (k, val)) in items.iter().enumerate() {
                    let mut layer: BTreeMap<String, VmValue> = BTreeMap::new();
                    layer.insert(value_var.clone(), val.clone());
                    if let Some(kv) = key_var {
                        layer.insert(kv.clone(), k.clone());
                    }
                    let mut loop_map: BTreeMap<String, VmValue> = BTreeMap::new();
                    loop_map.insert("index".into(), VmValue::Int(idx as i64 + 1));
                    loop_map.insert("index0".into(), VmValue::Int(idx as i64));
                    loop_map.insert("first".into(), VmValue::Bool(idx == 0));
                    loop_map.insert("last".into(), VmValue::Bool(idx as i64 == length - 1));
                    loop_map.insert("length".into(), VmValue::Int(length));
                    layer.insert("loop".into(), VmValue::Dict(std::sync::Arc::new(loop_map)));
                    scope.push(layer);
                    let iter_start = out.len();
                    let res = render_nodes(body, scope, rc, out, spans.as_deref_mut());
                    scope.pop();
                    res?;
                    if let Some(spans) = spans.as_deref_mut() {
                        spans.push(PromptSourceSpan {
                            template_line: *line,
                            template_col: *col,
                            output_start: iter_start,
                            output_end: out.len(),
                            kind: PromptSpanKind::ForIteration,
                            parent_span: rc.current_include_parent.clone(),
                            template_uri: current_template_uri(rc),
                            bound_value: None,
                        });
                    }
                }
            }
        }
        Node::Include {
            path,
            with,
            line,
            col,
        } => {
            let path_val = eval_expr(path, scope, *line, *col)?;
            let path_str = match path_val {
                VmValue::String(s) => s.to_string(),
                other => {
                    return Err(TemplateError::new(
                        *line,
                        *col,
                        format!("include path must be a string (got {})", other.type_name()),
                    ));
                }
            };
            let asset = super::assets::resolve_include(&rc.current_asset, &path_str, *line, *col)?;
            if rc.include_stack.iter().any(|id| id == &asset.id) {
                let chain = rc
                    .include_stack
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(" → ");
                return Err(TemplateError::new(
                    *line,
                    *col,
                    format!("circular include detected: {chain} → {}", asset.id),
                ));
            }
            let max_depth = RuntimeLimits::DEFAULT.max_template_include_depth;
            if rc.include_stack.len() >= max_depth {
                return Err(TemplateError::new(
                    *line,
                    *col,
                    format!("include depth exceeded ({max_depth} levels)"),
                ));
            }
            let mut child_bindings = scope.flatten();
            if let Some(pairs) = with {
                for (k, e) in pairs {
                    let v = eval_expr(e, scope, *line, *col)?;
                    child_bindings.insert(k.clone(), v);
                }
            }
            let child_nodes = super::assets::parse_cached(&asset)?;
            let mut child_scope = Scope::new(Some(&child_bindings));
            let saved_asset = rc.current_asset.clone();
            let saved_parent = rc.current_include_parent.clone();
            let include_call_span = PromptSourceSpan {
                template_line: *line,
                template_col: *col,
                output_start: start,
                output_end: start,
                kind: PromptSpanKind::Include,
                bound_value: None,
                parent_span: saved_parent.clone(),
                template_uri: current_template_uri(rc),
            };
            rc.current_asset = asset.clone();
            rc.current_include_parent = Some(Box::new(include_call_span));
            rc.include_stack.push(asset.id);
            let res = render_nodes(
                &child_nodes,
                &mut child_scope,
                rc,
                out,
                spans.as_deref_mut(),
            );
            rc.include_stack.pop();
            rc.current_asset = saved_asset;
            rc.current_include_parent = saved_parent;
            res?;
            if let Some(spans) = spans.as_mut() {
                spans.push(PromptSourceSpan {
                    template_line: *line,
                    template_col: *col,
                    output_start: start,
                    output_end: out.len(),
                    kind: PromptSpanKind::Include,
                    parent_span: rc.current_include_parent.clone(),
                    template_uri: current_template_uri(rc),
                    bound_value: None,
                });
            }
        }
        Node::Section {
            name,
            args,
            body,
            line,
            col,
        } => {
            let mut evaluated_args = BTreeMap::new();
            for (key, expr) in args {
                evaluated_args.insert(key.clone(), eval_expr(expr, scope, *line, *col)?);
            }
            let mut body_out = String::new();
            let mut body_spans = spans.as_ref().map(|_| Vec::new());
            render_nodes(body, scope, rc, &mut body_out, body_spans.as_mut())?;
            let llm = scope.lookup("llm");
            let section = super::sections::render_section(
                name,
                &body_out,
                &evaluated_args,
                llm.as_ref(),
                *line,
                *col,
            )?;
            let template_uri = current_template_uri(rc);
            if let Some(trace) = rc.branch_trace.as_mut() {
                trace.push(BranchDecision {
                    kind: BranchKind::Section,
                    template_uri,
                    line: *line,
                    col: *col,
                    branch_id: section.envelope.to_string(),
                    branch_label: Some(name.clone()),
                });
            }
            out.push_str(&section.text);
            if let Some(spans) = spans {
                if let (Some(child_spans), Some(body_output_start)) =
                    (body_spans, section.body_output_start)
                {
                    let body_start = section.body_source_start;
                    let body_end = section.body_source_end;
                    for mut span in child_spans {
                        if span.output_end <= body_start || span.output_start >= body_end {
                            continue;
                        }
                        span.output_start =
                            start + body_output_start + span.output_start.max(body_start)
                                - body_start;
                        span.output_end =
                            start + body_output_start + span.output_end.min(body_end) - body_start;
                        spans.push(span);
                    }
                }
                spans.push(PromptSourceSpan {
                    template_line: *line,
                    template_col: *col,
                    output_start: start,
                    output_end: out.len(),
                    kind: PromptSpanKind::Section,
                    parent_span: rc.current_include_parent.clone(),
                    template_uri: current_template_uri(rc),
                    bound_value: Some(name.clone()),
                });
            }
        }
    }
    Ok(())
}

/// Record a branch decision for an `{{ if }}` chain.
///
/// `matched` is the 0-based index of the conditional whose body fired;
/// `None` means none of the conditions were truthy. `had_else` is true
/// when the source carried an `{{ else }}` branch, which is the
/// label used in the trace when no condition matched but an else
/// rendered.
fn record_branch_if(
    rc: &mut RenderCtx,
    line: usize,
    col: usize,
    branches: &[IfBranch],
    had_else: bool,
    matched: Option<usize>,
) {
    if rc.branch_trace.is_none() {
        return;
    }
    let template_uri = current_template_uri(rc);
    let (branch_id, branch_label, anchor_line, anchor_col) = match matched {
        Some(0) => (
            "if".to_string(),
            describe_condition(&branches[0].cond),
            branches[0].line,
            branches[0].col,
        ),
        Some(idx) => (
            format!("elif:{idx}"),
            describe_condition(&branches[idx].cond),
            branches[idx].line,
            branches[idx].col,
        ),
        None if had_else => ("else".to_string(), None, line, col),
        None => ("none".to_string(), None, line, col),
    };
    if let Some(trace) = rc.branch_trace.as_mut() {
        trace.push(BranchDecision {
            kind: BranchKind::If,
            template_uri,
            line: anchor_line,
            col: anchor_col,
            branch_id,
            branch_label,
        });
    }
}

/// Describe an `{{ if expr }}` condition compactly so the trace shows
/// *what* gated the branch (e.g. `llm.capabilities.native_tools`). The
/// renderer doesn't depend on perfect source reconstruction here — the
/// trace already carries line/col for jump-to-source — but a readable
/// label helps in the portal's variant-resolution panel.
fn describe_condition(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Path(segs) => Some(format_path(segs)),
        Expr::Unary(UnOp::Not, inner) => describe_condition(inner).map(|s| format!("!{s}")),
        Expr::Binary(op, a, b) => {
            let lhs = describe_value(a);
            let rhs = describe_value(b);
            let op_str = match op {
                BinOp::Eq => "==",
                BinOp::Neq => "!=",
                BinOp::Lt => "<",
                BinOp::Le => "<=",
                BinOp::Gt => ">",
                BinOp::Ge => ">=",
                BinOp::And => "&&",
                BinOp::Or => "||",
            };
            Some(format!("{lhs} {op_str} {rhs}"))
        }
        Expr::Filter(inner, name, _) => describe_condition(inner).map(|s| format!("{s} | {name}")),
        _ => Some(describe_value(expr)),
    }
}

fn describe_value(expr: &Expr) -> String {
    match expr {
        Expr::Nil => "nil".to_string(),
        Expr::Bool(b) => b.to_string(),
        Expr::Int(n) => n.to_string(),
        Expr::Float(f) => f.to_string(),
        Expr::Str(s) => format!("\"{s}\""),
        Expr::Path(segs) => format_path(segs),
        _ => "<expr>".to_string(),
    }
}

fn format_path(segs: &[PathSeg]) -> String {
    let mut out = String::new();
    for (idx, seg) in segs.iter().enumerate() {
        match seg {
            PathSeg::Field(name) | PathSeg::Key(name) => {
                if idx > 0 {
                    out.push('.');
                }
                out.push_str(name);
            }
            PathSeg::Index(i) => {
                out.push_str(&format!("[{i}]"));
            }
        }
    }
    out
}

/// Cap a rendered value's preview at 80 chars so span records don't
/// carry kilobyte prompt chunks. The IDE can fetch the full text by
/// reading the rendered string at `output_start..output_end`.
fn truncate_for_preview(s: &str) -> String {
    const MAX: usize = 80;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let truncated: String = s.chars().take(MAX - 1).collect();
    format!("{truncated}…")
}

fn eval_expr(
    expr: &Expr,
    scope: &Scope<'_>,
    line: usize,
    col: usize,
) -> Result<VmValue, TemplateError> {
    match expr {
        Expr::Nil => Ok(VmValue::Nil),
        Expr::Bool(b) => Ok(VmValue::Bool(*b)),
        Expr::Int(n) => Ok(VmValue::Int(*n)),
        Expr::Float(f) => Ok(VmValue::Float(*f)),
        Expr::Str(s) => Ok(VmValue::String(std::sync::Arc::from(s.as_str()))),
        Expr::Path(segs) => Ok(resolve_path(segs, scope)),
        Expr::Unary(UnOp::Not, inner) => {
            let v = eval_expr(inner, scope, line, col)?;
            Ok(VmValue::Bool(!truthy(&v)))
        }
        Expr::Binary(op, a, b) => {
            match op {
                BinOp::And => {
                    let av = eval_expr(a, scope, line, col)?;
                    if !truthy(&av) {
                        return Ok(av);
                    }
                    return eval_expr(b, scope, line, col);
                }
                BinOp::Or => {
                    let av = eval_expr(a, scope, line, col)?;
                    if truthy(&av) {
                        return Ok(av);
                    }
                    return eval_expr(b, scope, line, col);
                }
                _ => {}
            }
            let av = eval_expr(a, scope, line, col)?;
            let bv = eval_expr(b, scope, line, col)?;
            Ok(apply_cmp(*op, &av, &bv))
        }
        Expr::Filter(inner, name, args) => {
            let v = eval_expr(inner, scope, line, col)?;
            let arg_vals = args
                .iter()
                .map(|e| eval_expr(e, scope, line, col))
                .collect::<Result<Vec<_>, _>>()?;
            apply_filter(name, &v, &arg_vals, line, col)
        }
    }
}

fn resolve_path(segs: &[PathSeg], scope: &Scope<'_>) -> VmValue {
    let mut cur: VmValue = match segs.first() {
        Some(PathSeg::Field(n)) => match scope.lookup(n) {
            Some(v) => v,
            None => return VmValue::Nil,
        },
        _ => return VmValue::Nil,
    };
    for seg in &segs[1..] {
        cur = match (seg, &cur) {
            (PathSeg::Field(n), VmValue::Dict(d)) => d.get(n).cloned().unwrap_or(VmValue::Nil),
            (PathSeg::Key(k), VmValue::Dict(d)) => d.get(k).cloned().unwrap_or(VmValue::Nil),
            (PathSeg::Index(i), VmValue::List(items)) => {
                let idx = if *i < 0 { items.len() as i64 + *i } else { *i };
                if idx < 0 || (idx as usize) >= items.len() {
                    VmValue::Nil
                } else {
                    items[idx as usize].clone()
                }
            }
            (PathSeg::Index(i), VmValue::String(s)) => {
                let chars: Vec<char> = s.chars().collect();
                let idx = if *i < 0 { chars.len() as i64 + *i } else { *i };
                if idx < 0 || (idx as usize) >= chars.len() {
                    VmValue::Nil
                } else {
                    VmValue::String(std::sync::Arc::from(chars[idx as usize].to_string()))
                }
            }
            _ => VmValue::Nil,
        };
    }
    cur
}

pub(super) fn truthy(v: &VmValue) -> bool {
    match v {
        VmValue::Nil => false,
        VmValue::Bool(b) => *b,
        VmValue::Int(n) => *n != 0,
        VmValue::Float(f) => *f != 0.0,
        VmValue::String(s) => !s.trim().is_empty(),
        VmValue::List(items) => !items.is_empty(),
        VmValue::Dict(d) => !d.is_empty(),
        _ => true,
    }
}

fn apply_cmp(op: BinOp, a: &VmValue, b: &VmValue) -> VmValue {
    match op {
        BinOp::Eq => VmValue::Bool(values_equal(a, b)),
        BinOp::Neq => VmValue::Bool(!values_equal(a, b)),
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            let ord = compare(a, b);
            match (op, ord) {
                (BinOp::Lt, Some(o)) => VmValue::Bool(o == std::cmp::Ordering::Less),
                (BinOp::Le, Some(o)) => VmValue::Bool(o != std::cmp::Ordering::Greater),
                (BinOp::Gt, Some(o)) => VmValue::Bool(o == std::cmp::Ordering::Greater),
                (BinOp::Ge, Some(o)) => VmValue::Bool(o != std::cmp::Ordering::Less),
                _ => VmValue::Bool(false),
            }
        }
        BinOp::And | BinOp::Or => unreachable!(),
    }
}

fn compare(a: &VmValue, b: &VmValue) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (VmValue::Int(x), VmValue::Int(y)) => Some(x.cmp(y)),
        (VmValue::Float(x), VmValue::Float(y)) => x.partial_cmp(y),
        (VmValue::Int(x), VmValue::Float(y)) => (*x as f64).partial_cmp(y),
        (VmValue::Float(x), VmValue::Int(y)) => x.partial_cmp(&(*y as f64)),
        (VmValue::String(x), VmValue::String(y)) => Some(x.as_ref().cmp(y.as_ref())),
        _ => None,
    }
}

fn iterable_items(v: &VmValue) -> Result<Vec<(VmValue, VmValue)>, String> {
    match v {
        VmValue::List(items) => Ok(items
            .iter()
            .enumerate()
            .map(|(i, it)| (VmValue::Int(i as i64), it.clone()))
            .collect()),
        VmValue::Dict(d) => Ok(d
            .iter()
            .map(|(k, v)| (VmValue::String(std::sync::Arc::from(k.as_str())), v.clone()))
            .collect()),
        VmValue::Set(items) => Ok(items
            .iter()
            .enumerate()
            .map(|(i, it)| (VmValue::Int(i as i64), it.clone()))
            .collect()),
        VmValue::Range(r) => {
            let mut out = Vec::new();
            let len = r.len();
            for i in 0..len {
                if let Some(v) = r.get(i) {
                    out.push((VmValue::Int(i), VmValue::Int(v)));
                }
            }
            Ok(out)
        }
        VmValue::Nil => Ok(Vec::new()),
        other => Err(format!(
            "cannot iterate over {} — expected list, dict, set, or range",
            other.type_name()
        )),
    }
}

pub(super) fn display_value(v: &VmValue) -> String {
    match v {
        VmValue::Nil => String::new(),
        other => other.display(),
    }
}
