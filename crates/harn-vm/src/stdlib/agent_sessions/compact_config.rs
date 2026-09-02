use crate::value::{VmError, VmValue};

use super::{compact_usize_opt, err};

const COMPACT_OPT_KEYS: &[&str] = &[
    "keep_last",
    "token_threshold",
    "tool_output_max_chars",
    "compact_strategy",
    "hard_limit_tokens",
    "hard_limit_strategy",
    "custom_compactor",
    "mask_callback",
    "compress_callback",
    "policy",
    "compaction_policy",
    "compaction_request",
    "instructions",
    "mode",
    "scope",
    "preserve",
    "drop",
    "extend_default_instructions",
    "author",
];

pub(super) fn build_compact_config(
    opts: &crate::value::DictMap,
) -> Result<crate::orchestration::AutoCompactConfig, VmError> {
    for key in opts.keys() {
        if !COMPACT_OPT_KEYS.contains(&key.as_str()) {
            let expected = COMPACT_OPT_KEYS.join(", ");
            return Err(err(format!(
                "agent_session_compact: unknown option key '{key}' (expected one of: {expected})"
            )));
        }
    }

    let mut config = crate::orchestration::AutoCompactConfig {
        policy: crate::orchestration::parse_compaction_policy_options(
            Some(opts),
            "agent_session_compact",
        )?,
        ..Default::default()
    };
    if let Some(value) = compact_usize_opt(opts, "keep_last")? {
        config.keep_last = value;
    }
    if let Some(value) = compact_usize_opt(opts, "token_threshold")? {
        config.token_threshold = value;
    }
    if let Some(value) = compact_usize_opt(opts, "tool_output_max_chars")? {
        config.tool_output_max_chars = value;
    }
    if let Some(VmValue::String(strategy)) = opts.get("compact_strategy") {
        config.compact_strategy = crate::orchestration::parse_compact_strategy(strategy)?;
        config.policy_strategy =
            crate::orchestration::compact_strategy_name(&config.compact_strategy).to_string();
    }
    if let Some(value) = compact_usize_opt(opts, "hard_limit_tokens")? {
        config.hard_limit_tokens = Some(value);
    }
    if let Some(VmValue::String(strategy)) = opts.get("hard_limit_strategy") {
        config.hard_limit_strategy = crate::orchestration::parse_compact_strategy(strategy)?;
    }
    config.custom_compactor = closure_option(opts, "custom_compactor")?;
    config.mask_callback = closure_option(opts, "mask_callback")?;
    config.compress_callback = closure_option(opts, "compress_callback")?;
    config.request_provenance = crate::orchestration::CompactionRequestProvenance {
        requested_strategy: Some(
            crate::orchestration::compact_strategy_name(&config.compact_strategy).to_string(),
        ),
        threshold_source: opts
            .contains_key("token_threshold")
            .then_some(crate::orchestration::CompactionThresholdSource::TokenThreshold),
    };
    Ok(config)
}

fn closure_option(opts: &crate::value::DictMap, key: &str) -> Result<Option<VmValue>, VmError> {
    let Some(value) = opts.get(key).cloned() else {
        return Ok(None);
    };
    if !matches!(value, VmValue::Closure(_)) {
        return Err(err(format!(
            "agent_session_compact: `{key}` must be a closure"
        )));
    }
    Ok(Some(value))
}
