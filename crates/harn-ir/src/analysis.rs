//! Entry points: run every invariant over a program, or explain one.
//!
//! `analyze_program` collects the handlers out of a parsed program and reports
//! each invariant's diagnostics; `explain_handler_invariant` renders the
//! reasoning for a single handler/invariant pair.

use harn_parser::{Node, SNode};

use crate::builder::*;
use crate::spec_parse::*;
use crate::types::*;
pub fn analyze_program(program: &[SNode]) -> AnalysisReport {
    let (handlers, mut diagnostics) = collect_handlers(program);
    let mut irs = Vec::with_capacity(handlers.len());

    for handler in handlers {
        let ir = HandlerIrBuilder::new(&handler).build();
        for spec in &handler.invariants {
            match instantiate_invariant(spec) {
                Ok(invariant) => diagnostics.extend(invariant.check(&ir)),
                Err(diag) => diagnostics.push(diag.with_handler(&handler.name)),
            }
        }
        irs.push(ir);
    }

    AnalysisReport {
        handlers: irs,
        diagnostics,
    }
}

pub fn explain_handler_invariant(
    program: &[SNode],
    handler_name: &str,
    invariant_name: &str,
) -> Result<Vec<InvariantDiagnostic>, String> {
    let (handlers, config_diags) = collect_handlers(program);
    let Some(handler) = handlers.iter().find(|handler| handler.name == handler_name) else {
        return Err(format!("handler `{handler_name}` was not found"));
    };
    if let Some(diag) = config_diags
        .into_iter()
        .find(|diag| diag.handler == handler.name || diag.handler.is_empty())
    {
        return Ok(vec![diag]);
    }
    let normalized = normalize_invariant_name(invariant_name)
        .ok_or_else(|| format!("unknown invariant `{invariant_name}`"))?;
    let Some(spec) = handler
        .invariants
        .iter()
        .find(|spec| spec.name == normalized)
        .cloned()
    else {
        return Err(format!(
            "handler `{handler_name}` does not declare `@invariant(\"{normalized}\")`"
        ));
    };
    let invariant = instantiate_invariant(&spec).map_err(|diag| diag.message)?;
    let ir = HandlerIrBuilder::new(handler).build();
    Ok(invariant.check(&ir))
}

fn collect_handlers(program: &[SNode]) -> (Vec<HandlerSpec>, Vec<InvariantDiagnostic>) {
    let mut handlers = Vec::new();
    let mut diagnostics = Vec::new();

    for node in program {
        let (attributes, inner) = match &node.node {
            Node::AttributedDecl { attributes, inner } => (attributes.as_slice(), inner.as_ref()),
            _ => (&[][..], node),
        };
        let Some((name, kind, params, body)) = handler_decl(inner) else {
            continue;
        };
        let (invariants, mut invariant_diags) = parse_invariant_specs(attributes, name, kind);
        diagnostics.append(&mut invariant_diags);
        let capability_handles = params
            .iter()
            .filter_map(|param| {
                let harn_parser::TypeExpr::Named(type_name) = param.type_expr.as_ref()? else {
                    return None;
                };
                let capability =
                    harn_builtin_meta::CapabilityId::from_type_name(type_name.as_str())?;
                Some((param.name.clone(), capability))
            })
            .collect();
        handlers.push(HandlerSpec {
            name: name.to_string(),
            kind,
            span: inner.span,
            body: body.to_vec(),
            invariants,
            capability_handles,
        });
    }

    (handlers, diagnostics)
}

fn handler_decl(node: &SNode) -> Option<(&str, HandlerKind, &[harn_parser::TypedParam], &[SNode])> {
    match &node.node {
        Node::FnDecl {
            name, params, body, ..
        } => Some((name.as_str(), HandlerKind::Function, params, body)),
        Node::ToolDecl {
            name, params, body, ..
        } => Some((name.as_str(), HandlerKind::Tool, params, body)),
        Node::Pipeline {
            name, params, body, ..
        } => Some((name.as_str(), HandlerKind::Pipeline, params, body)),
        _ => None,
    }
}
