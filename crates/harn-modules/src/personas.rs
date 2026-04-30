use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaManifestDocument {
    #[serde(default)]
    pub personas: Vec<PersonaManifestEntry>,
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
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize)]
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
                "metadata_refresh_hashes",
                "metadata_save",
                "metadata_set",
                "metadata_stale",
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
mod tests {
    use super::*;

    fn context(names: &[&str]) -> PersonaValidationContext {
        PersonaValidationContext {
            known_capabilities: default_persona_capabilities(),
            known_tools: BTreeSet::from(["github".to_string(), "ci".to_string()]),
            known_names: names.iter().map(|name| name.to_string()).collect(),
        }
    }

    #[test]
    fn validates_sample_manifest() {
        let parsed = parse_persona_manifest_str(
            r#"
[[personas]]
name = "merge_captain"
description = "Owns PR readiness."
entry_workflow = "workflows/merge_captain.harn#run"
tools = ["github", "ci"]
capabilities = ["git.get_diff"]
autonomy = "act_with_approval"
receipts = "required"
triggers = ["github.pr_opened"]
schedules = ["*/30 * * * *"]
handoffs = ["review_captain"]
context_packs = ["repo_policy"]
evals = ["merge_safety"]
budget = { daily_usd = 20.0 }

[[personas]]
name = "review_captain"
description = "Reviews code."
entry_workflow = "workflows/review_captain.harn#run"
tools = ["github"]
autonomy_tier = "suggest"
receipt_policy = "optional"
"#,
        )
        .expect("manifest parses");

        validate_persona_manifests(
            Path::new("harn.toml"),
            &parsed.personas,
            &context(&["merge_captain", "review_captain"]),
        )
        .expect("manifest validates");
    }

    #[test]
    fn bad_manifest_produces_typed_errors() {
        let parsed = parse_persona_manifest_str(
            r#"
[[personas]]
name = "bad"
description = ""
entry_workflow = ""
tools = ["unknown"]
capabilities = ["git"]
autonomy = "shadow"
receipts = "required"
triggers = ["github"]
schedules = [""]
handoffs = ["missing"]
budget = { daily_usd = -1.0, surprise = true }
surprise = true
"#,
        )
        .expect("manifest parses");

        let errors = validate_persona_manifests(
            Path::new("harn.toml"),
            &parsed.personas,
            &context(&["bad"]),
        )
        .expect_err("manifest rejects");
        let fields: BTreeSet<_> = errors
            .iter()
            .map(|error| error.field_path.as_str())
            .collect();
        assert!(fields.contains("[[personas]][0].description"));
        assert!(fields.contains("[[personas]][0].entry_workflow"));
        assert!(fields.contains("[[personas]][0].tools"));
        assert!(fields.contains("[[personas]][0].capabilities"));
        assert!(fields.contains("[[personas]][0].triggers"));
        assert!(fields.contains("[[personas]][0].schedules"));
        assert!(fields.contains("[[personas]][0].handoffs"));
        assert!(fields.contains("[[personas]][0].budget.daily_usd"));
        assert!(fields.contains("[[personas]][0].budget.surprise"));
        assert!(fields.contains("[[personas]][0].surprise"));
    }
}
