use crate::agent_sessions;
use crate::value::{VmError, VmValue};

use super::err;

pub(super) async fn compact_session(
    ctx: &crate::vm::AsyncBuiltinCtx,
    id: &str,
    args: &[VmValue],
) -> Result<VmValue, VmError> {
    if !agent_sessions::exists(id) {
        return Err(err(format!(
            "agent_session_compact: unknown session id '{id}'"
        )));
    }
    let opts_dict = match args.get(1) {
        Some(VmValue::Dict(d)) => (**d).clone(),
        None | Some(VmValue::Nil) => crate::value::DictMap::new(),
        _ => return Err(err("agent_session_compact: `opts` must be a dict or nil")),
    };
    let mut config = build_compact_config(&opts_dict)?;
    let mut messages = agent_sessions::messages_json(id);
    let original_count = messages.len();
    let reminder_events = session_compactable_events(id);
    let provider_options = if opts_dict.is_empty() {
        serde_json::json!({})
    } else {
        crate::llm::reminder_providers::options_map_to_json(&opts_dict)
    };
    let lifecycle =
        crate::orchestration::CompactLifecycle::new(crate::orchestration::CompactMode::Host)
            .with_session_id(Some(id))
            .with_reminder_events(reminder_events)
            .with_provider_options(provider_options);
    let Some(outcome) = crate::orchestration::run_compaction_lifecycle_with_ctx(
        Some(ctx),
        &mut messages,
        &mut config,
        None,
        lifecycle,
    )
    .await?
    else {
        return Ok(VmValue::Int(original_count as i64));
    };
    agent_sessions::replace_messages_with_summary(id, &messages, Some(&outcome.summary))
        .map_err(err)?;
    let compaction_event = crate::llm::helpers::transcript_event_with_id(
        &outcome.receipt.receipt_id,
        "compaction",
        "system",
        "internal",
        "",
        Some(outcome.event_metadata),
    );
    agent_sessions::append_event(id, compaction_event).map_err(err)?;
    for preserved in outcome.reminder_report.preserved_events {
        agent_sessions::append_event(id, preserved).map_err(err)?;
    }
    crate::orchestration::emit_transcript_compacted_event(Some(ctx), id, outcome.receipt).await;
    Ok(VmValue::Int(messages.len() as i64))
}

fn session_compactable_events(id: &str) -> Vec<VmValue> {
    let Some(transcript) = agent_sessions::transcript(id) else {
        return Vec::new();
    };
    let Some(dict) = transcript.as_dict() else {
        return Vec::new();
    };
    crate::orchestration::transcript_compactable_events(dict)
}

fn compact_usize_opt(
    opts: &crate::value::DictMap,
    key: &'static str,
) -> Result<Option<usize>, VmError> {
    let Some(value) = opts.get(key) else {
        return Ok(None);
    };
    let Some(raw) = value.as_int() else {
        return Err(err(format!(
            "agent_session_compact: `{key}` must be an int"
        )));
    };
    if raw < 0 {
        return Err(err(format!("agent_session_compact: `{key}` must be >= 0")));
    }
    Ok(Some(raw as usize))
}

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
    config.custom_compactor = closure_option(opts, "custom_compactor", "agent_session_compact")?;
    config.mask_callback = closure_option(opts, "mask_callback", "agent_session_compact")?;
    config.compress_callback = closure_option(opts, "compress_callback", "agent_session_compact")?;
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

pub(crate) fn closure_option(
    opts: &crate::value::DictMap,
    key: &str,
    builtin: &str,
) -> Result<Option<VmValue>, VmError> {
    let Some(value) = opts.get(key).cloned() else {
        return Ok(None);
    };
    if !matches!(value, VmValue::Closure(_)) {
        return Err(err(format!("{builtin}: `{key}` must be a closure")));
    }
    Ok(Some(value))
}
