//! Agent-loop transcript compaction policy.
//!
//! The orchestration module owns the low-level compaction engine. This
//! module owns the user-facing agent-loop policy shape and lowers it to that
//! engine so every `agent_loop` caller gets the same defaults.

use crate::llm::helpers::{opt_bool, opt_int, opt_str};
use crate::value::{VmError, VmValue};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompactionStrategy {
    None,
    Truncate {
        keep_first_n: usize,
        keep_last_n: usize,
    },
    SummarizeMiddle {
        keep_first_n: usize,
        keep_last_n: usize,
    },
    SummarizeAll {
        keep_first_n: usize,
    },
    Hybrid {
        keep_first_n: usize,
        keep_last_n: usize,
    },
}

impl Default for CompactionStrategy {
    fn default() -> Self {
        Self::Hybrid {
            keep_first_n: 0,
            keep_last_n: 10,
        }
    }
}

impl CompactionStrategy {
    pub fn label(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Truncate { .. } => "truncate",
            Self::SummarizeMiddle { .. } => "summarize_middle",
            Self::SummarizeAll { .. } => "summarize_all",
            Self::Hybrid { .. } => "hybrid",
        }
    }
}

pub(crate) async fn resolve_agent_loop_auto_compact(
    args: &[VmValue],
    options: &Option<std::collections::BTreeMap<String, VmValue>>,
) -> Result<Option<crate::orchestration::AutoCompactConfig>, VmError> {
    let has_auto_compact_key = options
        .as_ref()
        .is_some_and(|opts| opts.contains_key("auto_compact"));
    let has_compaction_key = options
        .as_ref()
        .is_some_and(|opts| opts.contains_key("compaction"));

    let mut ac = crate::orchestration::AutoCompactConfig::default();
    let user_specified_threshold = opt_int(options, "compact_threshold").is_some();
    let user_specified_hard_limit = opt_int(options, "hard_limit_tokens").is_some();

    if has_compaction_key {
        let value = options.as_ref().and_then(|opts| opts.get("compaction"));
        let strategy = parse_compaction_strategy(value, options)?;
        if matches!(strategy, CompactionStrategy::None) {
            return Ok(None);
        }
        apply_strategy(&mut ac, strategy);
    } else if has_auto_compact_key {
        if !opt_bool(options, "auto_compact") {
            return Ok(None);
        }
        apply_legacy_options(&mut ac, options)?;
    } else {
        apply_strategy(&mut ac, CompactionStrategy::default());
    }

    apply_common_options(&mut ac, options)?;
    if !has_compaction_key && has_auto_compact_key {
        apply_legacy_options(&mut ac, options)?;
    }

    let probe_opts = crate::llm::helpers::extract_llm_options(args)?;
    crate::llm::api::adapt_auto_compact_to_provider(
        &mut ac,
        user_specified_threshold,
        user_specified_hard_limit,
        &probe_opts.provider,
        &probe_opts.model,
        &probe_opts.api_key,
    )
    .await;

    Ok(Some(ac))
}

fn parse_compaction_strategy(
    value: Option<&VmValue>,
    options: &Option<std::collections::BTreeMap<String, VmValue>>,
) -> Result<CompactionStrategy, VmError> {
    let Some(value) = value else {
        return Ok(CompactionStrategy::default());
    };
    match value {
        VmValue::Nil => Ok(CompactionStrategy::default()),
        VmValue::Bool(false) => Ok(CompactionStrategy::None),
        VmValue::Bool(true) => Ok(CompactionStrategy::default()),
        VmValue::String(label) => strategy_from_label(label, 0, 10),
        VmValue::Dict(dict) => {
            let label = dict
                .get("type")
                .or_else(|| dict.get("kind"))
                .or_else(|| dict.get("strategy"))
                .and_then(|value| match value {
                    VmValue::String(s) => Some(s.to_string()),
                    _ => None,
                })
                .unwrap_or_else(|| "hybrid".to_string());
            let keep_first_n = dict
                .get("keep_first_n")
                .or_else(|| dict.get("keep_first"))
                .and_then(VmValue::as_int)
                .or_else(|| opt_int(options, "compact_keep_first"))
                .unwrap_or(0);
            let keep_last_n = dict
                .get("keep_last_n")
                .or_else(|| dict.get("keep_last"))
                .and_then(VmValue::as_int)
                .or_else(|| opt_int(options, "compact_keep_last"))
                .unwrap_or(10);
            if keep_first_n < 0 || keep_last_n < 0 {
                return Err(VmError::Runtime(
                    "agent_loop: compaction keep counts must be >= 0".to_string(),
                ));
            }
            strategy_from_label(&label, keep_first_n as usize, keep_last_n as usize)
        }
        _ => Err(VmError::Runtime(
            "agent_loop: compaction must be a string, dict, bool, or nil".to_string(),
        )),
    }
}

fn strategy_from_label(
    label: &str,
    keep_first_n: usize,
    keep_last_n: usize,
) -> Result<CompactionStrategy, VmError> {
    match label {
        "none" => Ok(CompactionStrategy::None),
        "truncate" => Ok(CompactionStrategy::Truncate {
            keep_first_n,
            keep_last_n,
        }),
        "summarize_middle" | "summarize" | "llm" => Ok(CompactionStrategy::SummarizeMiddle {
            keep_first_n,
            keep_last_n,
        }),
        "summarize_all" => Ok(CompactionStrategy::SummarizeAll { keep_first_n }),
        "hybrid" => Ok(CompactionStrategy::Hybrid {
            keep_first_n,
            keep_last_n,
        }),
        other => Err(VmError::Runtime(format!(
            "agent_loop: unknown compaction strategy '{other}' (expected none, truncate, summarize_middle, summarize_all, or hybrid)"
        ))),
    }
}

fn apply_strategy(ac: &mut crate::orchestration::AutoCompactConfig, strategy: CompactionStrategy) {
    ac.policy_strategy = strategy.label().to_string();
    match strategy {
        CompactionStrategy::None => {}
        CompactionStrategy::Truncate {
            keep_first_n,
            keep_last_n,
        } => {
            ac.keep_first = keep_first_n;
            ac.keep_last = keep_last_n;
            ac.compact_strategy = crate::orchestration::CompactStrategy::Truncate;
            ac.hard_limit_strategy = crate::orchestration::CompactStrategy::Truncate;
        }
        CompactionStrategy::SummarizeMiddle {
            keep_first_n,
            keep_last_n,
        } => {
            ac.keep_first = keep_first_n;
            ac.keep_last = keep_last_n;
            ac.compact_strategy = crate::orchestration::CompactStrategy::Llm;
            ac.hard_limit_strategy = crate::orchestration::CompactStrategy::Llm;
        }
        CompactionStrategy::SummarizeAll { keep_first_n } => {
            ac.keep_first = keep_first_n;
            ac.keep_last = 0;
            ac.compact_strategy = crate::orchestration::CompactStrategy::Llm;
            ac.hard_limit_strategy = crate::orchestration::CompactStrategy::Llm;
        }
        CompactionStrategy::Hybrid {
            keep_first_n,
            keep_last_n,
        } => {
            ac.keep_first = keep_first_n;
            ac.keep_last = keep_last_n;
            ac.compact_strategy = crate::orchestration::CompactStrategy::Llm;
            ac.hard_limit_strategy = crate::orchestration::CompactStrategy::Truncate;
        }
    }
}

fn apply_common_options(
    ac: &mut crate::orchestration::AutoCompactConfig,
    options: &Option<std::collections::BTreeMap<String, VmValue>>,
) -> Result<(), VmError> {
    if let Some(v) = opt_int(options, "compact_threshold") {
        if v < 0 {
            return Err(VmError::Runtime(
                "agent_loop: compact_threshold must be >= 0".to_string(),
            ));
        }
        ac.token_threshold = v as usize;
    }
    if let Some(v) = opt_int(options, "tool_output_max_chars") {
        if v < 0 {
            return Err(VmError::Runtime(
                "agent_loop: tool_output_max_chars must be >= 0".to_string(),
            ));
        }
        ac.tool_output_max_chars = v as usize;
    }
    if let Some(v) = opt_int(options, "compact_keep_first") {
        if v < 0 {
            return Err(VmError::Runtime(
                "agent_loop: compact_keep_first must be >= 0".to_string(),
            ));
        }
        ac.keep_first = v as usize;
    }
    if let Some(v) = opt_int(options, "compact_keep_last") {
        if v < 0 {
            return Err(VmError::Runtime(
                "agent_loop: compact_keep_last must be >= 0".to_string(),
            ));
        }
        ac.keep_last = v as usize;
    }
    if let Some(v) = opt_int(options, "hard_limit_tokens") {
        if v < 0 {
            return Err(VmError::Runtime(
                "agent_loop: hard_limit_tokens must be >= 0".to_string(),
            ));
        }
        ac.hard_limit_tokens = Some(v as usize);
    }
    if let Some(prompt) = opt_str(options, "summarize_prompt") {
        ac.summarize_prompt = Some(prompt);
    }
    if let Some(callback) = options.as_ref().and_then(|opts| opts.get("mask_callback")) {
        ac.mask_callback = Some(callback.clone());
    }
    if let Some(callback) = options
        .as_ref()
        .and_then(|opts| opts.get("compress_callback"))
    {
        ac.compress_callback = Some(callback.clone());
    }
    Ok(())
}

fn apply_legacy_options(
    ac: &mut crate::orchestration::AutoCompactConfig,
    options: &Option<std::collections::BTreeMap<String, VmValue>>,
) -> Result<(), VmError> {
    if let Some(strategy) = opt_str(options, "compact_strategy") {
        ac.policy_strategy = strategy.clone();
        ac.compact_strategy = crate::orchestration::parse_compact_strategy(&strategy)?;
    }
    if let Some(strategy) = opt_str(options, "hard_limit_strategy") {
        ac.hard_limit_strategy = crate::orchestration::parse_compact_strategy(&strategy)?;
    }
    if let Some(callback) = options
        .as_ref()
        .and_then(|opts| opts.get("compact_callback"))
    {
        ac.custom_compactor = Some(callback.clone());
        if !options
            .as_ref()
            .is_some_and(|opts| opts.contains_key("compact_strategy"))
        {
            ac.compact_strategy = crate::orchestration::CompactStrategy::Custom;
            ac.policy_strategy = "custom".to_string();
        }
    }
    Ok(())
}
