use super::*;

#[test]
fn model_role_defaults_fill_missing_llm_options() {
    let _guard = crate::llm::env_guard();
    clear_merge_role_env();
    let mut overlay = ProvidersConfig::default();
    overlay.model_roles.insert(
        "merge".to_string(),
        std::collections::BTreeMap::from_iter([
            (
                "provider".to_string(),
                toml::Value::String("mock".to_string()),
            ),
            (
                "model".to_string(),
                toml::Value::String("mock-merge".to_string()),
            ),
            ("max_tokens".to_string(), toml::Value::Integer(4096)),
            ("temperature".to_string(), toml::Value::Float(0.0)),
        ]),
    );
    crate::llm_config::set_user_overrides(Some(overlay));

    let opts = extract_with_options(model_role_options("merge")).expect("options");

    assert_eq!(opts.provider, "mock");
    assert_eq!(opts.model, "mock-merge");
    assert_eq!(opts.max_tokens, 4096);
    assert_eq!(opts.temperature, Some(0.0));

    crate::llm_config::clear_user_overrides();
    clear_merge_role_env();
}

#[test]
fn explicit_options_win_over_model_role_defaults() {
    let _guard = crate::llm::env_guard();
    clear_merge_role_env();
    let mut overlay = ProvidersConfig::default();
    overlay.model_roles.insert(
        "merge".to_string(),
        std::collections::BTreeMap::from_iter([
            (
                "provider".to_string(),
                toml::Value::String("mock".to_string()),
            ),
            (
                "model".to_string(),
                toml::Value::String("mock-merge".to_string()),
            ),
            ("max_tokens".to_string(), toml::Value::Integer(4096)),
        ]),
    );
    crate::llm_config::set_user_overrides(Some(overlay));

    let mut options = model_role_options("merge");
    options.put_str("model", "mock-explicit");
    options.insert(crate::value::intern_key("max_tokens"), VmValue::Int(512));
    let opts = extract_with_options(options).expect("options");

    assert_eq!(opts.provider, "mock");
    assert_eq!(opts.model, "mock-explicit");
    assert_eq!(opts.max_tokens, 512);

    crate::llm_config::clear_user_overrides();
    clear_merge_role_env();
}

#[test]
fn merge_model_role_has_env_overrides() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();
    clear_merge_role_env();
    std::env::set_var("HARN_LLM_MERGE_PROVIDER", "mock");
    std::env::set_var("HARN_LLM_MERGE_MODEL", "mock-env-merge");

    let opts = extract_with_options(model_role_options("merge")).expect("options");

    assert_eq!(opts.provider, "mock");
    assert_eq!(opts.model, "mock-env-merge");

    clear_merge_role_env();
    crate::llm_config::clear_user_overrides();
}

#[test]
fn model_role_aliases_do_not_override_exact_role_defaults() {
    let _guard = crate::llm::env_guard();
    clear_merge_role_env();
    let mut overlay = ProvidersConfig::default();
    overlay.model_roles.insert(
        "fast_apply".to_string(),
        std::collections::BTreeMap::from_iter([
            (
                "provider".to_string(),
                toml::Value::String("mock".to_string()),
            ),
            (
                "model".to_string(),
                toml::Value::String("mock-fast-apply".to_string()),
            ),
        ]),
    );
    overlay.model_roles.insert(
        "merge".to_string(),
        std::collections::BTreeMap::from_iter([
            (
                "provider".to_string(),
                toml::Value::String("mock".to_string()),
            ),
            (
                "model".to_string(),
                toml::Value::String("mock-merge".to_string()),
            ),
        ]),
    );
    crate::llm_config::set_user_overrides(Some(overlay));

    let merge_opts = extract_with_options(model_role_options("merge")).expect("merge options");
    let fast_apply_opts =
        extract_with_options(model_role_options("fast_apply")).expect("fast_apply options");

    assert_eq!(merge_opts.model, "mock-merge");
    assert_eq!(fast_apply_opts.model, "mock-fast-apply");

    crate::llm_config::clear_user_overrides();
    clear_merge_role_env();
}

#[test]
fn model_role_env_aliases_do_not_override_exact_role_env() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();
    clear_merge_role_env();
    std::env::set_var("HARN_LLM_FAST_APPLY_PROVIDER", "mock");
    std::env::set_var("HARN_LLM_FAST_APPLY_MODEL", "mock-env-fast-apply");
    std::env::set_var("HARN_LLM_MERGE_PROVIDER", "mock");
    std::env::set_var("HARN_LLM_MERGE_MODEL", "mock-env-merge");

    let merge_opts = extract_with_options(model_role_options("merge")).expect("merge options");
    let fast_apply_opts =
        extract_with_options(model_role_options("fast_apply")).expect("fast_apply options");

    assert_eq!(merge_opts.model, "mock-env-merge");
    assert_eq!(fast_apply_opts.model, "mock-env-fast-apply");

    clear_merge_role_env();
    crate::llm_config::clear_user_overrides();
}

#[test]
fn model_role_config_keys_normalize_like_call_options() {
    let _guard = crate::llm::env_guard();
    clear_merge_role_env();
    let mut overlay = ProvidersConfig::default();
    overlay.model_roles.insert(
        "fast-apply".to_string(),
        std::collections::BTreeMap::from_iter([
            (
                "provider".to_string(),
                toml::Value::String("mock".to_string()),
            ),
            (
                "model".to_string(),
                toml::Value::String("mock-fast-apply".to_string()),
            ),
        ]),
    );
    crate::llm_config::set_user_overrides(Some(overlay));

    let opts = extract_with_options(model_role_options("fast_apply")).expect("options");

    assert_eq!(opts.provider, "mock");
    assert_eq!(opts.model, "mock-fast-apply");

    crate::llm_config::clear_user_overrides();
    clear_merge_role_env();
}
