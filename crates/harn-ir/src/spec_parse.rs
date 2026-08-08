//! Parsing `@invariant(...)` attributes into instantiated checks.
//!
//! Turns the attribute arguments into an `InvariantSpec`, normalizes the
//! invariant name, and instantiates the matching `Invariant` — including the
//! capability-policy form, whose capability sets and config lists are parsed
//! here. Malformed config produces a diagnostic rather than a panic.

use harn_lexer::Span;
use harn_parser::{Attribute, AttributeArg, Node};
use std::collections::{BTreeMap, BTreeSet};

use crate::invariants::*;
use crate::types::*;
pub(crate) fn parse_invariant_specs(
    attributes: &[Attribute],
    handler_name: &str,
    handler_kind: HandlerKind,
) -> (Vec<InvariantSpec>, Vec<InvariantDiagnostic>) {
    let mut specs = Vec::new();
    let mut diagnostics = Vec::new();

    for attribute in attributes {
        if attribute.name != "invariant" {
            continue;
        }
        if !matches!(
            handler_kind,
            HandlerKind::Function | HandlerKind::Tool | HandlerKind::Pipeline
        ) {
            diagnostics.push(InvariantDiagnostic {
                invariant: "invariant".to_string(),
                handler: handler_name.to_string(),
                message: "`@invariant` only applies to function, tool, or pipeline declarations"
                    .to_string(),
                span: attribute.span,
                help: None,
                path: Vec::new(),
            });
            continue;
        }

        match parse_invariant_spec(attribute) {
            Ok(spec) => specs.push(spec),
            Err(mut diag) => {
                diag.handler = handler_name.to_string();
                diagnostics.push(*diag);
            }
        }
    }

    (specs, diagnostics)
}

fn parse_invariant_spec(attribute: &Attribute) -> Result<InvariantSpec, Box<InvariantDiagnostic>> {
    let mut named = BTreeMap::new();
    let mut positionals = Vec::new();

    for arg in &attribute.args {
        let Some(value) = attribute_arg_string(arg) else {
            return Err(Box::new(InvariantDiagnostic {
                invariant: "invariant".to_string(),
                handler: String::new(),
                message: "`@invariant(...)` arguments must be strings, identifiers, numbers, bools, or nil".to_string(),
                span: arg.span,
                help: Some("use strings for invariant names and configuration values".to_string()),
                path: Vec::new(),
            }));
        };
        if let Some(name) = &arg.name {
            named.insert(name.clone(), value);
        } else {
            positionals.push(value);
        }
    }

    let raw_name = named
        .remove("name")
        .or_else(|| positionals.first().cloned())
        .ok_or_else(|| Box::new(InvariantDiagnostic {
            invariant: "invariant".to_string(),
            handler: String::new(),
            message: "`@invariant(...)` requires an invariant name as the first positional argument or `name:`".to_string(),
            span: attribute.span,
            help: Some(
                "for example: `@invariant(\"fs.writes\", \"src/**\")`".to_string(),
            ),
            path: Vec::new(),
        }))?;
    let name = normalize_invariant_name(&raw_name).ok_or_else(|| {
        Box::new(InvariantDiagnostic {
            invariant: raw_name.clone(),
            handler: String::new(),
            message: format!("unknown invariant `{raw_name}`"),
            span: attribute.span,
            help: Some(
                "known invariants are `fs.writes`, `budget.remaining`, `approval.reachability`, and `capability.policy`"
                    .to_string(),
            ),
            path: Vec::new(),
        })
    })?;

    let remaining_positionals = if named.contains_key("name") {
        positionals
    } else {
        positionals.into_iter().skip(1).collect()
    };

    Ok(InvariantSpec {
        name,
        span: attribute.span,
        params: named,
        positionals: remaining_positionals,
    })
}

fn attribute_arg_string(arg: &AttributeArg) -> Option<String> {
    match &arg.value.node {
        Node::StringLiteral(value) | Node::RawStringLiteral(value) | Node::Identifier(value) => {
            Some(value.clone())
        }
        Node::IntLiteral(value) => Some(value.to_string()),
        Node::FloatLiteral(value) => Some(value.to_string()),
        Node::BoolLiteral(value) => Some(value.to_string()),
        Node::NilLiteral => Some("nil".to_string()),
        _ => None,
    }
}

pub(crate) fn normalize_invariant_name(name: &str) -> Option<String> {
    match name {
        "fs.writes" | "fs_writes" | "writes" => Some("fs.writes".to_string()),
        "budget.remaining" | "budget_remaining" | "budget" => Some("budget.remaining".to_string()),
        "approval.reachability" | "approval_reachability" | "approval" => {
            Some("approval.reachability".to_string())
        }
        "capability.policy" | "capability_policy" | "capabilities" | "policy.capabilities" => {
            Some("capability.policy".to_string())
        }
        _ => None,
    }
}

pub(crate) fn instantiate_invariant(
    spec: &InvariantSpec,
) -> Result<Box<dyn Invariant>, ConfigDiagnosticBuilder> {
    match spec.name.as_str() {
        "fs.writes" => {
            let mut globs = spec.positionals.clone();
            if let Some(glob) = spec
                .params
                .get("path_glob")
                .or_else(|| spec.params.get("glob"))
                .or_else(|| spec.params.get("allow"))
            {
                globs.push(glob.clone());
            }
            if globs.is_empty() {
                return Err(ConfigDiagnosticBuilder::new(
                    "fs.writes",
                    spec.span,
                    "`fs.writes` requires at least one allowed path glob".to_string(),
                    Some("for example: `@invariant(\"fs.writes\", \"src/**\")`".to_string()),
                ));
            }
            Ok(Box::new(FsWritesSubsetPathGlob { globs }))
        }
        "budget.remaining" => {
            let target = spec
                .params
                .get("target")
                .cloned()
                .or_else(|| spec.positionals.first().cloned())
                .unwrap_or_else(|| "budget.remaining".to_string());
            Ok(Box::new(BudgetRemainingNonIncreasing { target }))
        }
        "approval.reachability" => Ok(Box::new(ApprovalReachability)),
        "capability.policy" => instantiate_capability_policy_invariant(spec),
        other => Err(ConfigDiagnosticBuilder::new(
            other,
            spec.span,
            format!("unknown invariant `{other}`"),
            None,
        )),
    }
}

fn instantiate_capability_policy_invariant(
    spec: &InvariantSpec,
) -> Result<Box<dyn Invariant>, ConfigDiagnosticBuilder> {
    let allow_raw = spec
        .params
        .get("allow")
        .or_else(|| spec.params.get("capabilities"))
        .or_else(|| spec.params.get("allow_capabilities"))
        .or_else(|| spec.positionals.first())
        .ok_or_else(|| {
            ConfigDiagnosticBuilder::new(
                "capability.policy",
                spec.span,
                "`capability.policy` requires an `allow:` capability list".to_string(),
                Some(
                    "for example: `@invariant(\"capability.policy\", allow: \"fs.write,llm.model\")`"
                        .to_string(),
                ),
            )
        })?;
    let allowed = parse_capability_set(allow_raw).map_err(|message| {
        ConfigDiagnosticBuilder::new("capability.policy", spec.span, message, capability_help())
    })?;
    if allowed.is_empty() {
        return Err(ConfigDiagnosticBuilder::new(
            "capability.policy",
            spec.span,
            "`capability.policy` allow list must contain at least one capability".to_string(),
            capability_help(),
        ));
    }

    let workspace_globs = collect_named_values(
        spec,
        &[
            "workspace",
            "workspace_glob",
            "path_glob",
            "glob",
            "allow_workspace",
        ],
    );

    Ok(Box::new(CapabilityPolicyInvariant {
        allowed,
        workspace_globs,
        require_approval: parse_optional_capability_set(spec, &["require_approval"])?,
        require_budget: parse_optional_capability_set(spec, &["require_budget", "budget"])?,
        require_autonomy: parse_optional_capability_set(spec, &["require_autonomy"])?,
        require_execution_policy: parse_optional_capability_set(
            spec,
            &["require_execution_policy", "require_sandbox"],
        )?,
        require_command_policy: parse_optional_capability_set(spec, &["require_command_policy"])?,
        require_egress_policy: parse_optional_capability_set(spec, &["require_egress_policy"])?,
        require_approval_policy: parse_optional_capability_set(spec, &["require_approval_policy"])?,
    }))
}

fn parse_optional_capability_set(
    spec: &InvariantSpec,
    keys: &[&str],
) -> Result<BTreeSet<Capability>, ConfigDiagnosticBuilder> {
    let Some(raw) = keys.iter().find_map(|key| spec.params.get(*key)) else {
        return Ok(BTreeSet::new());
    };
    parse_capability_set(raw).map_err(|message| {
        ConfigDiagnosticBuilder::new("capability.policy", spec.span, message, capability_help())
    })
}

fn collect_named_values(spec: &InvariantSpec, keys: &[&str]) -> Vec<String> {
    keys.iter()
        .filter_map(|key| spec.params.get(*key).cloned())
        .flat_map(|value| split_config_list(&value))
        .collect()
}

fn parse_capability_set(raw: &str) -> Result<BTreeSet<Capability>, String> {
    let mut capabilities = BTreeSet::new();
    for item in split_config_list(raw) {
        let Some(capability) = Capability::from_policy_name(&item) else {
            return Err(format!(
                "unknown capability `{item}` in `capability.policy`"
            ));
        };
        capabilities.insert(capability);
    }
    Ok(capabilities)
}

fn split_config_list(raw: &str) -> Vec<String> {
    raw.split(|ch: char| ch == ',' || ch == ';' || ch.is_whitespace())
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

fn capability_help() -> Option<String> {
    Some(
        "known capabilities are `fs.write`, `process.exec`, `network.access`, `mcp.connector`, `llm.model`, `worker.dispatch`, `human.approval`, and `autonomy.policy`"
            .to_string(),
    )
}

#[derive(Debug, Clone)]
pub(crate) struct ConfigDiagnosticBuilder {
    pub(crate) invariant: String,
    pub(crate) span: Span,
    pub(crate) message: String,
    pub(crate) help: Option<String>,
}

impl ConfigDiagnosticBuilder {
    pub(crate) fn new(
        invariant: impl Into<String>,
        span: Span,
        message: String,
        help: Option<String>,
    ) -> Self {
        Self {
            invariant: invariant.into(),
            span,
            message,
            help,
        }
    }

    pub(crate) fn with_handler(self, handler: &str) -> InvariantDiagnostic {
        InvariantDiagnostic {
            invariant: self.invariant,
            handler: handler.to_string(),
            message: self.message,
            span: self.span,
            help: self.help,
            path: Vec::new(),
        }
    }
}
