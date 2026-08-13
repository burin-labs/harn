use super::*;

mod nested_budget_tests {
    use crate::orchestration::{
        clear_execution_policy_stacks, current_execution_policy, pop_execution_policy,
        push_execution_policy, CapabilityPolicy,
    };
    use crate::value::{VmDictExt, VmError, VmValue};

    use super::super::super::{build_nested_budget_denial, install_session_nested_budget};
    use super::vm_to_json;

    fn policy_value(policy: &CapabilityPolicy) -> VmValue {
        crate::stdlib::json_to_vm_value(&serde_json::to_value(policy).unwrap())
    }

    fn empty_session_id() -> String {
        format!("test_session_{}", uuid::Uuid::now_v7())
    }

    #[test]
    fn install_session_nested_budget_rejects_when_parent_is_zero() {
        clear_execution_policy_stacks();
        let parent = CapabilityPolicy {
            recursion_limit: Some(0),
            ..Default::default()
        };
        push_execution_policy(parent);

        let opts_map = crate::value::DictMap::new();
        let session_id = empty_session_id();
        let error = install_session_nested_budget(&opts_map, &session_id).unwrap_err();
        match error {
            VmError::CategorizedError { message, category } => {
                assert_eq!(category.as_str(), "budget_exceeded");
                assert!(message.contains("agent_loop"), "missing kind: {message}");
                assert!(message.contains(&session_id), "missing label: {message}");
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
        pop_execution_policy();
    }

    #[test]
    fn install_session_nested_budget_decrements_when_parent_has_room() {
        clear_execution_policy_stacks();
        push_execution_policy(CapabilityPolicy {
            recursion_limit: Some(3),
            ..Default::default()
        });

        let opts_map = crate::value::DictMap::new();
        let guard = install_session_nested_budget(&opts_map, "child").unwrap();
        assert_eq!(guard.parent_limit, Some(3));
        assert_eq!(guard.child_limit, Some(2));
        assert_eq!(current_execution_policy().unwrap().recursion_limit, Some(2));
        drop(guard);
        pop_execution_policy();
    }

    #[test]
    fn install_session_nested_budget_reads_kind_and_label_from_options() {
        clear_execution_policy_stacks();
        push_execution_policy(CapabilityPolicy {
            recursion_limit: Some(0),
            ..Default::default()
        });

        let mut opts_map = crate::value::DictMap::new();
        opts_map.put_str("_nested_kind", "sub_agent_run");
        opts_map.put_str("_nested_label", "research-worker");
        let error = install_session_nested_budget(&opts_map, "ignored").unwrap_err();
        match error {
            VmError::CategorizedError { message, .. } => {
                assert!(
                    message.contains("sub_agent_run"),
                    "kind not surfaced: {message}"
                );
                assert!(
                    message.contains("research-worker"),
                    "label not surfaced: {message}"
                );
            }
            other => panic!("unexpected variant: {other:?}"),
        }
        pop_execution_policy();
    }

    #[test]
    fn install_session_nested_budget_intersects_requested_policy() {
        clear_execution_policy_stacks();
        push_execution_policy(CapabilityPolicy {
            recursion_limit: Some(10),
            ..Default::default()
        });

        let mut opts_map = crate::value::DictMap::new();
        opts_map.insert(
            crate::value::intern_key("policy"),
            policy_value(&CapabilityPolicy {
                recursion_limit: Some(1),
                ..Default::default()
            }),
        );
        let guard = install_session_nested_budget(&opts_map, "child").unwrap();
        assert_eq!(guard.child_limit, Some(1));
        drop(guard);
        pop_execution_policy();
    }

    #[test]
    fn build_nested_budget_denial_carries_budget_exceeded_category() {
        let error = VmError::CategorizedError {
            message: "nested execution budget exhausted before sub_agent_run: research-worker"
                .to_string(),
            category: crate::value::ErrorCategory::BudgetExceeded,
        };
        let result = build_nested_budget_denial("session-x", "go", &error);
        let json = vm_to_json(&result);
        assert_eq!(json["final_status"], "budget_exhausted");
        assert_eq!(json["stop_reason"], "nested_execution_budget_exhausted");
        assert_eq!(json["error"]["category"], "budget_exceeded");
        assert!(json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("research-worker"));
        assert_eq!(json["session_id"], "session-x");
        assert_eq!(json["task"], "go");
    }
}

#[test]
fn session_totals_expose_cumulative_input_and_output_token_split() {
    reset_agent_session_host_state();
    let session_id = crate::agent_sessions::open_or_create(Some("totals-token-split".to_string()));
    seed_host_session_provider_model(&session_id, "anthropic", "claude-sonnet-4-5");
    super::super::with_session(&session_id, "totals-token-split", |session| {
        session.input_tokens = 140;
        session.output_tokens = 40;
        session.cache_read_tokens = 90;
        session.cache_write_tokens = 12;
        session.tokens_used = 180;
        Ok(())
    })
    .expect("seed accumulator");
    let totals = super::super::host_agent_session_totals_builtin(
        &[crate::value::VmValue::string(&session_id)],
        &mut String::new(),
    )
    .expect("totals");
    let json = vm_to_json(&totals);
    assert_eq!(json["input_tokens"], 140);
    assert_eq!(json["output_tokens"], 40);
    assert_eq!(json["tokens_used"], 180);
    assert_eq!(json["cache_read_tokens"], 90);
    assert_eq!(json["cache_write_tokens"], 12);
    assert_eq!(json.get("cache_creation_input_tokens"), None);
}

#[test]
fn record_usage_accumulates_cache_tokens_from_top_level_and_nested_usage() {
    reset_agent_session_host_state();
    let session_id =
        crate::agent_sessions::open_or_create(Some("totals-cache-token-split".to_string()));
    seed_host_session_provider_model(&session_id, "anthropic", "claude-sonnet-4-5");

    let first = json_to_vm(&serde_json::json!({
        "provider": "anthropic",
        "model": "claude-sonnet-4-5",
        "input_tokens": 100,
        "output_tokens": 20,
        "cache_read_tokens": 70,
        "cache_creation_input_tokens": 10,
    }));
    super::super::host_agent_session_record_usage_builtin(
        &[crate::value::VmValue::string(&session_id), first],
        &mut String::new(),
    )
    .expect("first record_usage");

    let second = json_to_vm(&serde_json::json!({
        "provider": "anthropic",
        "model": "claude-sonnet-4-5",
        "llm": {"input_tokens": 80, "output_tokens": 15},
        "usage": {"prompt_tokens_details": {"cached_tokens": 30, "cache_write_tokens": 5}},
    }));
    let returned = super::super::host_agent_session_record_usage_builtin(
        &[crate::value::VmValue::string(&session_id), second],
        &mut String::new(),
    )
    .expect("second record_usage");
    let returned_json = vm_to_json(&returned);
    assert_eq!(returned_json["cache_read_tokens"], 100);
    assert_eq!(returned_json["cache_write_tokens"], 15);

    let totals = super::super::host_agent_session_totals_builtin(
        &[crate::value::VmValue::string(&session_id)],
        &mut String::new(),
    )
    .expect("totals");
    let json = vm_to_json(&totals);
    assert_eq!(json["input_tokens"], 180);
    assert_eq!(json["output_tokens"], 35);
    assert_eq!(json["cache_read_tokens"], 100);
    assert_eq!(json["cache_write_tokens"], 15);
}

#[test]
fn record_usage_preserves_unknown_cost_and_reports_the_known_floor() {
    reset_agent_session_host_state();
    let session_id =
        crate::agent_sessions::open_or_create(Some("totals-cost-certainty".to_string()));
    seed_host_session_provider_model(&session_id, "fireworks", "unpriced/model");

    let unknown = json_to_vm(&serde_json::json!({
        "provider": "fireworks",
        "model": "unpriced/model",
        "usage": {
            "input_tokens": 0,
            "output_tokens": 0,
            "cost_usd": null,
            "accounting_status": "unknown"
        }
    }));
    let unknown_totals = super::super::host_agent_session_record_usage_builtin(
        &[crate::value::VmValue::string(&session_id), unknown],
        &mut String::new(),
    )
    .expect("unknown usage");
    let unknown_json = vm_to_json(&unknown_totals);
    assert_eq!(unknown_json["cost_usd"], serde_json::Value::Null);
    assert_eq!(unknown_json["known_cost_usd"], 0.0);
    assert_eq!(unknown_json["unpriced_calls"], 1);
    assert_eq!(unknown_json["usage_unknown_calls"], 1);

    let priced = json_to_vm(&serde_json::json!({
        "provider": "managed",
        "model": "receipt-priced",
        "usage": {
            "input_tokens": 10,
            "output_tokens": 4,
            "cost_usd": 0.0025,
            "accounting_status": "reported"
        }
    }));
    let mixed_totals = super::super::host_agent_session_record_usage_builtin(
        &[crate::value::VmValue::string(&session_id), priced],
        &mut String::new(),
    )
    .expect("priced usage after unknown");
    let mixed_json = vm_to_json(&mixed_totals);
    assert_eq!(mixed_json["cost_usd"], serde_json::Value::Null);
    assert_eq!(mixed_json["known_cost_usd"], 0.0025);
    assert_eq!(mixed_json["unpriced_calls"], 1);
    assert_eq!(mixed_json["usage_unknown_calls"], 1);
}
