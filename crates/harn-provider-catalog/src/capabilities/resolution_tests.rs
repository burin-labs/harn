use super::{Capabilities, CapabilitiesFile, CapabilityRoute, CapabilityRules};

#[test]
fn unmatched_route_preserves_the_complete_restrictive_default() {
    let base = CapabilitiesFile::default();
    let result = CapabilityRules::new(&base, None).resolve(CapabilityRoute {
        provider: "unknown",
        model: "unknown",
        responses_api: false,
    });
    assert!(!result.matched());
    assert!(!result.has_defaults());
    assert_eq!(result.capabilities(), Capabilities::default());
}

#[test]
fn explicit_overlay_extends_builtin_and_preserves_authored_provenance() {
    let base: CapabilitiesFile = toml::from_str(
        r#"
[provider_family]
proxy = "openai"
[provider_defaults.proxy]
seed_supported = true
[[provider.openai]]
model_match = "gpt-*"
native_tools = true
vision = true
"#,
    )
    .unwrap();
    let overlay = toml::from_str(
        r#"
[[provider.proxy]]
model_match = "gpt-*"
extends = true
vision = false
"#,
    )
    .unwrap();
    let rules = CapabilityRules::new(&base, Some(&overlay));
    let result = rules.resolve(CapabilityRoute {
        provider: "proxy",
        model: "gpt-5.4",
        responses_api: false,
    });
    assert!(result.matched());
    let caps = result.capabilities();
    assert!(!caps.vision);
    assert!(caps.native_tools);
    assert!(caps.seed_supported);
    assert_eq!(result.provenance.len(), 2);
    assert!(result.provenance[0].contains("user capability overlay"));
    assert!(result.provenance[1].contains("Harn built-in"));
    let unknown = rules.resolve(CapabilityRoute {
        provider: "proxy",
        model: "unknown",
        responses_api: false,
    });
    assert!(!unknown.matched());
    assert!(unknown.provenance.is_empty());
    let caps = unknown.capabilities();
    assert!(!caps.native_tools);
    assert!(caps.seed_supported);
}

#[test]
fn unmatched_mock_is_distinct_from_a_matched_restrictive_route() {
    let base = toml::from_str(
        r#"
[[provider.mock]]
model_match = "restricted"
native_tools = false
vision = false
[[provider.mock]]
model_match = "capable"
native_tools = true
"#,
    )
    .unwrap();
    let rules = CapabilityRules::new(&base, None);
    for (model, matched, native_tools) in [
        ("restricted", true, false),
        ("capable", true, true),
        ("unknown", false, false),
    ] {
        let result = rules.resolve(CapabilityRoute {
            provider: "mock",
            model,
            responses_api: false,
        });
        assert_eq!(result.matched(), matched, "{model}");
        assert_eq!(result.capabilities().native_tools, native_tools, "{model}");
    }
}

#[test]
fn responses_transport_and_hosted_authority_follow_the_explicit_route() {
    let base = toml::from_str(
        r#"
[provider_family]
proxy = "openai"
[[provider.openai]]
model_match = "gpt-*"
responses_api = true
hosted_tools = ["web_search"]
remote_mcp = true
conversation_state = true
compaction = true
background_mode = true
"#,
    )
    .unwrap();
    let rules = CapabilityRules::new(&base, None);
    for (provider, feature, responses, hosted) in [
        ("openai", false, true, true),
        ("mock", false, true, true),
        ("proxy", false, false, false),
        ("proxy", true, true, false),
    ] {
        let result = rules.resolve(CapabilityRoute {
            provider,
            model: "gpt-5.4",
            responses_api: feature,
        });
        assert!(result.matched(), "{provider}");
        let caps = result.capabilities();
        assert_eq!(caps.responses_api, responses, "{provider}:{feature}");
        assert_eq!(
            !caps.hosted_tools.is_empty(),
            hosted,
            "{provider}:{feature}"
        );
        assert_eq!(caps.remote_mcp, hosted, "{provider}:{feature}");
        assert_eq!(caps.conversation_state, hosted, "{provider}:{feature}");
        assert_eq!(caps.compaction, hosted, "{provider}:{feature}");
        assert_eq!(caps.background_mode, hosted, "{provider}:{feature}");
    }
}
