use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use harn_parser::{Attribute, DictEntry, Node, SNode};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaManifestDocument {
    #[serde(default)]
    pub personas: Vec<PersonaManifestEntry>,
}

/// A persona's declared output style — how it should shape its prose (tone,
/// verbosity, formatting). Accepts either a bare string (a named style) or a
/// table with `name` and/or `instructions`. This is the persona-manifest field
/// behind Burin's output-style surface; Harn owns the declaration + accessor,
/// Burin owns any editor/workbench UI over it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PersonaOutputStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

impl PersonaOutputStyle {
    /// Build a style from a bare name (the `output_style = "concise"` form).
    pub fn from_name(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            instructions: None,
        }
    }

    /// True when the style carries no name and no instructions.
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.instructions.is_none()
    }
}

impl<'de> Deserialize<'de> for PersonaOutputStyle {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Accept `output_style = "concise"` (a named style) or a table with
        // `name` / `instructions`. Unknown table keys are rejected so a typo
        // surfaces rather than being silently dropped.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Name(String),
            Table {
                #[serde(default)]
                name: Option<String>,
                #[serde(default)]
                instructions: Option<String>,
            },
        }
        Ok(match Repr::deserialize(deserializer)? {
            Repr::Name(name) => PersonaOutputStyle::from_name(name),
            Repr::Table { name, instructions } => PersonaOutputStyle { name, instructions },
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaManifestEntry {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, alias = "entry", alias = "entry_pipeline")]
    pub entry_workflow: Option<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default, alias = "tier", alias = "autonomy")]
    pub autonomy_tier: Option<PersonaAutonomyTier>,
    #[serde(default, alias = "receipts")]
    pub receipt_policy: Option<PersonaReceiptPolicy>,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub schedules: Vec<String>,
    #[serde(default)]
    pub model_policy: PersonaModelPolicy,
    #[serde(default)]
    pub budget: PersonaBudget,
    #[serde(default)]
    pub handoffs: Vec<String>,
    #[serde(default)]
    pub context_packs: Vec<String>,
    #[serde(default, alias = "eval_packs")]
    pub evals: Vec<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub package_source: PersonaPackageSource,
    #[serde(default)]
    pub rollout_policy: PersonaRolloutPolicy,
    #[serde(default)]
    pub steps: Vec<PersonaStepMetadata>,
    /// Per-stage tool-surface narrowing. Each stage names a `@step` and
    /// declares the tools / side-effect ceiling enforced while that step
    /// runs.
    #[serde(default)]
    pub stages: Vec<PersonaStageDecl>,
    /// How this persona should shape its output prose. A bare string names a
    /// style; a table carries `name` and/or inline `instructions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_style: Option<PersonaOutputStyle>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, toml::Value>,
}

/// Stage declaration carried on a `PersonaManifestEntry`.
///
/// Mirrors the runtime `harn_vm::StageDecl` shape so loaders can map
/// directly. `allowed_tools = None` means "inherit the persona-level
/// tool list"; `Some(vec![])` means "deny every tool while this stage is
/// active".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaStageDecl {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_effect_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_exit: Option<PersonaStageExit>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaStageExit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_complete: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_failure: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaStepMetadata {
    pub name: String,
    pub function: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_boundary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<PersonaStepRetry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<PersonaStepBudget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaStepRetry {
    pub max_attempts: u64,
}

/// Per-step token / cost ceiling. Either field is optional; whichever is
/// set governs that dimension. Surfaced statically by `harn persona
/// inspect --json` and consumed at runtime by `crates/harn-vm/src/step_runtime.rs`
/// to short-circuit `llm_call` invocations before they exceed the limit.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaStepBudget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_usd: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonaAutonomyTier {
    Shadow,
    Suggest,
    ActWithApproval,
    ActAuto,
}

impl PersonaAutonomyTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shadow => "shadow",
            Self::Suggest => "suggest",
            Self::ActWithApproval => "act_with_approval",
            Self::ActAuto => "act_auto",
        }
    }
}

impl FromStr for PersonaAutonomyTier {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "shadow" => Ok(Self::Shadow),
            "suggest" => Ok(Self::Suggest),
            "act_with_approval" => Ok(Self::ActWithApproval),
            "act_auto" => Ok(Self::ActAuto),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonaReceiptPolicy {
    #[default]
    Optional,
    Required,
    Disabled,
}

impl PersonaReceiptPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Optional => "optional",
            Self::Required => "required",
            Self::Disabled => "disabled",
        }
    }
}

impl FromStr for PersonaReceiptPolicy {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "optional" => Ok(Self::Optional),
            "required" => Ok(Self::Required),
            "disabled" => Ok(Self::Disabled),
            "none" => Ok(Self::Disabled),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaModelPolicy {
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub escalation_model: Option<String>,
    #[serde(default)]
    pub fallback_models: Vec<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaBudget {
    #[serde(default)]
    pub daily_usd: Option<f64>,
    #[serde(default)]
    pub hourly_usd: Option<f64>,
    #[serde(default)]
    pub run_usd: Option<f64>,
    #[serde(default)]
    pub frontier_escalations: Option<u32>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub max_runtime_seconds: Option<u64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaPackageSource {
    #[serde(default)]
    pub package: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub git: Option<String>,
    #[serde(default)]
    pub rev: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaRolloutPolicy {
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub percentage: Option<u8>,
    #[serde(default)]
    pub cohorts: Vec<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResolvedPersonaManifest {
    pub manifest_path: PathBuf,
    pub manifest_dir: PathBuf,
    pub personas: Vec<PersonaManifestEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonaValidationError {
    pub manifest_path: PathBuf,
    pub field_path: String,
    pub message: String,
}

impl std::fmt::Display for PersonaValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}: {}",
            self.manifest_path.display(),
            self.field_path,
            self.message
        )
    }
}

impl std::error::Error for PersonaValidationError {}

#[derive(Debug, Clone, Default)]
pub struct PersonaValidationContext {
    pub known_capabilities: BTreeSet<String>,
    pub known_tools: BTreeSet<String>,
    pub known_names: BTreeSet<String>,
}

pub fn parse_persona_manifest_str(
    source: &str,
) -> Result<PersonaManifestDocument, toml::de::Error> {
    let document = toml::from_str::<PersonaManifestDocument>(source)?;
    if !document.personas.is_empty() {
        return Ok(document);
    }
    let entry = toml::from_str::<PersonaManifestEntry>(source)?;
    if entry.name.is_some()
        || entry.description.is_some()
        || entry.entry_workflow.is_some()
        || !entry.tools.is_empty()
        || !entry.capabilities.is_empty()
    {
        Ok(PersonaManifestDocument {
            personas: vec![entry],
        })
    } else {
        Ok(document)
    }
}

pub fn parse_persona_manifest_file(path: &Path) -> Result<PersonaManifestDocument, String> {
    let content = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    parse_persona_manifest_str(&content)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

pub fn parse_persona_source_file(path: &Path) -> Result<PersonaManifestDocument, String> {
    let content = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    parse_persona_source_str(&content)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

pub fn parse_persona_source_str(source: &str) -> Result<PersonaManifestDocument, String> {
    let program = harn_parser::parse_source(source).map_err(|error| error.to_string())?;
    Ok(extract_personas_from_program(&program))
}

pub fn extract_personas_from_program(program: &[SNode]) -> PersonaManifestDocument {
    let step_decls = collect_step_declarations(program);
    let mut personas = Vec::new();
    for snode in program {
        let Node::AttributedDecl { attributes, inner } = &snode.node else {
            continue;
        };
        let Some(persona_attr) = attributes.iter().find(|attr| attr.name == "persona") else {
            continue;
        };
        let Node::FnDecl { name, body, .. } = &inner.node else {
            continue;
        };
        let persona_name = attr_string(persona_attr, "name").unwrap_or_else(|| name.clone());
        let mut seen = BTreeSet::new();
        let mut steps = Vec::new();
        for call_name in collect_called_functions(body) {
            if !seen.insert(call_name.clone()) {
                continue;
            }
            if let Some(step) = step_decls.get(&call_name) {
                steps.push(step.clone());
            }
        }
        personas.push(PersonaManifestEntry {
            name: Some(persona_name),
            description: Some(
                attr_string(persona_attr, "description")
                    .unwrap_or_else(|| "Source-declared persona".to_string()),
            ),
            entry_workflow: Some(name.clone()),
            tools: attr_string_list(persona_attr, "tools"),
            capabilities: {
                let capabilities = attr_string_list(persona_attr, "capabilities");
                if capabilities.is_empty() {
                    vec!["project.test_commands".to_string()]
                } else {
                    capabilities
                }
            },
            autonomy_tier: attr_string(persona_attr, "autonomy")
                .as_deref()
                .and_then(|value| PersonaAutonomyTier::from_str(value).ok())
                .or(Some(PersonaAutonomyTier::Suggest)),
            receipt_policy: attr_string(persona_attr, "receipts")
                .as_deref()
                .and_then(|value| PersonaReceiptPolicy::from_str(value).ok())
                .or(Some(PersonaReceiptPolicy::Optional)),
            steps,
            stages: attr_stage_list(persona_attr),
            output_style: attr_string(persona_attr, "output_style")
                .map(PersonaOutputStyle::from_name),
            ..PersonaManifestEntry::default()
        });
    }
    PersonaManifestDocument { personas }
}

pub fn extract_step_metadata_from_program(program: &[SNode]) -> Vec<PersonaStepMetadata> {
    collect_step_declarations(program).into_values().collect()
}

fn collect_step_declarations(program: &[SNode]) -> BTreeMap<String, PersonaStepMetadata> {
    let mut steps = BTreeMap::new();
    for snode in program {
        let Node::AttributedDecl { attributes, inner } = &snode.node else {
            continue;
        };
        let Some(step_attr) = attributes.iter().find(|attr| attr.name == "step") else {
            continue;
        };
        let Node::FnDecl { name, .. } = &inner.node else {
            continue;
        };
        steps.insert(
            name.clone(),
            PersonaStepMetadata {
                name: attr_string(step_attr, "name").unwrap_or_else(|| name.clone()),
                function: name.clone(),
                model: attr_string(step_attr, "model"),
                approval: attr_string(step_attr, "approval"),
                receipt: attr_string(step_attr, "receipt"),
                error_boundary: attr_string(step_attr, "error_boundary"),
                retry: attr_retry(step_attr),
                budget: attr_step_budget(step_attr),
                line: Some(inner.span.line),
            },
        );
    }
    steps
}

fn attr_string(attr: &Attribute, key: &str) -> Option<String> {
    attr.named_arg(key).and_then(node_string)
}

fn attr_string_list(attr: &Attribute, key: &str) -> Vec<String> {
    let Some(value) = attr.named_arg(key) else {
        return Vec::new();
    };
    let Node::ListLiteral(items) = &value.node else {
        return Vec::new();
    };
    items.iter().filter_map(node_string).collect()
}

fn node_string(node: &SNode) -> Option<String> {
    match &node.node {
        Node::StringLiteral(value) | Node::RawStringLiteral(value) | Node::Identifier(value) => {
            Some(value.clone())
        }
        _ => None,
    }
}

fn attr_stage_list(attr: &Attribute) -> Vec<PersonaStageDecl> {
    let Some(value) = attr.named_arg("stages") else {
        return Vec::new();
    };
    let Node::ListLiteral(entries) = &value.node else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let Node::DictLiteral(fields) = &entry.node else {
            continue;
        };
        let mut stage = PersonaStageDecl::default();
        for dict_entry in fields {
            let Some(key) = entry_key(&dict_entry.key) else {
                continue;
            };
            match key {
                "name" => {
                    if let Some(name) = node_string(&dict_entry.value) {
                        stage.name = name;
                    }
                }
                "allowed_tools" => {
                    if let Node::ListLiteral(items) = &dict_entry.value.node {
                        let tools: Vec<String> = items.iter().filter_map(node_string).collect();
                        stage.allowed_tools = Some(tools);
                    }
                }
                "side_effect_level" => {
                    stage.side_effect_level = node_string(&dict_entry.value);
                }
                "max_iterations" => {
                    if let Node::IntLiteral(n) = dict_entry.value.node {
                        if n >= 0 {
                            stage.max_iterations = Some(n as u32);
                        }
                    }
                }
                _ => {}
            }
        }
        if !stage.name.is_empty() {
            out.push(stage);
        }
    }
    out
}

fn attr_retry(attr: &Attribute) -> Option<PersonaStepRetry> {
    let retry = attr.named_arg("retry")?;
    let Node::DictLiteral(entries) = &retry.node else {
        return None;
    };
    for entry in entries {
        if entry_key(&entry.key) == Some("max_attempts") {
            if let Node::IntLiteral(value) = entry.value.node {
                if value >= 1 {
                    return Some(PersonaStepRetry {
                        max_attempts: value as u64,
                    });
                }
            }
        }
    }
    None
}

fn attr_step_budget(attr: &Attribute) -> Option<PersonaStepBudget> {
    let budget = attr.named_arg("budget")?;
    let Node::DictLiteral(entries) = &budget.node else {
        return None;
    };
    let mut out = PersonaStepBudget::default();
    let mut any = false;
    for entry in entries {
        match entry_key(&entry.key) {
            Some("max_tokens") => {
                if let Node::IntLiteral(value) = entry.value.node {
                    if value >= 1 {
                        out.max_tokens = Some(value as u64);
                        any = true;
                    }
                }
            }
            Some("max_usd") => match entry.value.node {
                Node::FloatLiteral(value) if value.is_finite() && value >= 0.0 => {
                    out.max_usd = Some(value);
                    any = true;
                }
                Node::IntLiteral(value) if value >= 0 => {
                    out.max_usd = Some(value as f64);
                    any = true;
                }
                _ => {}
            },
            _ => {}
        }
    }
    any.then_some(out)
}

fn entry_key(node: &SNode) -> Option<&str> {
    match &node.node {
        Node::Identifier(value) | Node::StringLiteral(value) | Node::RawStringLiteral(value) => {
            Some(value.as_str())
        }
        _ => None,
    }
}

fn collect_called_functions(body: &[SNode]) -> Vec<String> {
    let mut calls = Vec::new();
    for node in body {
        collect_called_functions_node(node, &mut calls);
    }
    calls
}

fn collect_called_functions_node(node: &SNode, calls: &mut Vec<String>) {
    match &node.node {
        Node::FunctionCall { name, args, .. } => {
            calls.push(name.clone());
            collect_many(args, calls);
        }
        Node::LetBinding { value, .. }
        | Node::ConstBinding { value, .. }
        | Node::ReturnStmt { value: Some(value) }
        | Node::YieldExpr { value: Some(value) }
        | Node::EmitExpr { value }
        | Node::ThrowStmt { value }
        | Node::Spread(value)
        | Node::TryOperator { operand: value }
        | Node::TryStar { operand: value }
        | Node::UnaryOp { operand: value, .. } => collect_called_functions_node(value, calls),
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            collect_called_functions_node(condition, calls);
            collect_many(then_body, calls);
            if let Some(else_body) = else_body {
                collect_many(else_body, calls);
            }
        }
        Node::ForIn { iterable, body, .. } => {
            collect_called_functions_node(iterable, calls);
            collect_many(body, calls);
        }
        Node::MatchExpr { value, arms } => {
            collect_called_functions_node(value, calls);
            for arm in arms {
                collect_called_functions_node(&arm.pattern, calls);
                if let Some(guard) = &arm.guard {
                    collect_called_functions_node(guard, calls);
                }
                collect_many(&arm.body, calls);
            }
        }
        Node::WhileLoop { condition, body } => {
            collect_called_functions_node(condition, calls);
            collect_many(body, calls);
        }
        Node::Retry { count, body } => {
            collect_called_functions_node(count, calls);
            collect_many(body, calls);
        }
        Node::CostRoute { options, body } => {
            for (_, value) in options {
                collect_called_functions_node(value, calls);
            }
            collect_many(body, calls);
        }
        Node::TryCatch {
            has_catch: _,
            body,
            catch_body,
            finally_body,
            ..
        } => {
            collect_many(body, calls);
            collect_many(catch_body, calls);
            if let Some(finally_body) = finally_body {
                collect_many(finally_body, calls);
            }
        }
        Node::TryExpr { body }
        | Node::SpawnExpr { body }
        | Node::DeferStmt { body }
        | Node::MutexBlock { body, .. }
        | Node::Block(body)
        | Node::Closure { body, .. } => collect_many(body, calls),
        Node::DeadlineBlock { duration, body } => {
            collect_called_functions_node(duration, calls);
            collect_many(body, calls);
        }
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            collect_called_functions_node(condition, calls);
            collect_many(else_body, calls);
        }
        Node::RequireStmt { condition, message } => {
            collect_called_functions_node(condition, calls);
            if let Some(message) = message {
                collect_called_functions_node(message, calls);
            }
        }
        Node::Parallel {
            expr,
            body,
            options,
            ..
        } => {
            collect_called_functions_node(expr, calls);
            for (_, value) in options {
                collect_called_functions_node(value, calls);
            }
            collect_many(body, calls);
        }
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            for case in cases {
                collect_called_functions_node(&case.channel, calls);
                collect_many(&case.body, calls);
            }
            if let Some((duration, body)) = timeout {
                collect_called_functions_node(duration, calls);
                collect_many(body, calls);
            }
            if let Some(body) = default_body {
                collect_many(body, calls);
            }
        }
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            collect_called_functions_node(object, calls);
            collect_many(args, calls);
        }
        Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
            collect_called_functions_node(object, calls);
        }
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => {
            collect_called_functions_node(object, calls);
            collect_called_functions_node(index, calls);
        }
        Node::SliceAccess { object, start, end } => {
            collect_called_functions_node(object, calls);
            if let Some(start) = start {
                collect_called_functions_node(start, calls);
            }
            if let Some(end) = end {
                collect_called_functions_node(end, calls);
            }
        }
        Node::BinaryOp { left, right, .. } => {
            collect_called_functions_node(left, calls);
            collect_called_functions_node(right, calls);
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            collect_called_functions_node(condition, calls);
            collect_called_functions_node(true_expr, calls);
            collect_called_functions_node(false_expr, calls);
        }
        Node::Assignment { target, value, .. } => {
            collect_called_functions_node(target, calls);
            collect_called_functions_node(value, calls);
        }
        Node::EnumConstruct { args, .. } => collect_many(args, calls),
        Node::StructConstruct { fields, .. } | Node::DictLiteral(fields) => {
            collect_dict_calls(fields, calls);
        }
        Node::ListLiteral(items) | Node::OrPattern(items) => collect_many(items, calls),
        Node::HitlExpr { args, .. } => {
            for arg in args {
                collect_called_functions_node(&arg.value, calls);
            }
        }
        Node::AttributedDecl { inner, .. } => collect_called_functions_node(inner, calls),
        Node::Pipeline { body, .. }
        | Node::OverrideDecl { body, .. }
        | Node::FnDecl { body, .. }
        | Node::ToolDecl { body, .. } => collect_many(body, calls),
        Node::SkillDecl { fields, .. } | Node::EvalPackDecl { fields, .. } => {
            for (_, value) in fields {
                collect_called_functions_node(value, calls);
            }
        }
        _ => {}
    }
}

fn collect_many(nodes: &[SNode], calls: &mut Vec<String>) {
    for node in nodes {
        collect_called_functions_node(node, calls);
    }
}

fn collect_dict_calls(entries: &[DictEntry], calls: &mut Vec<String>) {
    for entry in entries {
        collect_called_functions_node(&entry.key, calls);
        collect_called_functions_node(&entry.value, calls);
    }
}

pub fn validate_persona_manifests(
    manifest_path: &Path,
    personas: &[PersonaManifestEntry],
    context: &PersonaValidationContext,
) -> Result<(), Vec<PersonaValidationError>> {
    let mut errors = Vec::new();
    for (index, persona) in personas.iter().enumerate() {
        validate_persona(persona, index, manifest_path, context, &mut errors);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

pub fn validate_persona(
    persona: &PersonaManifestEntry,
    index: usize,
    manifest_path: &Path,
    context: &PersonaValidationContext,
    errors: &mut Vec<PersonaValidationError>,
) {
    let root = format!("[[personas]][{index}]");
    for field in persona.extra.keys() {
        persona_error(
            manifest_path,
            format!("{root}.{field}"),
            "unknown persona field",
            errors,
        );
    }
    let name = validate_required_string(
        manifest_path,
        &root,
        "name",
        persona.name.as_deref(),
        errors,
    );
    if let Some(name) = name {
        validate_tokenish(manifest_path, &root, "name", name, errors);
    }
    validate_required_string(
        manifest_path,
        &root,
        "description",
        persona.description.as_deref(),
        errors,
    );
    validate_required_string(
        manifest_path,
        &root,
        "entry_workflow",
        persona.entry_workflow.as_deref(),
        errors,
    );
    if persona.tools.is_empty() && persona.capabilities.is_empty() {
        persona_error(
            manifest_path,
            format!("{root}.tools"),
            "persona requires at least one tool or capability",
            errors,
        );
    }
    if persona.autonomy_tier.is_none() {
        persona_error(
            manifest_path,
            format!("{root}.autonomy_tier"),
            "missing required autonomy tier",
            errors,
        );
    }
    if persona.receipt_policy.is_none() {
        persona_error(
            manifest_path,
            format!("{root}.receipt_policy"),
            "missing required receipt policy",
            errors,
        );
    }
    validate_string_list(manifest_path, &root, "tools", &persona.tools, errors);
    for tool in &persona.tools {
        if !context.known_tools.is_empty() && !context.known_tools.contains(tool) {
            persona_error(
                manifest_path,
                format!("{root}.tools"),
                format!("unknown tool '{tool}'"),
                errors,
            );
        }
    }
    for capability in &persona.capabilities {
        let Some((cap, op)) = capability.split_once('.') else {
            persona_error(
                manifest_path,
                format!("{root}.capabilities"),
                format!("capability '{capability}' must use capability.operation syntax"),
                errors,
            );
            continue;
        };
        if cap.trim().is_empty() || op.trim().is_empty() {
            persona_error(
                manifest_path,
                format!("{root}.capabilities"),
                format!("capability '{capability}' must use capability.operation syntax"),
                errors,
            );
        } else if !context.known_capabilities.is_empty()
            && !context.known_capabilities.contains(capability)
        {
            persona_error(
                manifest_path,
                format!("{root}.capabilities"),
                format!("unknown capability '{capability}'"),
                errors,
            );
        }
    }
    validate_string_list(
        manifest_path,
        &root,
        "context_packs",
        &persona.context_packs,
        errors,
    );
    validate_string_list(manifest_path, &root, "evals", &persona.evals, errors);
    for schedule in &persona.schedules {
        if schedule.trim().is_empty() {
            persona_error(
                manifest_path,
                format!("{root}.schedules"),
                "schedule entries must not be empty",
                errors,
            );
        } else if let Err(error) = croner::Cron::from_str(schedule) {
            persona_error(
                manifest_path,
                format!("{root}.schedules"),
                format!("invalid cron schedule '{schedule}': {error}"),
                errors,
            );
        }
    }
    for trigger in &persona.triggers {
        match trigger.split_once('.') {
            Some((provider, event)) if !provider.trim().is_empty() && !event.trim().is_empty() => {}
            _ => persona_error(
                manifest_path,
                format!("{root}.triggers"),
                format!("trigger '{trigger}' must use provider.event syntax"),
                errors,
            ),
        }
    }
    for handoff in &persona.handoffs {
        if !context.known_names.contains(handoff) {
            persona_error(
                manifest_path,
                format!("{root}.handoffs"),
                format!("unknown handoff target '{handoff}'"),
                errors,
            );
        }
    }
    validate_persona_budget(manifest_path, &root, &persona.budget, errors);
    validate_persona_stages(manifest_path, &root, persona, context, errors);
    validate_persona_nested_extra(
        manifest_path,
        &root,
        "model_policy",
        &persona.model_policy.extra,
        errors,
    );
    validate_persona_nested_extra(
        manifest_path,
        &root,
        "package_source",
        &persona.package_source.extra,
        errors,
    );
    validate_persona_nested_extra(
        manifest_path,
        &root,
        "rollout_policy",
        &persona.rollout_policy.extra,
        errors,
    );
    if let Some(percentage) = persona.rollout_policy.percentage {
        if percentage > 100 {
            persona_error(
                manifest_path,
                format!("{root}.rollout_policy.percentage"),
                "rollout percentage must be between 0 and 100",
                errors,
            );
        }
    }
}

pub fn validate_required_string<'a>(
    manifest_path: &Path,
    root: &str,
    field: &str,
    value: Option<&'a str>,
    errors: &mut Vec<PersonaValidationError>,
) -> Option<&'a str> {
    match value.map(str::trim) {
        Some(value) if !value.is_empty() => Some(value),
        _ => {
            persona_error(
                manifest_path,
                format!("{root}.{field}"),
                format!("missing required {field}"),
                errors,
            );
            None
        }
    }
}

pub fn validate_string_list(
    manifest_path: &Path,
    root: &str,
    field: &str,
    values: &[String],
    errors: &mut Vec<PersonaValidationError>,
) {
    for value in values {
        if value.trim().is_empty() {
            persona_error(
                manifest_path,
                format!("{root}.{field}"),
                format!("{field} entries must not be empty"),
                errors,
            );
        } else {
            validate_tokenish(manifest_path, root, field, value, errors);
        }
    }
}

pub fn validate_tokenish(
    manifest_path: &Path,
    root: &str,
    field: &str,
    value: &str,
    errors: &mut Vec<PersonaValidationError>,
) {
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/'))
    {
        persona_error(
            manifest_path,
            format!("{root}.{field}"),
            format!("'{value}' must contain only letters, numbers, '.', '-', '_', or '/'"),
            errors,
        );
    }
}

pub fn validate_persona_budget(
    manifest_path: &Path,
    root: &str,
    budget: &PersonaBudget,
    errors: &mut Vec<PersonaValidationError>,
) {
    validate_persona_nested_extra(manifest_path, root, "budget", &budget.extra, errors);
    for (field, value) in [
        ("daily_usd", budget.daily_usd),
        ("hourly_usd", budget.hourly_usd),
        ("run_usd", budget.run_usd),
    ] {
        if value.is_some_and(|number| !number.is_finite() || number < 0.0) {
            persona_error(
                manifest_path,
                format!("{root}.budget.{field}"),
                "budget amounts must be finite non-negative numbers",
                errors,
            );
        }
    }
}

pub fn validate_persona_nested_extra(
    manifest_path: &Path,
    root: &str,
    field: &str,
    extra: &BTreeMap<String, toml::Value>,
    errors: &mut Vec<PersonaValidationError>,
) {
    for key in extra.keys() {
        persona_error(
            manifest_path,
            format!("{root}.{field}.{key}"),
            format!("unknown {field} field"),
            errors,
        );
    }
}

pub fn validate_persona_stages(
    manifest_path: &Path,
    root: &str,
    persona: &PersonaManifestEntry,
    context: &PersonaValidationContext,
    errors: &mut Vec<PersonaValidationError>,
) {
    let stage_names: BTreeSet<&str> = persona
        .stages
        .iter()
        .map(|stage| stage.name.as_str())
        .collect();
    let mut seen = BTreeSet::new();
    for (index, stage) in persona.stages.iter().enumerate() {
        let field = format!("{root}.stages[{index}]");
        if stage.name.trim().is_empty() {
            persona_error(
                manifest_path,
                format!("{field}.name"),
                "stage name must not be empty",
                errors,
            );
        } else {
            validate_tokenish(manifest_path, &field, "name", &stage.name, errors);
            if !seen.insert(stage.name.as_str()) {
                persona_error(
                    manifest_path,
                    format!("{field}.name"),
                    format!("duplicate stage name '{}'", stage.name),
                    errors,
                );
            }
        }
        for key in stage.extra.keys() {
            persona_error(
                manifest_path,
                format!("{field}.{key}"),
                "unknown stage field",
                errors,
            );
        }
        if let Some(tools) = stage.allowed_tools.as_ref() {
            for tool in tools {
                if tool.trim().is_empty() {
                    persona_error(
                        manifest_path,
                        format!("{field}.allowed_tools"),
                        "allowed_tools entries must not be empty",
                        errors,
                    );
                    continue;
                }
                if !context.known_tools.is_empty() && !context.known_tools.contains(tool) {
                    persona_error(
                        manifest_path,
                        format!("{field}.allowed_tools"),
                        format!("unknown tool '{tool}'"),
                        errors,
                    );
                } else if !persona.tools.is_empty() && !persona.tools.contains(tool) {
                    persona_error(
                        manifest_path,
                        format!("{field}.allowed_tools"),
                        format!("tool '{tool}' is not part of the persona-level tools allowlist"),
                        errors,
                    );
                }
            }
        }
        if let Some(level) = stage.side_effect_level.as_deref() {
            match level {
                "none" | "read_only" | "workspace_write" | "process_exec" | "network" => {}
                _ => persona_error(
                    manifest_path,
                    format!("{field}.side_effect_level"),
                    format!(
                        "unknown side_effect_level '{level}' (expected none, read_only, workspace_write, process_exec, or network)"
                    ),
                    errors,
                ),
            }
        }
        if let Some(exit) = stage.on_exit.as_ref() {
            validate_persona_nested_extra(manifest_path, &field, "on_exit", &exit.extra, errors);
            for (key, target) in [
                ("on_complete", exit.on_complete.as_deref()),
                ("on_failure", exit.on_failure.as_deref()),
            ] {
                let Some(target) = target else { continue };
                if !stage_names.contains(target) {
                    persona_error(
                        manifest_path,
                        format!("{field}.on_exit.{key}"),
                        format!("unknown stage '{target}'"),
                        errors,
                    );
                }
            }
        }
    }
}

pub fn persona_error(
    manifest_path: &Path,
    field_path: String,
    message: impl Into<String>,
    errors: &mut Vec<PersonaValidationError>,
) {
    errors.push(PersonaValidationError {
        manifest_path: manifest_path.to_path_buf(),
        field_path,
        message: message.into(),
    });
}

pub fn default_persona_capability_map() -> BTreeMap<&'static str, Vec<&'static str>> {
    BTreeMap::from([
        (
            "workspace",
            vec![
                "read_text",
                "write_text",
                "apply_edit",
                "delete",
                "exists",
                "file_exists",
                "list",
                "project_root",
                "roots",
            ],
        ),
        ("process", vec!["exec"]),
        ("template", vec!["render"]),
        ("interaction", vec!["ask"]),
        (
            "runtime",
            vec![
                "approved_plan",
                "dry_run",
                "pipeline_input",
                "record_run",
                "set_result",
                "task",
            ],
        ),
        (
            "project",
            vec![
                "agent_instructions",
                "code_patterns",
                "compute_content_hash",
                "ide_context",
                "lessons",
                "mcp_config",
                "metadata_get",
                "metadata_inspect",
                "metadata_refresh_hashes",
                "metadata_save",
                "metadata_set",
                "metadata_stale",
                "path_metadata_entries",
                "path_metadata_get",
                "path_metadata_set",
                "scan",
                "scope_test_command",
                "test_commands",
            ],
        ),
        (
            "session",
            vec![
                "active_roots",
                "changed_paths",
                "preread_get",
                "preread_read_many",
            ],
        ),
        (
            "editor",
            vec!["get_active_file", "get_selection", "get_visible_files"],
        ),
        ("diagnostics", vec!["get_causal_traces", "get_errors"]),
        ("git", vec!["get_branch", "get_diff"]),
        ("learning", vec!["get_learned_rules", "report_correction"]),
    ])
}

pub fn default_persona_capabilities() -> BTreeSet<String> {
    let mut capabilities = BTreeSet::new();
    for (capability, operations) in default_persona_capability_map() {
        for operation in operations {
            capabilities.insert(format!("{capability}.{operation}"));
        }
    }
    capabilities
}

#[cfg(test)]
#[path = "personas_tests.rs"]
mod tests;
