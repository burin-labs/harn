mod blocks;
mod messages;
mod opt_get;
mod options;
mod provider;
mod transcript;

use std::collections::BTreeMap;

use crate::value::VmValue;

pub(crate) use messages::{vm_add_role_message, vm_message_value, vm_messages_to_json};
pub(crate) use opt_get::{opt_bool, opt_float, opt_int, opt_str};
pub(crate) use options::{
    expects_structured_output, extract_json, extract_llm_options, opt_str_list,
};
#[cfg(test)]
pub(crate) use provider::reset_provider_key_cache;
pub use provider::resolve_api_key;
pub(crate) use provider::{vm_resolve_model, vm_resolve_provider, ResolvedProvider};
pub(crate) use transcript::{
    is_transcript_value, new_transcript_with, new_transcript_with_events,
    normalize_transcript_asset, transcript_asset_list, transcript_event,
    transcript_events_from_messages, transcript_id, transcript_message_list,
    transcript_summary_text, transcript_to_vm_with_events,
};

pub(super) const TRANSCRIPT_TYPE: &str = "transcript";
pub(super) const TRANSCRIPT_ASSET_TYPE: &str = "transcript_asset";
pub(super) const TRANSCRIPT_VERSION: i64 = 2;

/// Convert a VmValue dict to serde_json::Value for API payloads.
pub(crate) fn vm_value_dict_to_json(dict: &BTreeMap<String, VmValue>) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in dict {
        map.insert(k.clone(), vm_value_to_json(v));
    }
    serde_json::Value::Object(map)
}

pub fn vm_value_to_json(val: &VmValue) -> serde_json::Value {
    match val {
        VmValue::Int(i) => serde_json::json!(i),
        VmValue::Float(f) => serde_json::json!(f),
        VmValue::String(s) => serde_json::json!(s.as_ref()),
        VmValue::Bool(b) => serde_json::json!(b),
        VmValue::Nil => serde_json::Value::Null,
        VmValue::List(list) => {
            serde_json::Value::Array(list.iter().map(vm_value_to_json).collect())
        }
        VmValue::Dict(d) => vm_value_dict_to_json(d),
        _ => serde_json::json!(val.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    #[test]
    fn local_provider_is_selected_when_local_base_url_and_model_are_set() {
        // Share the crate-wide LLM env lock so this test cannot race with
        // sibling modules (e.g. llm::api streaming classification tests) that
        // also mutate LOCAL_LLM_BASE_URL.
        let _guard = crate::llm::env_lock().lock().expect("env lock");
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
        reset_provider_key_cache();

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
        reset_provider_key_cache();
    }

    #[test]
    fn vm_messages_to_json_preserves_tool_message_fields() {
        let message = VmValue::Dict(Rc::new(BTreeMap::from([
            ("role".to_string(), VmValue::String(Rc::from("tool"))),
            (
                "tool_call_id".to_string(),
                VmValue::String(Rc::from("call_123")),
            ),
            ("content".to_string(), VmValue::String(Rc::from("ok"))),
        ])));

        let json = vm_messages_to_json(&[message]).expect("message json");
        assert_eq!(json[0]["role"], "tool");
        assert_eq!(json[0]["tool_call_id"], "call_123");
        assert_eq!(json[0]["content"], "ok");
    }

    #[test]
    fn extract_llm_options_rejects_removed_transcript_key() {
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        let prev_harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        let prev_harn_model = std::env::var("HARN_LLM_MODEL").ok();
        unsafe {
            std::env::set_var("HARN_LLM_PROVIDER", "mock");
            std::env::remove_var("HARN_LLM_MODEL");
        }

        let transcript = new_transcript_with(None, Vec::new(), None, None);
        let options = VmValue::Dict(Rc::new(BTreeMap::from([(
            "transcript".to_string(),
            transcript,
        )])));
        let err = extract_llm_options(&[VmValue::String(Rc::from("")), VmValue::Nil, options])
            .err()
            .expect("transcript option is rejected");
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
        let _guard = crate::llm::env_lock().lock().expect("env lock");
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
        reset_provider_key_cache();

        let options = Some(BTreeMap::from([(
            "model_tier".to_string(),
            VmValue::String(Rc::from("small")),
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
        let _guard = crate::llm::env_lock().lock().expect("env lock");
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
        reset_provider_key_cache();

        let options = Some(BTreeMap::from([(
            "model_tier".to_string(),
            VmValue::String(Rc::from("small")),
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
        let _guard = crate::llm::env_lock().lock().expect("env lock");
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
    fn provider_auto_with_local_prefix_model_routes_to_local() {
        // `provider: "auto"` must fall through to inference. With a `local:`
        // model prefix that inference should resolve to the local provider
        // rather than anthropic/ollama.
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        let prev_harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        let prev_harn_model = std::env::var("HARN_LLM_MODEL").ok();
        let prev_base = std::env::var("LOCAL_LLM_BASE_URL").ok();
        unsafe {
            std::env::remove_var("HARN_LLM_PROVIDER");
            std::env::remove_var("HARN_LLM_MODEL");
            std::env::remove_var("LOCAL_LLM_BASE_URL");
        }
        reset_provider_key_cache();

        let mut opts: BTreeMap<String, VmValue> = BTreeMap::new();
        opts.insert("provider".to_string(), VmValue::String(Rc::from("auto")));
        opts.insert(
            "model".to_string(),
            VmValue::String(Rc::from("local:gemma-4-e4b-it")),
        );
        assert_eq!(vm_resolve_provider(&Some(opts)), "local");

        // Case-insensitive: "AUTO" should behave the same.
        let mut opts2: BTreeMap<String, VmValue> = BTreeMap::new();
        opts2.insert("provider".to_string(), VmValue::String(Rc::from("AUTO")));
        opts2.insert(
            "model".to_string(),
            VmValue::String(Rc::from("local:foo/bar")),
        );
        assert_eq!(vm_resolve_provider(&Some(opts2)), "local");

        // Explicit non-auto provider still wins.
        let mut opts3: BTreeMap<String, VmValue> = BTreeMap::new();
        opts3.insert(
            "provider".to_string(),
            VmValue::String(Rc::from("anthropic")),
        );
        opts3.insert("model".to_string(), VmValue::String(Rc::from("local:foo")));
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
        reset_provider_key_cache();
    }
}
