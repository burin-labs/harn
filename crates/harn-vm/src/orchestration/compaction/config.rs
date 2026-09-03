use crate::value::{VmError, VmValue};

use super::CompactionPolicy;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompactStrategy {
    Llm,
    Truncate,
    Custom,
    ObservationMask,
}

pub fn parse_compact_strategy(value: &str) -> Result<CompactStrategy, VmError> {
    match value {
        "llm" => Ok(CompactStrategy::Llm),
        "truncate" => Ok(CompactStrategy::Truncate),
        "custom" => Ok(CompactStrategy::Custom),
        "observation_mask" => Ok(CompactStrategy::ObservationMask),
        other => Err(VmError::Runtime(format!(
            "unknown compact_strategy '{other}' (expected 'llm', 'truncate', 'custom', or 'observation_mask')"
        ))),
    }
}

pub fn compact_strategy_name(strategy: &CompactStrategy) -> &'static str {
    match strategy {
        CompactStrategy::Llm => "llm",
        CompactStrategy::Truncate => "truncate",
        CompactStrategy::Custom => "custom",
        CompactStrategy::ObservationMask => "observation_mask",
    }
}

#[derive(Clone, Debug, Default)]
pub struct CompactionRequestProvenance {
    /// Normalized strategy requested at the owning seam, before a lifecycle
    /// hook or fallback changes the engine that actually runs.
    pub requested_strategy: Option<String>,
    /// Boundary field that supplied the tier-1 threshold.
    pub threshold_source: Option<CompactionThresholdSource>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionThresholdSource {
    TokenThreshold,
    CompactThreshold,
    TargetTokens,
    CompactionPolicy,
    RuntimeConfig,
    PreCompactModify,
    Default,
}

impl CompactionThresholdSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TokenThreshold => "token_threshold",
            Self::CompactThreshold => "compact_threshold",
            Self::TargetTokens => "target_tokens",
            Self::CompactionPolicy => "compaction_policy",
            Self::RuntimeConfig => "runtime_config",
            Self::PreCompactModify => "pre_compact_modify",
            Self::Default => "default",
        }
    }
}

/// Configuration for automatic transcript compaction in agent loops.
///
/// Tier 1 uses `token_threshold` and `compact_strategy` for early deterministic
/// reduction. Tier 2 uses `hard_limit_tokens` and `hard_limit_strategy` for
/// aggressive recovery near the model context limit.
#[derive(Clone, Debug)]
pub struct AutoCompactConfig {
    /// Request facts captured by the seam that parsed this config. Lifecycle
    /// code snapshots them before hooks alter the effective policy.
    pub request_provenance: CompactionRequestProvenance,
    /// Number of earliest messages to preserve verbatim.
    pub keep_first: usize,
    /// Tier-1 threshold in estimated tokens.
    pub token_threshold: usize,
    /// Maximum character length before per-tool-result microcompaction.
    pub tool_output_max_chars: usize,
    /// Number of recent messages to preserve during compaction.
    pub keep_last: usize,
    /// Tier-1 strategy.
    pub compact_strategy: CompactStrategy,
    /// Optional tier-2 threshold.
    pub hard_limit_tokens: Option<usize>,
    /// Tier-2 strategy.
    pub hard_limit_strategy: CompactStrategy,
    /// Harn callback used by the custom strategy.
    pub custom_compactor: Option<VmValue>,
    /// Pending reminders supplied to the custom compactor.
    pub custom_compactor_reminders: Vec<VmValue>,
    /// Domain-specific observation-mask callback.
    pub mask_callback: Option<VmValue>,
    /// Per-tool-result compression callback.
    pub compress_callback: Option<VmValue>,
    /// Optional LLM compaction prompt asset.
    pub summarize_prompt: Option<String>,
    /// User-facing policy label, which may be broader than the engine strategy.
    pub policy_strategy: String,
    /// Strategy used if the primary strategy fails.
    pub fallback_strategy: Option<CompactStrategy>,
    /// Host or user instructions that guide compaction.
    pub policy: CompactionPolicy,
    /// Maximum observation-mask recap body size.
    pub recap_budget_bytes: usize,
}

impl Default for AutoCompactConfig {
    fn default() -> Self {
        Self {
            request_provenance: CompactionRequestProvenance::default(),
            keep_first: 0,
            token_threshold: 48_000,
            tool_output_max_chars: 16_000,
            keep_last: 12,
            compact_strategy: CompactStrategy::ObservationMask,
            hard_limit_tokens: None,
            hard_limit_strategy: CompactStrategy::Llm,
            custom_compactor: None,
            custom_compactor_reminders: Vec::new(),
            mask_callback: None,
            compress_callback: None,
            summarize_prompt: None,
            policy_strategy: compact_strategy_name(&CompactStrategy::ObservationMask).to_string(),
            fallback_strategy: None,
            policy: CompactionPolicy::default(),
            recap_budget_bytes: DEFAULT_RECAP_BUDGET_BYTES,
        }
    }
}

/// Default byte budget for an observation-mask recap body (about 4k tokens).
pub const DEFAULT_RECAP_BUDGET_BYTES: usize = 16_000;
