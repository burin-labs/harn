//! Capability audit and the display/JSON provider matrix.
//!
//! Owns the [`ProviderCapabilityMatrixRow`] projection used by the CLI matrix
//! surfaces (`matrix_rows`, `push_matrix_rows`, `rule_to_matrix_row`) and the
//! tool-capability coverage audit that flags priced catalog models missing an
//! explicit `native_tools` / `preferred_tool_format` rule
//! (`audit_tool_capability_coverage` and the suggested-default helpers).

use serde::Serialize;

use super::lookup::{builtin, USER_OVERRIDES};
use super::model::CapabilitiesFile;
use super::rule::{
    first_matching_rule, rule_preferred_tool_format, rule_structured_output,
    rule_structured_output_mode, rule_thinking_block_style, rule_thinking_modes,
    rule_tool_mode_parity, rule_vision, MatchedCapabilityRule, ProviderRule,
};
use super::BUILTIN_PROVIDERS_TOML;

/// Display-oriented row for `harn provider catalog matrix`, the legacy
/// `harn check --provider-matrix` surface, and the generated docs page. Rows
/// are intentionally rule-shaped: `model` is the rule's `model_match` pattern,
/// because the shipped capability source of truth is a first-match rule table
/// rather than an exhaustive remote model inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderCapabilityMatrixRow {
    pub provider: String,
    pub model: String,
    pub version_min: Option<Vec<u32>>,
    /// Whether this rule opts into field-wise fall-through
    /// ([`ProviderRule::extends`]). Rows in this matrix are rule-shaped, so
    /// an `extends` row honestly reports its OWN fields only — for a
    /// matching model, unset fields resolve from later matching rows and
    /// provider defaults rather than the printed per-rule values.
    pub extends: bool,
    pub thinking: Vec<String>,
    pub vision: bool,
    pub audio: bool,
    pub pdf: bool,
    pub video: bool,
    pub streaming: bool,
    pub files_api_supported: bool,
    pub json_schema: Option<String>,
    pub prefers_xml_scaffolding: bool,
    pub reserved_tool_call_token: bool,
    pub prefers_markdown_scaffolding: bool,
    pub structured_output_mode: String,
    pub supports_assistant_prefill: bool,
    pub prefers_role_developer: bool,
    pub prefers_xml_tools: bool,
    pub thinking_block_style: String,
    pub native_tools: bool,
    pub text_tools: bool,
    pub preferred_tool_format: String,
    pub tool_mode_parity: String,
    pub tools: bool,
    pub cache: bool,
    /// Serving-quality / precision trust verdict for this route. See
    /// [`ProviderRule::serving_precision`]. `"unverified"` when unset.
    pub serving_precision: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolCapabilityAuditReport {
    pub audited_models: usize,
    pub gaps: Vec<ToolCapabilityAuditGap>,
}

impl ToolCapabilityAuditReport {
    pub fn ok(&self) -> bool {
        self.gaps.is_empty()
    }

    pub fn render_human(&self) -> String {
        if self.gaps.is_empty() {
            return format!(
                "provider capability audit OK: {} priced chat models have explicit native_tools and preferred_tool_format rules",
                self.audited_models
            );
        }

        let mut out = format!(
            "provider capability audit found {} catalog gaps among {} priced chat models:",
            self.gaps.len(),
            self.audited_models
        );
        for gap in &self.gaps {
            let matched = match (&gap.rule_provider, &gap.rule_model_match) {
                (Some(provider), Some(model_match)) => {
                    format!("provider.{provider} model_match=\"{model_match}\"")
                }
                _ => "no matching rule".to_string(),
            };
            out.push_str(&format!(
                "\n- {}:{} ({matched}) missing {}; suggest native_tools = {}, preferred_tool_format = \"{}\"",
                gap.provider,
                gap.model,
                gap.missing_fields.join(", "),
                gap.suggested_native_tools,
                gap.suggested_preferred_tool_format,
            ));
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolCapabilityAuditGap {
    pub provider: String,
    pub model: String,
    pub rule_provider: Option<String>,
    pub rule_model_match: Option<String>,
    pub missing_fields: Vec<String>,
    pub suggested_native_tools: bool,
    pub suggested_preferred_tool_format: String,
}

/// Return the currently-effective provider capability rule matrix. User
/// override rows, when installed for the current thread, are emitted before
/// built-in rows so the display mirrors lookup precedence.
pub fn matrix_rows() -> Vec<ProviderCapabilityMatrixRow> {
    let user = USER_OVERRIDES.with(|cell| cell.borrow().clone());
    let mut rows = Vec::new();
    if let Some(user) = user.as_ref() {
        push_matrix_rows(&mut rows, user, "project");
    }
    push_matrix_rows(&mut rows, builtin(), "builtin");
    rows
}

/// Audit the currently effective provider/model catalog against the currently
/// effective capability rules. This is the user-facing path used by the CLI
/// when authors are adding provider catalog or capability override rows.
pub fn audit_catalogued_chat_model_tool_capabilities() -> ToolCapabilityAuditReport {
    let user = USER_OVERRIDES.with(|cell| cell.borrow().clone());
    audit_tool_capability_coverage(
        crate::llm_config::model_catalog_entries(),
        builtin(),
        user.as_ref(),
    )
}

/// Audit the built-in catalog only. The CI test uses this path so external
/// provider config cannot hide a gap in the shipped TOML assets.
pub fn audit_builtin_catalogued_chat_model_tool_capabilities() -> ToolCapabilityAuditReport {
    let catalog = crate::llm_config::parse_config_toml(BUILTIN_PROVIDERS_TOML)
        .expect("providers.toml must parse at build time");
    audit_tool_capability_coverage(catalog.models, builtin(), None)
}

fn audit_tool_capability_coverage<I>(
    models: I,
    builtin: &CapabilitiesFile,
    user: Option<&CapabilitiesFile>,
) -> ToolCapabilityAuditReport
where
    I: IntoIterator<Item = (String, crate::llm_config::ModelDef)>,
{
    let mut gaps = Vec::new();
    let mut audited_models = 0;

    for (model_id, model) in models {
        if model.pricing.is_none() {
            continue;
        }
        audited_models += 1;
        let matched = first_matching_rule(user, builtin, &model.provider, &model_id);
        let mut missing_fields = Vec::new();
        match matched.as_ref().map(|matched| &matched.rule) {
            Some(rule) => {
                if rule.native_tools.is_none() {
                    missing_fields.push("native_tools".to_string());
                }
                if rule.preferred_tool_format.is_none() {
                    missing_fields.push("preferred_tool_format".to_string());
                }
            }
            None => {
                missing_fields.push("native_tools".to_string());
                missing_fields.push("preferred_tool_format".to_string());
            }
        }
        if missing_fields.is_empty() {
            continue;
        }

        let (suggested_native_tools, suggested_preferred_tool_format) =
            suggested_tool_capability_defaults(
                &model.provider,
                &model_id,
                &model,
                matched.as_ref(),
            );
        gaps.push(ToolCapabilityAuditGap {
            provider: model.provider,
            model: model_id,
            rule_provider: matched.as_ref().map(|matched| matched.provider.clone()),
            // Honest per-rule provenance: an `extends` fall-through chain
            // reports every absorbed rule pattern in precedence order, not a
            // fake single source row.
            rule_model_match: matched.map(|matched| matched.matched_patterns.join(" -> ")),
            missing_fields,
            suggested_native_tools,
            suggested_preferred_tool_format,
        });
    }

    gaps.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then_with(|| left.model.cmp(&right.model))
    });
    ToolCapabilityAuditReport {
        audited_models,
        gaps,
    }
}

fn suggested_tool_capability_defaults(
    provider: &str,
    model_id: &str,
    model: &crate::llm_config::ModelDef,
    matched: Option<&MatchedCapabilityRule>,
) -> (bool, String) {
    if let Some(rule) = matched.map(|matched| &matched.rule) {
        let native_tools = rule.native_tools.unwrap_or_else(|| {
            // Resolve native_tools from the pinned tool_format via its channel
            // so `json` (a TEXT-channel format) correctly implies
            // native_tools = false, identically to `text`. Falling through to
            // the provider heuristic for `json` would wrongly mark a gemini /
            // cerebras row native. Unknown formats keep the heuristic.
            match rule
                .preferred_tool_format
                .as_deref()
                .and_then(crate::llm_config::tool_format_channel)
            {
                Some(crate::llm_config::ToolFormatChannel::Native) => true,
                Some(crate::llm_config::ToolFormatChannel::Text) => false,
                None => suggested_native_tools(provider, model_id, model),
            }
        });
        let preferred_tool_format = rule
            .preferred_tool_format
            .clone()
            .unwrap_or_else(|| tool_format_for_native(native_tools));
        return (native_tools, preferred_tool_format);
    }

    let native_tools = suggested_native_tools(provider, model_id, model);
    (native_tools, tool_format_for_native(native_tools))
}

fn suggested_native_tools(
    provider: &str,
    model_id: &str,
    model: &crate::llm_config::ModelDef,
) -> bool {
    if provider == "anthropic" || model_id.contains("claude") {
        return true;
    }
    if matches!(
        provider,
        "openai" | "gemini" | "cerebras" | "bedrock" | "azure_openai" | "vertex"
    ) {
        return true;
    }
    model
        .capabilities
        .iter()
        .any(|capability| capability == "tools")
}

/// The derived `preferred_tool_format` for a capability row (or unmatched
/// model) that does not pin one. Native-capable models derive `native`;
/// text-channel models derive `json` (fenced-JSON), the GLOBAL text-channel
/// default. Heredoc (`text`) is never auto-derived — it is reachable only via
/// an explicit `preferred_tool_format = "text"` pin or an explicit request (the
/// reverse safety valve). This is the primary default site: it fires for every
/// model that matches a capability row without an explicit format pin.
fn tool_format_for_native(native_tools: bool) -> String {
    if native_tools {
        "native".to_string()
    } else {
        "json".to_string()
    }
}

fn push_matrix_rows(
    rows: &mut Vec<ProviderCapabilityMatrixRow>,
    file: &CapabilitiesFile,
    source: &str,
) {
    for (provider, rules) in &file.provider {
        for rule in rules {
            rows.push(rule_to_matrix_row(provider, rule, source));
        }
    }
}

fn rule_to_matrix_row(
    provider: &str,
    rule: &ProviderRule,
    source: &str,
) -> ProviderCapabilityMatrixRow {
    ProviderCapabilityMatrixRow {
        provider: provider.to_string(),
        model: rule.model_match.clone(),
        version_min: rule.version_min.clone(),
        extends: rule.extends,
        thinking: rule_thinking_modes(rule),
        vision: rule_vision(rule),
        audio: rule.audio.unwrap_or(false),
        pdf: rule.pdf.unwrap_or(false),
        video: rule.video.unwrap_or(false),
        streaming: true,
        files_api_supported: rule.files_api_supported.unwrap_or(false),
        json_schema: rule_structured_output(rule),
        prefers_xml_scaffolding: rule.prefers_xml_scaffolding.unwrap_or(false),
        reserved_tool_call_token: rule.reserved_tool_call_token.unwrap_or(false),
        prefers_markdown_scaffolding: rule.prefers_markdown_scaffolding.unwrap_or(false),
        structured_output_mode: rule_structured_output_mode(rule),
        supports_assistant_prefill: rule.supports_assistant_prefill.unwrap_or(false),
        prefers_role_developer: rule
            .prefers_role_developer
            .unwrap_or_else(|| rule.requires_completion_tokens.unwrap_or(false)),
        prefers_xml_tools: rule.prefers_xml_tools.unwrap_or(false),
        thinking_block_style: rule_thinking_block_style(rule),
        native_tools: rule.native_tools.unwrap_or(false),
        text_tools: rule.text_tool_wire_format_supported.unwrap_or(true),
        preferred_tool_format: rule_preferred_tool_format(rule),
        tool_mode_parity: rule_tool_mode_parity(rule),
        tools: rule.native_tools.unwrap_or(false)
            || rule.text_tool_wire_format_supported.unwrap_or(true),
        cache: rule.prompt_caching.unwrap_or(false),
        serving_precision: rule
            .serving_precision
            .clone()
            .unwrap_or_else(|| "unverified".to_string()),
        source: source.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::lookup::clear_user_overrides;
    use super::*;

    fn reset() {
        clear_user_overrides();
    }

    #[test]
    fn every_catalogued_chat_model_has_explicit_tool_capabilities() {
        reset();
        let report = audit_builtin_catalogued_chat_model_tool_capabilities();
        assert!(report.ok(), "{}", report.render_human());
    }

    #[test]
    fn every_catalogued_alias_has_explicit_tool_capabilities() {
        // The model-level audit only covers priced catalog `models`, so a
        // `[[provider.local]]` / Ollama alias (e.g. the local gemma-4 route in
        // Fix A) could omit native_tools/preferred_tool_format and silently
        // degrade to text tools without tripping a test. Walk every alias's
        // (provider, id) through the same matcher and require explicit fields.
        reset();
        let catalog = crate::llm_config::parse_config_toml(BUILTIN_PROVIDERS_TOML)
            .expect("providers.toml must parse at build time");
        let builtin = builtin();
        let mut gaps = Vec::new();
        for (alias, def) in &catalog.aliases {
            let matched = first_matching_rule(None, builtin, &def.provider, &def.id);
            let explicit = matched
                .as_ref()
                .map(|matched| {
                    matched.rule.native_tools.is_some()
                        && matched.rule.preferred_tool_format.is_some()
                })
                .unwrap_or(false);
            if !explicit {
                gaps.push(format!(
                    "{alias} -> {}:{} (rule={})",
                    def.provider,
                    def.id,
                    matched
                        .as_ref()
                        .map(|matched| matched.rule.model_match.as_str())
                        .unwrap_or("<none>")
                ));
            }
        }
        assert!(
            gaps.is_empty(),
            "aliases missing explicit native_tools/preferred_tool_format:\n- {}",
            gaps.join("\n- ")
        );
    }

    #[test]
    fn tool_capability_audit_reports_suggested_defaults() {
        reset();
        let capabilities: CapabilitiesFile = toml::from_str(
            r#"
[[provider.acme]]
model_match = "acme-good-*"
preferred_tool_format = "native"
"#,
        )
        .unwrap();
        let report = audit_tool_capability_coverage(
            vec![(
                "acme-good-1".to_string(),
                crate::llm_config::ModelDef {
                    name: "Acme Good".to_string(),
                    provider: "acme".to_string(),
                    context_window: 128_000,
                    logical_model: None,
                    equivalence_group: None,
                    served_variant: None,
                    wire_model: None,
                    api_dialect: None,
                    rate_limits: None,
                    performance: None,
                    architecture: None,
                    local_memory: None,
                    runtime_context_window: None,
                    stream_timeout: None,
                    capabilities: Vec::new(),
                    pricing: Some(crate::llm_config::ModelPricing {
                        input_per_mtok: 1.0,
                        output_per_mtok: 2.0,
                        cache_read_per_mtok: None,
                        cache_write_per_mtok: None,
                        input_token_bands: Vec::new(),
                    }),
                    deprecated: false,
                    deprecation_note: None,
                    superseded_by: None,
                    serving_tiers: Vec::new(),
                    quality_tags: Vec::new(),
                    availability: crate::llm_config::ModelAvailability::Serverless,
                    tier: None,
                    open_weight: None,
                    strengths: Vec::new(),
                    benchmarks: std::collections::BTreeMap::new(),
                    family: None,
                    lineage: None,
                    complementary_with: Vec::new(),
                    avoid_as_reviewer_for: Vec::new(),
                },
            )],
            &capabilities,
            None,
        );

        assert!(!report.ok());
        assert_eq!(report.audited_models, 1);
        assert_eq!(report.gaps.len(), 1);
        assert_eq!(report.gaps[0].missing_fields, ["native_tools"]);
        assert!(report.gaps[0].suggested_native_tools);
        assert_eq!(report.gaps[0].suggested_preferred_tool_format, "native");
        assert!(report.render_human().contains(
            "acme:acme-good-1 (provider.acme model_match=\"acme-good-*\") missing native_tools; suggest native_tools = true, preferred_tool_format = \"native\""
        ));
    }

    #[test]
    fn matrix_rows_include_provider_patterns_and_sources() {
        reset();
        let rows = matrix_rows();
        assert!(rows.iter().any(|row| {
            row.provider == "openai"
                && row.model == "gpt-4o*"
                && row.vision
                && row.audio
                && row.json_schema.as_deref() == Some("native")
                && row.source == "builtin"
        }));
    }
}
