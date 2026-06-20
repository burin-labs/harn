use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::llm_config::{
    self, AliasToolCallingDef, LocalMemoryDef, ModelArchitectureDef, ModelAvailability,
    ModelPricing, RateLimitsDef,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCatalogArtifact {
    pub schema_version: u32,
    pub schema: String,
    pub generated_by: String,
    pub providers: Vec<CatalogProvider>,
    pub models: Vec<CatalogModel>,
    pub aliases: Vec<CatalogAlias>,
    pub variants: Vec<CatalogVariant>,
    pub qc_defaults: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogProvider {
    pub id: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub classification: ProviderClassification,
    pub endpoint: ProviderEndpoint,
    pub auth: ProviderAuth,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra_headers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<CatalogProviderHealthcheck>,
    pub protocols: Vec<String>,
    pub features: Vec<String>,
    pub caveats: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limits: Option<RateLimitsDef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_runtime: Option<llm_config::LocalRuntimeDef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_p50_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderClassification {
    Hosted,
    Local,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEndpoint {
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url_env: Option<String>,
    pub chat_endpoint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderAuth {
    pub style: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    pub env: Vec<String>,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogProviderHealthcheck {
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogAlias {
    pub name: String,
    pub model_id: String,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calling: Option<AliasToolCallingDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogModel {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub aliases: Vec<String>,
    pub context_window: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equivalence_group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub served_variant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wire_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_dialect: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limits: Option<RateLimitsDef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub architecture: Option<ModelArchitectureDef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_memory: Option<LocalMemoryDef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_timeout: Option<f64>,
    pub modalities: ModelModalities,
    pub tool_support: ModelToolSupport,
    pub structured_output: String,
    pub format_preferences: ModelFormatPreferences,
    pub reasoning: ModelReasoning,
    pub prompt_cache: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelPricing>,
    pub deprecation: ModelDeprecation,
    pub availability: ModelAvailabilityStatus,
    pub quality_tags: Vec<String>,
    pub capability_tags: Vec<String>,
    pub family: String,
    pub lineage: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub complementary_with: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avoid_as_reviewer_for: Vec<String>,
    /// Popular-consensus tier label: "small" | "mid" | "frontier" |
    /// "reasoning". Self-declared on the model row; the rule-based path
    /// is a fallback only.
    pub tier: String,
    /// True when weights are downloadable / self-hostable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_weight: Option<bool>,
    /// Workload-shaped strength tags (coding, summarization, vision, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strengths: Vec<String>,
    /// Public benchmark numbers, snake_case identifier -> score.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub benchmarks: BTreeMap<String, f64>,
    /// Accelerated-serving ("fast mode") tier metadata, when offered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fast_mode: Option<ModelFastMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailabilityStatus {
    Serverless,
    Dedicated,
    Unknown,
}

impl From<ModelAvailability> for ModelAvailabilityStatus {
    fn from(value: ModelAvailability) -> Self {
        match value {
            ModelAvailability::Serverless => Self::Serverless,
            ModelAvailability::Dedicated => Self::Dedicated,
            ModelAvailability::Unknown => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelModalities {
    pub input: Vec<String>,
    pub output: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelToolSupport {
    pub native: bool,
    pub text: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parity_notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empirical_parity: Option<ModelToolEmpiricalParity>,
    pub tool_search: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tools: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelToolEmpiricalParity {
    pub verdict: String,
    pub preferred_format: String,
    pub confidence: String,
    pub sample_size: u32,
    pub last_evaluated: String,
    pub native_pass_rate: f64,
    pub text_pass_rate: f64,
    pub verifier_divergence_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelFormatPreferences {
    pub prefers_xml_scaffolding: bool,
    pub prefers_markdown_scaffolding: bool,
    pub structured_output_mode: String,
    pub supports_assistant_prefill: bool,
    pub prefers_role_developer: bool,
    pub prefers_xml_tools: bool,
    pub thinking_block_style: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelReasoning {
    pub modes: Vec<String>,
    pub effort_supported: bool,
    pub none_supported: bool,
    pub interleaved_supported: bool,
    pub preserve_thinking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDeprecation {
    pub status: DeprecationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Catalog id of the model that supersedes this one, when declared.
    /// Surfaces `ModelDef::superseded_by` as a machine-readable migration
    /// target so downstream consumers don't have to parse `note` prose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeprecationStatus {
    Active,
    Deprecated,
}

/// Catalog projection of an accelerated-serving ("fast mode") tier.
/// Surfaces `ModelDef::fast_mode` so downstream consumers can show the
/// opt-in knob, premium pricing, and lifecycle without re-parsing the
/// source TOML. Absent on models with no faster serving path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelFastMode {
    pub param: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub beta_header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub otps_speedup: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelPricing>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogVariant {
    pub id: String,
    pub label: String,
    pub description: String,
    pub model_id: String,
    pub provider: String,
    pub source: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderCatalogValidation {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl ProviderCatalogValidation {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}
