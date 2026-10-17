//! Parse catalog documents and expose ignored-field diagnostics.

use super::ProvidersConfig;

/// Parse a provider/model catalog overlay in the same shape as
/// `providers.toml` or `[llm]` package-manifest sections.
pub fn parse_config_toml(src: &str) -> Result<ProvidersConfig, toml::de::Error> {
    parse_config_toml_with_diagnostics(src).map(|parsed| parsed.config)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfigDiagnostic {
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for ProviderConfigDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            write!(formatter, "{}", self.message)
        } else {
            write!(formatter, "{}: {}", self.path, self.message)
        }
    }
}

#[derive(Debug, Clone)]
pub struct ParsedProvidersConfig {
    pub config: ProvidersConfig,
    pub diagnostics: Vec<ProviderConfigDiagnostic>,
}

/// Parse a provider/model catalog overlay and report keys the schema would
/// otherwise silently ignore. Unknown model/provider fields usually mean a
/// catalog migration was missed; keep serde as the schema source of truth
/// instead of hand-maintaining duplicate top-level allowlists.
pub fn parse_config_toml_with_diagnostics(
    src: &str,
) -> Result<ParsedProvidersConfig, toml::de::Error> {
    let mut diagnostics = Vec::new();
    let deserializer = toml::Deserializer::parse(src)?;
    let config = serde_ignored::deserialize(deserializer, |path| {
        diagnostics.push(unknown_field_diagnostic(path.to_string()));
    })?;
    diagnostics.extend(patch_model_unknown_field_diagnostics(src));
    Ok(ParsedProvidersConfig {
        config,
        diagnostics,
    })
}

fn unknown_field_diagnostic(path: String) -> ProviderConfigDiagnostic {
    let hint = if path.ends_with(".fast_mode") || path == "fast_mode" {
        " `fast_mode` was removed in v0.10.1; use `serving_tiers` with an explicit request knob instead."
    } else {
        ""
    };
    ProviderConfigDiagnostic {
        path,
        message: format!("unknown providers.toml field was ignored.{hint}"),
    }
}

fn patch_model_unknown_field_diagnostics(src: &str) -> Vec<ProviderConfigDiagnostic> {
    let Ok(value) = toml::from_str::<toml::Value>(src) else {
        return Vec::new();
    };
    let Some(patch_models) = value
        .get("patch")
        .and_then(|patch| patch.get("models"))
        .and_then(toml::Value::as_table)
    else {
        return Vec::new();
    };
    let mut diagnostics = Vec::new();
    for (model_id, patch) in patch_models {
        collect_model_patch_unknowns(
            &format!("patch.models.{model_id}"),
            patch,
            model_patch_schema(),
            &mut diagnostics,
        );
    }
    diagnostics
}

fn collect_model_patch_unknowns(
    path: &str,
    value: &toml::Value,
    schema: &PatchSchema,
    diagnostics: &mut Vec<ProviderConfigDiagnostic>,
) {
    let Some(table) = value.as_table() else {
        return;
    };
    for (key, child) in table {
        if schema.freeform.contains(&key.as_str()) {
            continue;
        }
        if let Some((_, nested)) = schema
            .nested
            .iter()
            .find(|(nested_key, _)| *nested_key == key.as_str())
        {
            collect_model_patch_unknowns(&format!("{path}.{key}"), child, nested, diagnostics);
            continue;
        }
        if schema.fields.contains(&key.as_str()) {
            continue;
        }
        diagnostics.push(unknown_field_diagnostic(format!("{path}.{key}")));
    }
}

struct PatchSchema {
    fields: &'static [&'static str],
    freeform: &'static [&'static str],
    nested: &'static [(&'static str, PatchSchema)],
}

fn model_patch_schema() -> &'static PatchSchema {
    static MODEL_PATCH_SCHEMA: PatchSchema = PatchSchema {
        fields: &[
            "name",
            "operations",
            "provider",
            "context_window",
            "logical_model",
            "equivalence_group",
            "served_variant",
            "wire_model",
            "api_dialect",
            "rate_limits",
            "performance",
            "architecture",
            "local_memory",
            "runtime_context_window",
            "stream_timeout",
            "capabilities",
            "pricing",
            "deprecated",
            "deprecation_note",
            "sunset_date",
            "superseded_by",
            "serving_tiers",
            "quality_tags",
            "availability",
            "tier",
            "open_weight",
            "strengths",
            "benchmarks",
            "family",
            "lineage",
            "complementary_with",
            "avoid_as_reviewer_for",
            "completion_review",
            "released",
            "row_kind",
            "current_snapshot",
            "embedding_dim",
            "embedding_max_tokens",
        ],
        freeform: &["benchmarks"],
        nested: &[
            ("pricing", pricing_patch_schema()),
            ("rate_limits", rate_limits_patch_schema()),
            ("performance", performance_patch_schema()),
            ("architecture", architecture_patch_schema()),
            ("local_memory", local_memory_patch_schema()),
            ("completion_review", completion_review_patch_schema()),
        ],
    };
    &MODEL_PATCH_SCHEMA
}

const fn pricing_patch_schema() -> PatchSchema {
    PatchSchema {
        fields: &[
            "input_per_mtok",
            "output_per_mtok",
            "cache_read_per_mtok",
            "cache_write_per_mtok",
            "input_token_bands",
        ],
        freeform: &[],
        nested: &[],
    }
}

const fn rate_limits_patch_schema() -> PatchSchema {
    PatchSchema {
        fields: &[
            "rpm",
            "rph",
            "rpd",
            "tpm",
            "tph",
            "tpd",
            "input_tpm",
            "output_tpm",
            "concurrency",
            "tier",
            "source_url",
            "last_verified",
            "notes",
        ],
        freeform: &[],
        nested: &[],
    }
}

const fn performance_patch_schema() -> PatchSchema {
    PatchSchema {
        fields: &[
            "observed_ttft_ms",
            "output_tokens_per_sec",
            "time_to_answer_s",
            "source",
            "source_url",
            "last_verified",
            "sample_size",
            "notes",
        ],
        freeform: &[],
        nested: &[],
    }
}

const fn architecture_patch_schema() -> PatchSchema {
    PatchSchema {
        fields: &[
            "parameter_count_b",
            "active_parameter_count_b",
            "moe",
            "quantization",
            "precision",
            "license",
            "tokenizer",
            "knowledge_cutoff",
            "source_url",
            "last_verified",
        ],
        freeform: &[],
        nested: &[],
    }
}

const fn local_memory_patch_schema() -> PatchSchema {
    PatchSchema {
        fields: &[
            "measured_resident_gib",
            "measured_context_window",
            "measured_cache_type",
            "base_resident_gib",
            "kv_cache_gib_per_1k_ctx",
            "cache_type_multipliers",
            "default_cache_type",
            "safety_margin_gib",
            "max_recommended_context",
            "source_url",
            "last_verified",
            "notes",
        ],
        freeform: &["cache_type_multipliers"],
        nested: &[],
    }
}

const fn completion_review_patch_schema() -> PatchSchema {
    PatchSchema {
        fields: &["scrutiny", "max_judge_calls", "evidence"],
        freeform: &[],
        nested: &[],
    }
}
