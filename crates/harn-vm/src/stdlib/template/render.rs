use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::value::{values_equal, VmValue};

use super::ast::{BinOp, Expr, Node, PathSeg, UnOp};
use super::error::TemplateError;
use super::filters::apply_filter;
use super::{PromptSourceSpan, PromptSpanKind};

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
    pub(super) base: Option<PathBuf>,
    pub(super) include_stack: Vec<PathBuf>,
    pub(super) current_path: Option<PathBuf>,
    /// When inside an `{% include %}`, this holds the include-call's
    /// span (in the *parent* template). Every span emitted during the
    /// recursive render points at this as its `parent_span`, so the
    /// IDE can walk a breadcrumb back through nested includes
    /// (#96). `None` at the top level.
    pub(super) current_include_parent: Option<Box<PromptSourceSpan>>,
}

/// Template URI reported alongside every span — the absolute path of
/// the currently-rendering `.harn.prompt` file. Empty string when the
/// renderer doesn't know (inline template arg or synthetic snippet).
fn current_template_uri(rc: &RenderCtx) -> String {
    rc.current_path
        .as_deref()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .unwrap_or_default()
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
            let mut matched = false;
            for (cond, body) in branches {
                let v = eval_expr(cond, scope, *line, *col)?;
                if truthy(&v) {
                    render_nodes(body, scope, rc, out, spans.as_deref_mut())?;
                    matched = true;
                    break;
                }
            }
            if !matched {
                if let Some(eb) = else_branch {
                    render_nodes(eb, scope, rc, out, spans.as_deref_mut())?;
                }
            }
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
                    layer.insert("loop".into(), VmValue::Dict(Rc::new(loop_map)));
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
            let resolved: PathBuf = if Path::new(&path_str).is_absolute() {
                PathBuf::from(&path_str)
            } else if let Some(base) = &rc.base {
                base.join(&path_str)
            } else {
                crate::stdlib::process::resolve_source_asset_path(&path_str)
            };
            let canonical = resolved.canonicalize().unwrap_or(resolved.clone());
            if rc.include_stack.iter().any(|p| p == &canonical) {
                let chain = rc
                    .include_stack
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" → ");
                return Err(TemplateError::new(
                    *line,
                    *col,
                    format!(
                        "circular include detected: {chain} → {}",
                        canonical.display()
                    ),
                ));
            }
            if rc.include_stack.len() > 32 {
                return Err(TemplateError::new(
                    *line,
                    *col,
                    "include depth exceeded (32 levels)",
                ));
            }
            let contents = std::fs::read_to_string(&resolved).map_err(|e| {
                TemplateError::new(
                    *line,
                    *col,
                    format!(
                        "failed to read included template {}: {e}",
                        resolved.display()
                    ),
                )
            })?;
            let new_base = resolved.parent().map(Path::to_path_buf);
            let mut child_bindings = scope.flatten();
            if let Some(pairs) = with {
                for (k, e) in pairs {
                    let v = eval_expr(e, scope, *line, *col)?;
                    child_bindings.insert(k.clone(), v);
                }
            }
            let child_nodes = super::parser::parse(&contents).map_err(|mut e| {
                if e.path.is_none() {
                    e.path = Some(resolved.clone());
                }
                e
            })?;
            let mut child_scope = Scope::new(Some(&child_bindings));
            let saved_base = rc.base.clone();
            let saved_current = rc.current_path.clone();
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
            rc.base = new_base;
            rc.current_path = Some(resolved.clone());
            rc.current_include_parent = Some(Box::new(include_call_span));
            rc.include_stack.push(canonical);
            let res = render_nodes(
                &child_nodes,
                &mut child_scope,
                rc,
                out,
                spans.as_deref_mut(),
            );
            rc.include_stack.pop();
            rc.base = saved_base;
            rc.current_path = saved_current;
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
    }
    Ok(())
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
        Expr::Str(s) => Ok(VmValue::String(Rc::from(s.as_str()))),
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
                    VmValue::String(Rc::from(chars[idx as usize].to_string()))
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
            .map(|(k, v)| (VmValue::String(Rc::from(k.as_str())), v.clone()))
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
