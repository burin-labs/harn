use super::*;

#[test]
fn measured_tool_format_beats_catalog_but_not_explicit_alias_pin() {
    clear_user_overrides();
    clear_runtime_provider_endpoint_overrides();
    let config = ProvidersConfig::default();
    assert_eq!(
        default_tool_format_with_config_and_fitness(
            &config,
            "gpt-5.4-mini",
            "openai",
            Some("text".to_string()),
        ),
        "text"
    );

    let pinned = parse_config_toml(
        "[aliases.calibrated]\nid = \"gpt-5.4-mini\"\nprovider = \"openai\"\ntool_format = \"json\"\n",
    )
    .expect("alias config parses");
    assert_eq!(
        default_tool_format_with_config_and_fitness(
            &pinned,
            "calibrated",
            "openai",
            Some("text".to_string()),
        ),
        "json"
    );
}
