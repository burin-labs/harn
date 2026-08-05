//! Tests for provider-rule matching, `extends` fall-through, and
//! capability resolution. Split out of `rule.rs` to keep that module under
//! the source-length cap.

use harn_glob::match_name as glob_match;

use super::lookup::parse_capabilities_toml;
use super::model::{Capabilities, WireDialect};
use super::rule::lookup_with;

#[test]
fn glob_match_substring() {
    assert!(glob_match("*gpt*", "openai/gpt-5.4"));
    assert!(glob_match("*claude*", "anthropic/claude-opus-4-7"));
    assert!(!glob_match("*xyz*", "openai/gpt-5.4"));
}

/// Resolve capabilities for a synthetic provider whose rules come entirely
/// from `src`: the parsed file is passed as the builtin base with no user
/// layer, so no shipped rule interferes with the `extends` assertions.
fn extends_caps(src: &str) -> Capabilities {
    let file = parse_capabilities_toml(src).expect("test capabilities toml parses");
    lookup_with("testprov", "test-model", &file, None)
}

#[test]
fn extends_rule_fills_unset_fields_from_later_matching_rule() {
    // Rule 1 opts into `extends` and sets only native_tools; rule 2 (lower
    // precedence, same match) supplies the fields the chain left unset.
    let caps = extends_caps(
        r#"
[[provider.testprov]]
model_match = "test-*"
extends = true
native_tools = true

[[provider.testprov]]
model_match = "test-*"
vision = true
message_wire_format = "anthropic"
"#,
    );
    assert!(caps.native_tools, "field from the extends rule applies");
    assert!(
        caps.vision,
        "unset field filled from the later matching rule"
    );
    assert_eq!(caps.message_wire_format, WireDialect::Anthropic);
}

#[test]
fn non_extends_rule_terminates_resolution_unchanged() {
    // Without `extends`, the first match wins outright and the later
    // rule's vision never applies — the pre-`extends` first-match-wins
    // behavior is preserved.
    let caps = extends_caps(
        r#"
[[provider.testprov]]
model_match = "test-*"
native_tools = true

[[provider.testprov]]
model_match = "test-*"
vision = true
"#,
    );
    assert!(caps.native_tools);
    assert!(
        !caps.vision,
        "a non-extends first match must not absorb later rules"
    );
}

#[test]
fn extends_rule_does_not_override_explicitly_set_field() {
    // The higher-precedence extends rule's explicit native_tools = true
    // wins; the later rule only fills fields the chain left unset, so its
    // native_tools = false is ignored while its vision still applies.
    let caps = extends_caps(
        r#"
[[provider.testprov]]
model_match = "test-*"
extends = true
native_tools = true

[[provider.testprov]]
model_match = "test-*"
native_tools = false
vision = true
"#,
    );
    assert!(
        caps.native_tools,
        "the extends rule's explicit value is not overridden by a lower rule"
    );
    assert!(caps.vision, "still fills the field the chain left unset");
}

#[test]
fn extends_chain_falls_through_to_provider_defaults() {
    // An unterminated extends chain (no later matching rule) fills its
    // remaining gaps from provider defaults.
    let caps = extends_caps(
        r#"
[provider_defaults.testprov]
seed_supported = true

[[provider.testprov]]
model_match = "test-*"
extends = true
native_tools = true
"#,
    );
    assert!(caps.native_tools, "field from the extends rule applies");
    assert!(
        caps.seed_supported,
        "unset field filled from provider defaults"
    );
}
