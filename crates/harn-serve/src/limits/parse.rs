//! Parse `@limits(...)` and `@budget(...)` attribute literals into the
//! typed [`RouteLimits`] / [`BudgetSpec`] structs the rest of the
//! primitive consumes.
//!
//! Acceptable forms:
//!
//! ```harn
//! @limits(
//!     per_tenant: "100/min",
//!     per_scope:  "1000/min",
//!     per_route:  "5000/min",
//!     burst:      50,
//!     algorithm:  "token_bucket",
//!     in_flight_max: 20,
//! )
//! @budget(
//!     llm_cost_usd: 0.50,
//!     pg_queries:   50,
//!     mcp_calls:    20,
//! )
//! ```
//!
//! Parsing is tolerant: unknown keys are ignored (forward-compat with
//! richer specs landed later); missing keys leave the field unset.
//! Quota strings accept `N/sec`, `N/min`, `N/hour` (or `s`/`m`/`h`).

use std::collections::BTreeSet;

use harn_parser::{Attribute, Node};

use super::{Algorithm, BudgetSpec, Quota, RouteLimits};

/// Collect both `@limits(...)` and `@budget(...)` declarations from a
/// declaration's attribute list. Each attribute kind may appear at most
/// once; multiple occurrences merge by taking the last-write-wins.
pub fn limits_and_budget_from_attributes(
    attrs: &[Attribute],
) -> (Option<RouteLimits>, Option<BudgetSpec>) {
    let mut limits: Option<RouteLimits> = None;
    let mut budget: Option<BudgetSpec> = None;

    for attr in attrs {
        match attr.name.as_str() {
            "limits" => limits = Some(parse_limits(attr, limits.take())),
            "budget" => budget = Some(parse_budget(attr, budget.take())),
            _ => continue,
        }
    }
    (limits, budget)
}

fn parse_limits(attr: &Attribute, base: Option<RouteLimits>) -> RouteLimits {
    let mut limits = base.unwrap_or_default();
    for arg in &attr.args {
        let Some(key) = arg.name.as_deref() else {
            continue;
        };
        match key {
            "per_tenant" => {
                if let Some(quota) = string_value(arg).and_then(Quota::parse) {
                    limits.per_tenant = Some(quota);
                }
            }
            "per_scope" => {
                if let Some(quota) = string_value(arg).and_then(Quota::parse) {
                    limits.per_scope = Some(quota);
                }
            }
            "per_route" => {
                if let Some(quota) = string_value(arg).and_then(Quota::parse) {
                    limits.per_route = Some(quota);
                }
            }
            "burst" => {
                if let Some(value) = int_value(arg) {
                    limits.burst = Some(value.max(1) as u32);
                }
            }
            "algorithm" => {
                if let Some(algo) = string_value(arg).as_deref().and_then(Algorithm::parse) {
                    limits.algorithm = algo;
                }
            }
            "in_flight_max" => {
                if let Some(value) = int_value(arg) {
                    limits.in_flight_max = Some(value.max(1) as u32);
                }
            }
            _ => continue,
        }
    }
    limits
}

fn parse_budget(attr: &Attribute, base: Option<BudgetSpec>) -> BudgetSpec {
    let mut budget = base.unwrap_or_default();
    for arg in &attr.args {
        let Some(key) = arg.name.as_deref() else {
            continue;
        };
        match key {
            "llm_cost_usd" => {
                if let Some(value) = float_value(arg) {
                    budget.llm_cost_usd = Some(value.max(0.0));
                }
            }
            "llm_tokens" => {
                if let Some(value) = int_value(arg) {
                    budget.llm_tokens = Some(value.max(0) as u64);
                }
            }
            "pg_queries" => {
                if let Some(value) = int_value(arg) {
                    budget.pg_queries = Some(value.max(0) as u64);
                }
            }
            "mcp_calls" => {
                if let Some(value) = int_value(arg) {
                    budget.mcp_calls = Some(value.max(0) as u64);
                }
            }
            _ => continue,
        }
    }
    budget
}

fn string_value(arg: &harn_parser::AttributeArg) -> Option<String> {
    match &arg.value.node {
        Node::StringLiteral(s) | Node::RawStringLiteral(s) => Some(s.clone()),
        _ => None,
    }
}

fn int_value(arg: &harn_parser::AttributeArg) -> Option<i64> {
    match &arg.value.node {
        Node::IntLiteral(n) => Some(*n),
        Node::FloatLiteral(f) if f.is_finite() && f.fract() == 0.0 => Some(*f as i64),
        _ => None,
    }
}

fn float_value(arg: &harn_parser::AttributeArg) -> Option<f64> {
    match &arg.value.node {
        Node::FloatLiteral(f) => Some(*f),
        Node::IntLiteral(n) => Some(*n as f64),
        _ => None,
    }
}

/// Forward-declared in [`super::ExportedFunctionLimits`] / consumed by
/// `exports.rs`. Centralised here so attribute parsing tests can
/// validate the wiring without pulling the export catalog into scope.
pub fn collect_scope_set(scopes: &BTreeSet<String>) -> Vec<String> {
    scopes.iter().cloned().collect()
}
