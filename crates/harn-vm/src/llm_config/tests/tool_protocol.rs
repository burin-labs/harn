use super::*;

#[test]
fn automatic_tier_routes_have_an_explicit_viable_tool_protocol() {
    let audit = crate::llm::capabilities::audit_builtin_catalogued_chat_model_tool_capabilities();
    for alias in ["frontier", "mid", "small"] {
        let (model, provider) = resolve_tier_model(alias, None)
            .unwrap_or_else(|| panic!("tier alias `{alias}` must resolve"));
        let entry = model_catalog_entry(&model).unwrap_or_else(|| {
            panic!("tier alias `{alias}` -> `{provider}/{model}` must be catalogued")
        });
        assert_eq!(
            entry.provider, provider,
            "tier alias `{alias}` must resolve to its catalogued provider"
        );
        assert!(
            !entry.deprecated && entry.availability == ModelAvailability::Serverless,
            "tier alias `{alias}` must be an active serverless route"
        );
        assert!(
            entry.pricing.is_some(),
            "tier alias `{alias}` must remain in the capability audit's catalog scope"
        );
        assert!(
            entry
                .capabilities
                .iter()
                .any(|capability| capability == "tools"),
            "tier alias `{alias}` must explicitly claim tool support"
        );
        assert!(
            !audit
                .gaps
                .iter()
                .any(|gap| gap.provider == provider && gap.model == model),
            "tier alias `{alias}` must have explicit native_tools and preferred_tool_format facts"
        );

        let caps = crate::llm::capabilities::lookup(&provider, &model);
        assert!(
            caps.native_tools || caps.text_tool_wire_format_supported,
            "tier alias `{alias}` must expose at least one declared tool channel"
        );
        let preferred = caps
            .preferred_tool_format
            .as_deref()
            .unwrap_or_else(|| panic!("tier alias `{alias}` must declare a preferred tool format"));
        assert!(
            crate::llm::capabilities::no_viable_tool_channel(&provider, &model).is_none(),
            "tier alias `{alias}` must not auto-route onto a tool-dead route"
        );
        assert!(
            crate::llm::capabilities::validate_tool_format(&provider, &model, preferred)
                .correction
                .is_none(),
            "tier alias `{alias}` preferred tool format `{preferred}` must be viable"
        );
    }
}
