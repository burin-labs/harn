mod blocks;
mod messages;
mod opt_get;
mod options;
mod provider;
mod transcript;

use crate::value::VmValue;

pub(crate) use messages::{
    json_messages_to_vm, vm_add_role_message, vm_message_value, vm_messages_to_json,
};
pub(crate) use opt_get::{opt_bool, opt_float, opt_int, opt_str};
pub(crate) use options::{
    assemble_system_prompt, compose_system_prompt, expects_structured_output, extract_json,
    extract_llm_options, project_llm_options, resolve_catalog_thinking_config,
    resolve_thinking_config, system_prompt_event_metadata, system_prompt_metadata,
    validate_llm_option_keys,
};
pub(crate) use provider::{vm_resolve_model, vm_resolve_provider, ResolvedProvider};
#[cfg(test)]
pub(crate) use transcript::transcript_to_vm_with_events;
pub(crate) use transcript::{
    apply_reminder_post_turn, emit_reminder_lifecycle_event, is_transcript_value,
    new_transcript_with, new_transcript_with_events, normalize_transcript_asset,
    reminder_from_event, reminder_from_vm_value, reminder_lifecycle_payload,
    reminder_propagation_from_transcript, replace_reminder_payload, transcript_asset_list,
    transcript_drain_decision_event_from_value, transcript_event, transcript_event_from_message,
    transcript_events_from_messages, transcript_id, transcript_message_list,
    transcript_reminder_event_from_value, transcript_resumption_event_from_value,
    transcript_summary_text, transcript_suspension_event_from_value,
    transcript_to_vm_with_event_prefix,
};
// Re-exports reserved for the R-02+ wave (stdlib reminder providers,
// bridge `agent/inject_reminder`, hook `Reminder` return variants).
// They live here so consumers don't have to reach into the
// `transcript::` submodule; mark as unused-allowed until those callers
// land.
#[allow(unused_imports)]
pub(crate) use transcript::{
    transcript_drain_decision_event, transcript_reminder_event, transcript_resumption_event,
    transcript_suspension_event, DrainDecision, DrainDecisionAction, DrainDecisionItem,
    DrainDecisionItemCategory, ReminderPropagate, ReminderRoleHint, ReminderSource, Resumption,
    ResumptionInitiator, Suspension, SuspensionInitiator, SystemReminder,
    DRAIN_DECISION_EVENT_KIND, REMINDER_DEDUPED_EVENT_KIND, REMINDER_DROPPED_EVENT_KIND,
    REMINDER_EXPIRED_EVENT_KIND, REMINDER_FIRED_EVENT_KIND, REMINDER_INHERITED_EVENT_KIND,
    REMINDER_INJECTED_EVENT_KIND, REMINDER_ITERATION_SUMMARY_EVENT_KIND, REMINDER_LIFECYCLE_TOPIC,
    REMINDER_PROVIDER_EVALUATED_EVENT_KIND, RESUMPTION_EVENT_KIND, SUSPENSION_EVENT_KIND,
    SYSTEM_REMINDER_EVENT_KIND,
};

pub(super) const TRANSCRIPT_TYPE: &str = "transcript";
pub(super) const TRANSCRIPT_ASSET_TYPE: &str = "transcript_asset";
pub(super) const TRANSCRIPT_VERSION: i64 = 2;

/// Convert a VmValue dict to serde_json::Value for API payloads.
pub(crate) fn vm_value_dict_to_json(dict: &crate::value::DictMap) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in dict {
        map.insert(k.to_string(), vm_value_to_json(v));
    }
    serde_json::Value::Object(map)
}

pub fn vm_value_to_json(val: &VmValue) -> serde_json::Value {
    match val {
        VmValue::Int(i) => serde_json::json!(i),
        VmValue::Float(f) => serde_json::json!(f),
        // Decimal crosses the host bridge as a string to preserve exact
        // precision (binary-float JSON numbers would corrupt money values).
        VmValue::Decimal(d) => serde_json::json!(d.to_string()),
        VmValue::String(s) => serde_json::json!(s.as_str()),
        VmValue::Bytes(bytes) => crate::schema::tagged_bytes_json(bytes),
        VmValue::Bool(b) => serde_json::json!(b),
        VmValue::Nil => serde_json::Value::Null,
        VmValue::List(list) => {
            serde_json::Value::Array(list.iter().map(vm_value_to_json).collect())
        }
        VmValue::Dict(d) => vm_value_dict_to_json(d),
        VmValue::StructInstance(_) => {
            vm_value_dict_to_json(&val.struct_fields_map().unwrap_or_default())
        }
        _ => serde_json::json!(val.display()),
    }
}

/// Strict variant of [`vm_value_to_json`] for durable persistence seams
/// (worker snapshots, session state written to disk).
///
/// [`vm_value_to_json`] serves display/debug paths, so it stringifies
/// runtime-only values — closures, task handles, channels, streams — via
/// `display()`. That is fine for a log line, but at a persistence seam it
/// silently corrupts state: the value rehydrates as a plain string and only
/// fails much later, far from the save that dropped the data. This variant
/// refuses those values with a path-annotated error such as
/// `options.custom_compactor: closure is not serializable` so the caller can
/// fail loud (or strip-and-warn) at save time.
///
/// Data-shaped values without a native JSON form (durations, enum variants,
/// sets, ranges, pairs) keep the lenient `display()` encoding — they
/// round-trip as readable strings today and erroring on them would break
/// existing snapshots for no safety gain.
pub fn vm_value_to_json_strict(val: &VmValue, path: &str) -> Result<serde_json::Value, String> {
    match val {
        VmValue::List(list) => {
            let mut items = Vec::with_capacity(list.len());
            for (index, item) in list.iter().enumerate() {
                items.push(vm_value_to_json_strict(item, &format!("{path}[{index}]"))?);
            }
            Ok(serde_json::Value::Array(items))
        }
        VmValue::Dict(dict) => vm_value_dict_to_json_strict(dict, path),
        VmValue::StructInstance(_) => {
            vm_value_dict_to_json_strict(&val.struct_fields_map().unwrap_or_default(), path)
        }
        VmValue::Closure(_)
        | VmValue::BuiltinRef(_)
        | VmValue::BuiltinRefId(_)
        | VmValue::TaskHandle(_)
        | VmValue::Channel(_)
        | VmValue::Atomic(_)
        | VmValue::Rng(_)
        | VmValue::SyncPermit(_)
        | VmValue::McpClient(_)
        | VmValue::VerdictReceipt(_)
        | VmValue::Generator(_)
        | VmValue::Stream(_)
        | VmValue::Iter(_)
        | VmValue::Harness(_) => Err(format!("{path}: {} is not serializable", val.type_name())),
        other => Ok(vm_value_to_json(other)),
    }
}

/// Dict walker for [`vm_value_to_json_strict`]; extends the error path with
/// each key (`options.hooks[0].callback`) as it recurses.
pub(crate) fn vm_value_dict_to_json_strict(
    dict: &crate::value::DictMap,
    path: &str,
) -> Result<serde_json::Value, String> {
    let mut map = serde_json::Map::new();
    for (k, v) in dict {
        map.insert(
            k.to_string(),
            vm_value_to_json_strict(v, &format!("{path}.{k}"))?,
        );
    }
    Ok(serde_json::Value::Object(map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::resolve_api_key;
    use crate::value::VmDictExt;

    use std::rc::Rc;

    #[test]
    fn local_provider_is_selected_when_local_base_url_and_model_are_set() {
        // Share the crate-wide LLM env lock so this test cannot race with
        // sibling modules (e.g. llm::api streaming classification tests) that
        // also mutate LOCAL_LLM_BASE_URL.
        let _guard = crate::llm::env_guard();
        let prev_base = std::env::var("LOCAL_LLM_BASE_URL").ok();
        let prev_model = std::env::var("LOCAL_LLM_MODEL").ok();
        let prev_harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        let prev_harn_model = std::env::var("HARN_LLM_MODEL").ok();

        unsafe {
            std::env::set_var("LOCAL_LLM_BASE_URL", "http://127.0.0.1:8000");
            std::env::set_var("LOCAL_LLM_MODEL", "qwen2.5-coder-32b");
            std::env::remove_var("HARN_LLM_PROVIDER");
            std::env::remove_var("HARN_LLM_MODEL");
        }

        assert_eq!(vm_resolve_provider(&None), "local");
        assert_eq!(vm_resolve_model(&None, "local"), "qwen2.5-coder-32b");
        assert!(resolve_api_key("local").is_ok());

        unsafe {
            match prev_base {
                Some(value) => std::env::set_var("LOCAL_LLM_BASE_URL", value),
                None => std::env::remove_var("LOCAL_LLM_BASE_URL"),
            }
            match prev_model {
                Some(value) => std::env::set_var("LOCAL_LLM_MODEL", value),
                None => std::env::remove_var("LOCAL_LLM_MODEL"),
            }
            match prev_harn_provider {
                Some(value) => std::env::set_var("HARN_LLM_PROVIDER", value),
                None => std::env::remove_var("HARN_LLM_PROVIDER"),
            }
            match prev_harn_model {
                Some(value) => std::env::set_var("HARN_LLM_MODEL", value),
                None => std::env::remove_var("HARN_LLM_MODEL"),
            }
        }
    }

    #[test]
    fn catalog_known_model_beats_local_base_url_fast_path() {
        // Regression: when LOCAL_LLM_BASE_URL is set (e.g. an Ollama
        // server running on the dev box), an explicit `model:` option
        // that names a catalog-known cross-provider alias must route
        // to the catalog provider, not silently fall into the
        // `local` fast-path. Otherwise a call like
        // `llm_call(..., {model: "anthropic/claude-sonnet-4-6"})`
        // ends up at the local Ollama endpoint and returns a 404
        // `model_unavailable`.
        let _guard = crate::llm::env_guard();
        let prev_base = std::env::var("LOCAL_LLM_BASE_URL").ok();
        let prev_local_model = std::env::var("LOCAL_LLM_MODEL").ok();
        let prev_harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        let prev_harn_model = std::env::var("HARN_LLM_MODEL").ok();

        unsafe {
            std::env::set_var("LOCAL_LLM_BASE_URL", "http://127.0.0.1:11434");
            std::env::remove_var("LOCAL_LLM_MODEL");
            std::env::remove_var("HARN_LLM_PROVIDER");
            std::env::remove_var("HARN_LLM_MODEL");
        }

        // `anthropic/claude-sonnet-4-6` is a catalog alias that resolves
        // to the openrouter provider. With LOCAL_LLM_BASE_URL set the
        // pre-fix code would have returned "local".
        let opts = Some(crate::value::DictMap::from_iter([(
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("anthropic/claude-sonnet-4-6")),
        )]));
        assert_eq!(vm_resolve_provider(&opts), "openrouter");

        // An unknown id with no catalog hit still falls into "local"
        // so users with a custom local server keep working.
        let opts_unknown = Some(crate::value::DictMap::from_iter([(
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("my-custom-local-tag")),
        )]));
        assert_eq!(vm_resolve_provider(&opts_unknown), "local");

        unsafe {
            match prev_base {
                Some(value) => std::env::set_var("LOCAL_LLM_BASE_URL", value),
                None => std::env::remove_var("LOCAL_LLM_BASE_URL"),
            }
            match prev_local_model {
                Some(value) => std::env::set_var("LOCAL_LLM_MODEL", value),
                None => std::env::remove_var("LOCAL_LLM_MODEL"),
            }
            match prev_harn_provider {
                Some(value) => std::env::set_var("HARN_LLM_PROVIDER", value),
                None => std::env::remove_var("HARN_LLM_PROVIDER"),
            }
            match prev_harn_model {
                Some(value) => std::env::set_var("HARN_LLM_MODEL", value),
                None => std::env::remove_var("HARN_LLM_MODEL"),
            }
        }
    }

    #[test]
    fn vm_messages_to_json_preserves_tool_message_fields() {
        let message = VmValue::dict(crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("role"),
                VmValue::String(arcstr::ArcStr::from("tool")),
            ),
            (
                crate::value::intern_key("tool_call_id"),
                VmValue::String(arcstr::ArcStr::from("call_123")),
            ),
            (
                crate::value::intern_key("content"),
                VmValue::String(arcstr::ArcStr::from("ok")),
            ),
        ]));

        let json = vm_messages_to_json(&[message]).expect("message json");
        assert_eq!(json[0]["role"], "tool");
        assert_eq!(json[0]["tool_call_id"], "call_123");
        assert_eq!(json[0]["content"], "ok");
    }

    #[test]
    fn extract_llm_options_rejects_removed_transcript_key() {
        let _guard = crate::llm::env_guard();
        let prev_harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        let prev_harn_model = std::env::var("HARN_LLM_MODEL").ok();
        unsafe {
            std::env::set_var("HARN_LLM_PROVIDER", "mock");
            std::env::remove_var("HARN_LLM_MODEL");
        }

        let transcript = new_transcript_with(None, Vec::new(), None, None);
        let options = VmValue::dict(crate::value::DictMap::from_iter([(
            crate::value::intern_key("transcript"),
            transcript,
        )]));
        let err = extract_llm_options(&[
            VmValue::String(arcstr::ArcStr::from("")),
            VmValue::Nil,
            options,
        ])
        .expect_err("transcript option is rejected");
        let msg = match err {
            crate::value::VmError::Thrown(VmValue::String(s)) => s.to_string(),
            other => panic!("unexpected error: {other:?}"),
        };
        assert!(
            msg.contains("transcript") && msg.contains("session_id"),
            "got: {msg}"
        );

        unsafe {
            match prev_harn_provider {
                Some(value) => std::env::set_var("HARN_LLM_PROVIDER", value),
                None => std::env::remove_var("HARN_LLM_PROVIDER"),
            }
            match prev_harn_model {
                Some(value) => std::env::set_var("HARN_LLM_MODEL", value),
                None => std::env::remove_var("HARN_LLM_MODEL"),
            }
        }
    }

    #[test]
    fn model_tier_prefers_reachable_env_provider_and_model() {
        let _guard = crate::llm::env_guard();
        let prev_harn_model = std::env::var("HARN_LLM_MODEL").ok();
        let prev_harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        let prev_local_model = std::env::var("LOCAL_LLM_MODEL").ok();
        let prev_local_base = std::env::var("LOCAL_LLM_BASE_URL").ok();

        unsafe {
            std::env::set_var("HARN_LLM_MODEL", "gemma-4-e4b-it");
            std::env::set_var("HARN_LLM_PROVIDER", "local");
            std::env::set_var("LOCAL_LLM_MODEL", "gemma-4-e4b-it");
            std::env::set_var("LOCAL_LLM_BASE_URL", "http://127.0.0.1:8000");
        }

        let options = Some(crate::value::DictMap::from_iter([(
            crate::value::intern_key("model_tier"),
            VmValue::String(arcstr::ArcStr::from("small")),
        )]));
        let provider = vm_resolve_provider(&options);
        let resolved = vm_resolve_model(&options, &provider);

        unsafe {
            match prev_harn_model {
                Some(value) => std::env::set_var("HARN_LLM_MODEL", value),
                None => std::env::remove_var("HARN_LLM_MODEL"),
            }
            match prev_harn_provider {
                Some(value) => std::env::set_var("HARN_LLM_PROVIDER", value),
                None => std::env::remove_var("HARN_LLM_PROVIDER"),
            }
            match prev_local_model {
                Some(value) => std::env::set_var("LOCAL_LLM_MODEL", value),
                None => std::env::remove_var("LOCAL_LLM_MODEL"),
            }
            match prev_local_base {
                Some(value) => std::env::set_var("LOCAL_LLM_BASE_URL", value),
                None => std::env::remove_var("LOCAL_LLM_BASE_URL"),
            }
        }
        assert_eq!(provider, "local");
        assert_eq!(resolved, "gemma-4-e4b-it");
    }

    #[test]
    fn model_tier_falls_back_to_reachable_local_provider_when_default_alias_is_unavailable() {
        let _guard = crate::llm::env_guard();
        let prev_harn_model = std::env::var("HARN_LLM_MODEL").ok();
        let prev_harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        let prev_local_model = std::env::var("LOCAL_LLM_MODEL").ok();
        let prev_local_base = std::env::var("LOCAL_LLM_BASE_URL").ok();

        unsafe {
            std::env::remove_var("HARN_LLM_MODEL");
            std::env::remove_var("HARN_LLM_PROVIDER");
            std::env::set_var("LOCAL_LLM_MODEL", "gemma-4-e4b-it");
            std::env::set_var("LOCAL_LLM_BASE_URL", "http://127.0.0.1:8000");
        }

        let options = Some(crate::value::DictMap::from_iter([(
            crate::value::intern_key("model_tier"),
            VmValue::String(arcstr::ArcStr::from("small")),
        )]));
        let provider = vm_resolve_provider(&options);
        let resolved = vm_resolve_model(&options, &provider);

        unsafe {
            match prev_harn_model {
                Some(value) => std::env::set_var("HARN_LLM_MODEL", value),
                None => std::env::remove_var("HARN_LLM_MODEL"),
            }
            match prev_harn_provider {
                Some(value) => std::env::set_var("HARN_LLM_PROVIDER", value),
                None => std::env::remove_var("HARN_LLM_PROVIDER"),
            }
            match prev_local_model {
                Some(value) => std::env::set_var("LOCAL_LLM_MODEL", value),
                None => std::env::remove_var("LOCAL_LLM_MODEL"),
            }
            match prev_local_base {
                Some(value) => std::env::set_var("LOCAL_LLM_BASE_URL", value),
                None => std::env::remove_var("LOCAL_LLM_BASE_URL"),
            }
        }

        assert_eq!(provider, "local");
        assert_eq!(resolved, "gemma-4-e4b-it");
    }

    #[test]
    fn raw_env_model_is_accepted_when_env_provider_matches() {
        let _guard = crate::llm::env_guard();
        let prev_harn_model = std::env::var("HARN_LLM_MODEL").ok();
        let prev_harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();

        unsafe {
            std::env::set_var("HARN_LLM_MODEL", "google/gemma-4-31B-it");
            std::env::set_var("HARN_LLM_PROVIDER", "together");
        }

        let resolved = vm_resolve_model(&None, "together");

        unsafe {
            match prev_harn_model {
                Some(value) => std::env::set_var("HARN_LLM_MODEL", value),
                None => std::env::remove_var("HARN_LLM_MODEL"),
            }
            match prev_harn_provider {
                Some(value) => std::env::set_var("HARN_LLM_PROVIDER", value),
                None => std::env::remove_var("HARN_LLM_PROVIDER"),
            }
        }

        assert_eq!(resolved, "google/gemma-4-31B-it");
    }

    #[test]
    fn provider_auto_with_local_prefix_model_routes_to_ollama() {
        // `provider: "auto"` must fall through to inference. With a `local:`
        // model prefix that inference should resolve to Ollama rather than
        // the local OpenAI-compatible provider or Anthropic.
        let _guard = crate::llm::env_guard();
        let prev_harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        let prev_harn_model = std::env::var("HARN_LLM_MODEL").ok();
        let prev_base = std::env::var("LOCAL_LLM_BASE_URL").ok();
        unsafe {
            std::env::remove_var("HARN_LLM_PROVIDER");
            std::env::remove_var("HARN_LLM_MODEL");
            std::env::remove_var("LOCAL_LLM_BASE_URL");
        }

        let mut opts: crate::value::DictMap = crate::value::DictMap::new();
        opts.put_str("provider", "auto");
        opts.put_str("model", "local:gemma-4-e4b-it");
        assert_eq!(vm_resolve_provider(&Some(opts)), "ollama");

        // Case-insensitive: "AUTO" should behave the same.
        let mut opts2: crate::value::DictMap = crate::value::DictMap::new();
        opts2.put_str("provider", "AUTO");
        opts2.put_str("model", "local:foo/bar");
        assert_eq!(vm_resolve_provider(&Some(opts2)), "ollama");

        // Explicit non-auto provider still wins.
        let mut opts3: crate::value::DictMap = crate::value::DictMap::new();
        opts3.put_str("provider", "anthropic");
        opts3.put_str("model", "local:foo");
        assert_eq!(vm_resolve_provider(&Some(opts3)), "anthropic");

        unsafe {
            match prev_harn_provider {
                Some(v) => std::env::set_var("HARN_LLM_PROVIDER", v),
                None => std::env::remove_var("HARN_LLM_PROVIDER"),
            }
            match prev_harn_model {
                Some(v) => std::env::set_var("HARN_LLM_MODEL", v),
                None => std::env::remove_var("HARN_LLM_MODEL"),
            }
            match prev_base {
                Some(v) => std::env::set_var("LOCAL_LLM_BASE_URL", v),
                None => std::env::remove_var("LOCAL_LLM_BASE_URL"),
            }
        }
    }

    #[test]
    fn provider_auto_unknown_model_warns_and_uses_default_provider() {
        let _guard = crate::llm::env_guard();
        let prev_harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        let prev_harn_model = std::env::var("HARN_LLM_MODEL").ok();
        let prev_default_provider = std::env::var("HARN_DEFAULT_PROVIDER").ok();
        let prev_base = std::env::var("LOCAL_LLM_BASE_URL").ok();
        unsafe {
            std::env::remove_var("HARN_LLM_PROVIDER");
            std::env::remove_var("HARN_LLM_MODEL");
            std::env::remove_var("LOCAL_LLM_BASE_URL");
            std::env::set_var("HARN_DEFAULT_PROVIDER", "mock");
        }

        crate::events::clear_event_sinks();
        let sink = Rc::new(crate::events::CollectorSink::new());
        crate::events::add_event_sink(sink.clone());

        let opts = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("auto")),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from("unclassified-provider-model-for-test")),
            ),
        ]);

        assert_eq!(vm_resolve_provider(&Some(opts)), "mock");
        let logs = sink.logs.borrow();
        assert!(logs.iter().any(|event| {
            event.level == crate::events::EventLevel::Warn
                && event.category == "llm.provider"
                && event
                    .message
                    .contains("falling back to default provider 'mock'")
        }));

        crate::events::reset_event_sinks();
        unsafe {
            match prev_harn_provider {
                Some(v) => std::env::set_var("HARN_LLM_PROVIDER", v),
                None => std::env::remove_var("HARN_LLM_PROVIDER"),
            }
            match prev_harn_model {
                Some(v) => std::env::set_var("HARN_LLM_MODEL", v),
                None => std::env::remove_var("HARN_LLM_MODEL"),
            }
            match prev_default_provider {
                Some(v) => std::env::set_var("HARN_DEFAULT_PROVIDER", v),
                None => std::env::remove_var("HARN_DEFAULT_PROVIDER"),
            }
            match prev_base {
                Some(v) => std::env::set_var("LOCAL_LLM_BASE_URL", v),
                None => std::env::remove_var("LOCAL_LLM_BASE_URL"),
            }
        }
    }

    #[test]
    fn openrouter_provider_fallback_uses_current_valid_model_id() {
        let _guard = crate::llm::env_guard();
        let prev_harn_model = std::env::var("HARN_LLM_MODEL").ok();
        let prev_harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        let prev_local_model = std::env::var("LOCAL_LLM_MODEL").ok();
        let prev_local_base = std::env::var("LOCAL_LLM_BASE_URL").ok();
        unsafe {
            std::env::remove_var("HARN_LLM_MODEL");
            std::env::remove_var("HARN_LLM_PROVIDER");
            std::env::remove_var("LOCAL_LLM_MODEL");
            std::env::remove_var("LOCAL_LLM_BASE_URL");
        }

        let resolved = vm_resolve_model(&None, "openrouter");

        unsafe {
            match prev_harn_model {
                Some(value) => std::env::set_var("HARN_LLM_MODEL", value),
                None => std::env::remove_var("HARN_LLM_MODEL"),
            }
            match prev_harn_provider {
                Some(value) => std::env::set_var("HARN_LLM_PROVIDER", value),
                None => std::env::remove_var("HARN_LLM_PROVIDER"),
            }
            match prev_local_model {
                Some(value) => std::env::set_var("LOCAL_LLM_MODEL", value),
                None => std::env::remove_var("LOCAL_LLM_MODEL"),
            }
            match prev_local_base {
                Some(value) => std::env::set_var("LOCAL_LLM_BASE_URL", value),
                None => std::env::remove_var("LOCAL_LLM_BASE_URL"),
            }
        }

        assert_eq!(resolved, "anthropic/claude-sonnet-4.6");
    }

    #[test]
    fn session_pinned_model_overrides_env_default_resolution() {
        // ACP `session/set_config_option(configId="model")` writes a
        // per-session pin via `agent_sessions::set_pinned_model`. The
        // resolvers must surface that pin in place of the env-driven
        // default so the next `llm_call` honours it without each
        // builtin threading the session id manually.
        let _guard = crate::llm::env_guard();
        let prev_harn_model = std::env::var("HARN_LLM_MODEL").ok();
        let prev_harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        unsafe {
            std::env::set_var("HARN_LLM_MODEL", "gpt-4o-mini");
            std::env::set_var("HARN_LLM_PROVIDER", "openai");
        }

        crate::agent_sessions::reset_session_store();
        let id = crate::agent_sessions::open_or_create(Some("pinned-resolver-session".to_string()));
        crate::agent_sessions::set_pinned_model(&id, Some("claude-sonnet-4-6".to_string()))
            .expect("set pinned model");
        let _session_guard = crate::agent_sessions::enter_current_session(id);

        let provider = vm_resolve_provider(&None);
        let model = vm_resolve_model(&None, &provider);

        // Drop the guard before mutating shared env to keep cleanup
        // local even if the assertion below fails.
        drop(_session_guard);
        crate::agent_sessions::reset_session_store();
        unsafe {
            match prev_harn_model {
                Some(value) => std::env::set_var("HARN_LLM_MODEL", value),
                None => std::env::remove_var("HARN_LLM_MODEL"),
            }
            match prev_harn_provider {
                Some(value) => std::env::set_var("HARN_LLM_PROVIDER", value),
                None => std::env::remove_var("HARN_LLM_PROVIDER"),
            }
        }

        assert_eq!(provider, "anthropic", "session pin should reroute provider");
        assert_eq!(
            model, "claude-sonnet-4-6",
            "session pin should reroute model"
        );
    }

    #[test]
    fn explicit_call_site_model_wins_over_session_pin() {
        let _guard = crate::llm::env_guard();
        let prev_harn_model = std::env::var("HARN_LLM_MODEL").ok();
        let prev_harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        unsafe {
            std::env::remove_var("HARN_LLM_MODEL");
            std::env::remove_var("HARN_LLM_PROVIDER");
        }

        crate::agent_sessions::reset_session_store();
        let id =
            crate::agent_sessions::open_or_create(Some("explicit-override-session".to_string()));
        crate::agent_sessions::set_pinned_model(&id, Some("claude-sonnet-4-6".to_string()))
            .expect("set pinned model");
        let _session_guard = crate::agent_sessions::enter_current_session(id);

        // Call-site `model:` option must win — scripts opting into a
        // specific model should not be silently overridden by an ACP
        // pin meant only for "no-option" calls.
        let mut explicit_opts: crate::value::DictMap = crate::value::DictMap::new();
        explicit_opts.put_str("model", "gpt-4o-mini");
        explicit_opts.put_str("provider", "openai");
        let opts = Some(explicit_opts);
        let provider = vm_resolve_provider(&opts);
        let model = vm_resolve_model(&opts, &provider);

        drop(_session_guard);
        crate::agent_sessions::reset_session_store();
        unsafe {
            match prev_harn_model {
                Some(value) => std::env::set_var("HARN_LLM_MODEL", value),
                None => std::env::remove_var("HARN_LLM_MODEL"),
            }
            match prev_harn_provider {
                Some(value) => std::env::set_var("HARN_LLM_PROVIDER", value),
                None => std::env::remove_var("HARN_LLM_PROVIDER"),
            }
        }

        assert_eq!(provider, "openai");
        assert_eq!(model, "gpt-4o-mini");
    }
}
