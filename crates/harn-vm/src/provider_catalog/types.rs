use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::llm_config::{
    self, AliasToolCallingDef, LocalMemoryDef, ModelArchitectureDef, ModelAvailability,
    ModelFamilyDimensionDef, ModelFamilyPresetDef, ModelPricing, RateLimitsDef,
    ServingPerformanceDef,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCatalogArtifact {
    pub schema_version: u32,
    pub schema: String,
    pub generated_by: String,
    pub providers: Vec<CatalogProvider>,
    pub models: Vec<CatalogModel>,
    pub aliases: Vec<CatalogAlias>,
    pub variants: Vec<CatalogVariant>,
    pub families: Vec<CatalogModelFamily>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routing_routes: Vec<CatalogRoutingRoute>,
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
    /// Whether zero-valued cache usage fields from this provider represent a
    /// real cache miss. `None` means nobody has declared either way, which is
    /// not the same claim as `Some(false)`: that one asserts the route reports
    /// nothing, and consumers surface it as an audited zero rather than an
    /// unmeasured field. Publishing `None` as `false` would put that assertion
    /// in the mouths of routes nobody has looked at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_usage_accounting: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_usage_accounting: Option<bool>,
    /// Researched retention/training posture: what this provider lets a caller
    /// decide per request, what happens when Harn sets nothing, and the
    /// documentation behind both. Absent means the provider sits in the
    /// registry's expiring unresearched queue — which is not the same claim as
    /// a declared `control_scope: "none"`. Projecting it here lets a consumer
    /// generate an accurate "what Harn sets per provider" table from the
    /// binary instead of restating prose that drifts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_controls: Option<CatalogProviderDataControls>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub performance: Option<ServingPerformanceDef>,
}

/// Catalog projection of a provider's researched retention/training posture.
///
/// Deliberately not the internal `DataControlsDef`. On the wire a control's
/// value is a bool on one provider and a string on another, which the typed
/// `.harn` binding cannot render as a union. The projection therefore carries
/// the kind explicitly beside the literal's text, so a consumer can rebuild
/// the exact wire value without the artifact schema needing a union — and
/// `false` stays distinguishable from `"false"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogProviderDataControls {
    pub control_scope: llm_config::DataControlScope,
    pub retention_default: llm_config::RetentionDefault,
    pub training_default: llm_config::TrainingDefault,
    pub checked_on: String,
    pub sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_controls: Vec<CatalogProviderDataControl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogProviderDataControl {
    pub location: llm_config::DataControlLocation,
    pub name: String,
    pub value_kind: CatalogDataControlValueKind,
    /// The literal as text. Pair with `value_kind` to rebuild the JSON value.
    pub value: String,
    pub effect: llm_config::DataControlEffect,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applies_to: Vec<llm_config::DataControlDialect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caveat: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogDataControlValueKind {
    Bool,
    String,
}

impl CatalogProviderDataControls {
    pub fn from_definition(definition: &llm_config::DataControlsDef) -> Self {
        Self {
            control_scope: definition.control_scope,
            retention_default: definition.retention_default,
            training_default: definition.training_default,
            checked_on: definition.checked_on.clone(),
            sources: definition.sources.clone(),
            note: definition.note.clone(),
            request_controls: definition
                .request_controls
                .iter()
                .map(|control| CatalogProviderDataControl {
                    location: control.location,
                    name: control.name.clone(),
                    value_kind: match control.value {
                        llm_config::DataControlValue::Bool(_) => CatalogDataControlValueKind::Bool,
                        llm_config::DataControlValue::Text(_) => {
                            CatalogDataControlValueKind::String
                        }
                    },
                    value: control.value.as_header_value(),
                    effect: control.effect,
                    applies_to: control.applies_to.clone(),
                    caveat: control.caveat.clone(),
                })
                .collect(),
        }
    }
}

impl CatalogProviderDataControls {
    /// Inverse of [`Self::from_definition`], for rebuilding a `ProviderDef`
    /// from a published artifact. The value's kind is carried explicitly, so
    /// the round trip restores `false` as a bool rather than as `"false"`.
    pub fn to_definition(&self) -> llm_config::DataControlsDef {
        llm_config::DataControlsDef {
            control_scope: self.control_scope,
            retention_default: self.retention_default,
            training_default: self.training_default,
            checked_on: self.checked_on.clone(),
            sources: self.sources.clone(),
            note: self.note.clone(),
            request_controls: self
                .request_controls
                .iter()
                .map(|control| llm_config::DataControlDef {
                    location: control.location,
                    name: control.name.clone(),
                    value: match control.value_kind {
                        CatalogDataControlValueKind::Bool => {
                            llm_config::DataControlValue::Bool(control.value == "true")
                        }
                        CatalogDataControlValueKind::String => {
                            llm_config::DataControlValue::Text(control.value.clone())
                        }
                    },
                    effect: control.effect,
                    applies_to: control.applies_to.clone(),
                    caveat: control.caveat.clone(),
                })
                .collect(),
        }
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region_env: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub regions: BTreeMap<String, ProviderEndpointRegion>,
    pub chat_endpoint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_endpoint: Option<String>,
    /// Provider embeddings route, when one exists. Absent means this
    /// provider has no embeddings API; hosts must not invent a path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embeddings_endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderEndpointRegion {
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_verified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
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
#[serde(deny_unknown_fields)]
pub struct CatalogModel {
    pub id: String,
    pub name: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blurb: Option<String>,
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
    pub performance: Option<ServingPerformanceDef>,
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
    pub batch: Option<ModelBatchSupport>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_review: Option<llm_config::CompletionReviewDef>,
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
    /// Non-default synchronous serving tiers such as fast, priority, or flex.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub serving_tiers: Vec<llm_config::ServingTierDef>,
    /// Non-default reasoning execution modes such as OpenAI pro mode. Sibling
    /// of `serving_tiers`: a tier changes the per-token rate, a mode changes
    /// how many tokens the model spends at unchanged rates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_modes: Vec<llm_config::ReasoningModeDef>,
    /// ISO 8601 date (`YYYY-MM-DD`) when the provider published this snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released: Option<String>,
    /// Pinned snapshot versus undated selector. Absent when unauthored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_kind: Option<llm_config::ModelRowKind>,
    /// Catalog id of the snapshot a selector currently points at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_snapshot: Option<String>,
    /// Embedding vector length when this row describes an embeddings model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_dim: Option<u32>,
    /// Maximum input tokens the embeddings endpoint accepts for this row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_max_tokens: Option<u32>,
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
    /// `declared` when a capability row states `parity` outright, `derived`
    /// when it was computed from `native`/`text`. Both are declarations; a
    /// forced-format sweep is [`Self::empirical_parity`] instead (#5885).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parity_source: Option<String>,
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
pub struct ModelBatchSupport {
    pub wire_format: String,
    pub input_mode: String,
    pub result_ordering: BatchResultOrdering,
    pub partial_failure: BatchPartialFailure,
    pub cancellation: BatchCancellationSupport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discount_percent: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turnaround_hours: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_requests: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_input_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_retention_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub security_notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operational_notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BatchResultOrdering {
    /// Results carry caller-supplied stable ids/keys; consumers must rejoin by
    /// that id instead of trusting response order.
    CustomIdRejoin,
    /// Provider documents that result order matches request order.
    ProviderOrdered,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BatchPartialFailure {
    /// One request can fail without making the entire batch unusable.
    PerRequest,
    /// Provider treats the batch as an all-or-nothing job.
    WholeBatch,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BatchCancellationSupport {
    Supported,
    NotSupported,
    Unknown,
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
    pub effort_levels: Vec<String>,
    pub none_supported: bool,
    pub interleaved_supported: bool,
    pub preserve_thinking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDeprecation {
    pub status: DeprecationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// ISO 8601 date (`YYYY-MM-DD`) when the provider stops serving the route.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sunset_date: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogVariant {
    pub id: String,
    pub label: String,
    pub description: String,
    pub model_id: String,
    pub provider: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automatic_eligibility: Option<llm_config::AutomaticModelEligibility>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogModelFamily {
    pub id: String,
    pub label: String,
    pub plain_description: String,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    pub dimensions: Vec<ModelFamilyDimensionDef>,
    pub presets: Vec<ModelFamilyPresetDef>,
}

/// Provider/model route-decision row derived from the catalog.
///
/// This mirrors the cloud routing-policy row shape: it carries dispatch data
/// and credential *names*, never resolved secret values. VM-facing snapshots
/// must redact `base_url` and `secret_env` when tenant code only needs the
/// selected provider/model/family/capability envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct CatalogRoutingRoute {
    pub provider: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
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
