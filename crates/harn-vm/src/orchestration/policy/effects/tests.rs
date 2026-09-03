use super::*;

#[test]
fn harness_net_call_yields_net_effect() {
    let source = r#"fn main(harness: Harness) { harness.net.get("https://example.test") }"#;
    let effects = compute_handoff_effects(source, None);
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect.kind, EffectKind::Net)
                && effect.scope == EffectScope::Read
                && effect.resource.as_deref() == Some("https://example.test")),
        "expected Net read effect, got {effects:?}"
    );
}

#[test]
fn harness_process_run_yields_process_hostcall_effect() {
    let source = r#"fn main(harness: Harness) {
            harness.process.run({program: "printf", args: ["hello"]})
        }"#;
    let effects = compute_handoff_effects(source, None);
    assert!(
        effects.iter().any(|effect| {
            matches!(&effect.kind, EffectKind::Process)
                && effect.scope == EffectScope::Write
                && effect.resource.as_deref() == Some("printf")
        }),
        "expected process hostcall write effect, got {effects:?}"
    );
}

#[test]
fn http_get_builtin_yields_net_effect_with_resource() {
    let source = r#"fn main(harness: Harness) { harness.net.get("https://example.test/api") }"#;
    let effects = compute_handoff_effects(source, None);
    let net = effects
        .iter()
        .find(|effect| matches!(effect.kind, EffectKind::Net))
        .expect("net effect");
    assert_eq!(net.scope, EffectScope::Read);
    assert_eq!(net.resource.as_deref(), Some("https://example.test/api"));
}

#[test]
fn unix_socket_json_request_yields_net_effect_with_resource() {
    let source = r#"fn main(harness: Harness) {
            harness.net.unix_socket_json_request("/tmp/harn.sock", {})
        }"#;
    let effects = compute_handoff_effects(source, None);
    let net = effects
        .iter()
        .find(|effect| matches!(effect.kind, EffectKind::Net))
        .expect("net effect");
    assert_eq!(net.scope, EffectScope::Mutate);
    assert_eq!(net.resource.as_deref(), Some("/tmp/harn.sock"));
}

#[test]
fn files_upload_yields_fs_read_and_net_write_effects() {
    let source = r#"fn main(harness: Harness) {
            harness.llm.upload_file("/tmp/input.pdf", "gemini")
        }"#;
    let effects = compute_handoff_effects(source, None);
    assert!(
        effects.iter().any(|effect| {
            matches!(effect.kind, EffectKind::Fs)
                && effect.scope == EffectScope::Read
                && effect.resource.as_deref() == Some("/tmp/input.pdf")
        }),
        "expected Fs read effect, got {effects:?}"
    );
    assert!(
        effects.iter().any(|effect| {
            matches!(effect.kind, EffectKind::Net)
                && effect.scope == EffectScope::Write
                && effect.resource.as_deref() == Some("gemini")
        }),
        "expected Net write effect, got {effects:?}"
    );
}

#[test]
fn harness_fs_write_yields_fs_write_effect() {
    let source = r#"fn main(harness: Harness) { harness.fs.write_text("/tmp/out", "hi") }"#;
    let effects = compute_handoff_effects(source, None);
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect.kind, EffectKind::Fs)
                && effect.scope == EffectScope::Write
                && effect.resource.as_deref() == Some("/tmp/out")),
        "expected Fs write effect, got {effects:?}"
    );
}

#[test]
fn granular_capability_parameter_preserves_effect_contract() {
    let source = r#"
fn write_output(fs: HarnessFs) {
    fs.write_text("/tmp/out", "hi")
}

fn main(harness: Harness) {
    write_output(harness.fs)
}
"#;
    let effects = compute_handoff_effects(source, None);
    assert!(
        effects.iter().any(|effect| {
            matches!(effect.kind, EffectKind::Fs)
                && effect.scope == EffectScope::Write
                && effect.resource.as_deref() == Some("/tmp/out")
        }),
        "expected granular HarnessFs effect, got {effects:?}"
    );
}

#[test]
fn capability_alias_preserves_effect_contract() {
    let source = r#"
fn main(harness: Harness) {
    const fs = harness.fs
    fs.write_text("/tmp/out", "hi")
}
"#;
    let effects = compute_handoff_effects(source, None);
    assert!(
        effects.iter().any(|effect| {
            matches!(effect.kind, EffectKind::Fs)
                && effect.scope == EffectScope::Write
                && effect.resource.as_deref() == Some("/tmp/out")
        }),
        "expected aliased HarnessFs effect, got {effects:?}"
    );
}

#[test]
fn capability_method_can_declare_multiple_effects() {
    let source = r#"fn main(harness: Harness) {
            harness.net.download("https://example.test/data", "/tmp/data")
        }"#;
    let effects = compute_handoff_effects(source, None);
    assert!(
        effects.iter().any(|effect| {
            matches!(effect.kind, EffectKind::Net)
                && effect.scope == EffectScope::Read
                && effect.resource.as_deref() == Some("https://example.test/data")
        }),
        "expected download network effect, got {effects:?}"
    );
    assert!(
        effects.iter().any(|effect| {
            matches!(effect.kind, EffectKind::Fs)
                && effect.scope == EffectScope::Write
                && effect.resource.as_deref() == Some("/tmp/data")
        }),
        "expected download filesystem effect, got {effects:?}"
    );
}

#[test]
fn harness_term_read_password_yields_stdio_read_effect() {
    let source = r#"fn main(harness: Harness) { harness.term.read_password("password: ") }"#;
    let effects = compute_handoff_effects(source, None);
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect.kind, EffectKind::Stdio)
                && effect.scope == EffectScope::Read),
        "expected Stdio read effect, got {effects:?}"
    );
}

#[test]
fn harness_fs_mkdtemp_yields_fs_write_effect() {
    let source = r#"fn main(harness: Harness) { harness.fs.mkdtemp("harn-") }"#;
    let effects = compute_handoff_effects(source, None);
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect.kind, EffectKind::Fs)
                && effect.scope == EffectScope::Write),
        "expected Fs write effect, got {effects:?}"
    );
}

#[test]
fn harness_crypto_sha256_is_pure_for_handoff_effects() {
    let source = r#"fn main(harness: Harness) { sha256_hex("hello") }"#;
    let effects = compute_handoff_effects(source, None);
    assert!(effects.is_empty(), "expected no effects, got {effects:?}");
}

#[test]
fn harness_stdio_read_line_yields_stdio_read_effect() {
    let source = r"fn main(harness: Harness) { harness.stdio.read_line() }";
    let effects = compute_handoff_effects(source, None);
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect.kind, EffectKind::Stdio)
                && effect.scope == EffectScope::Read),
        "expected Stdio read effect, got {effects:?}"
    );
}

#[test]
fn llm_call_emits_llm_effect_with_provider_and_model() {
    let source = r#"fn main(harness: Harness) {
            harness.llm.call(
                "summarize",
                nil,
                { provider: "anthropic", model: "claude-3-5-sonnet" },
            )
        }"#;
    let effects = compute_handoff_effects(source, None);
    let llm = effects
        .iter()
        .find(|effect| matches!(effect.kind, EffectKind::Llm { .. }))
        .expect("llm effect");
    let EffectKind::Llm { provider, model } = &llm.kind else {
        panic!("expected llm kind, got {:?}", llm.kind);
    };
    assert_eq!(provider.as_deref(), Some("anthropic"));
    assert_eq!(model.as_deref(), Some("claude-3-5-sonnet"));
}

#[test]
fn runtime_llm_contract_combines_provider_and_model_resources() {
    let entry = crate::stdlib::builtin_manifest_entry("__cap_llm_call")
        .expect("LLM capability manifest entry");
    let options = VmValue::dict(crate::value::DictMap::from_iter([
        ("provider", VmValue::String("anthropic".into())),
        ("model", VmValue::String("claude-sonnet-4".into())),
    ]));
    let effects = runtime_effects_from_contract(
        entry.contract.effects,
        &[VmValue::Nil, VmValue::Nil, options],
    );
    assert_eq!(effects.len(), 1);
    assert!(matches!(
        &effects[0].kind,
        EffectKind::Llm { provider: Some(provider), model: Some(model) }
            if provider == "anthropic" && model == "claude-sonnet-4"
    ));
}

#[test]
fn harness_llm_catalog_yields_read_effect() {
    let source = r"fn main(harness: Harness) {
            harness.llm.catalog()
            harness.llm.providers()
        }";
    let effects = compute_handoff_effects(source, None);
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect.kind, EffectKind::Llm { .. })
                && effect.scope == EffectScope::Read),
        "expected LLM read effect, got {effects:?}"
    );
}

#[test]
fn ceiling_drops_disallowed_capabilities() {
    let source = r#"fn main(harness: Harness) {
            harness.net.get("https://example.test")
            harness.fs.read_text("/tmp/in")
        }"#;
    let mut ceiling = CapabilityPolicy::default();
    ceiling
        .capabilities
        .insert("workspace".to_string(), vec!["read_text".to_string()]);
    let effects = compute_handoff_effects(source, Some(&ceiling));
    assert!(
        effects
            .iter()
            .all(|effect| !matches!(effect.kind, EffectKind::Net)),
        "ceiling without `network` should drop Net effect, got {effects:?}"
    );
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect.kind, EffectKind::Fs)),
        "ceiling with workspace.read_text should keep Fs read, got {effects:?}"
    );
}

#[test]
fn ceiling_side_effect_level_clamps_writes() {
    let source = r#"fn main(harness: Harness) {
            harness.net.get("https://example.test")
            harness.stdio.println("hi")
        }"#;
    let ceiling = CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        ..Default::default()
    };
    let effects = compute_handoff_effects(source, Some(&ceiling));
    assert!(
        effects
            .iter()
            .all(|effect| !matches!(effect.kind, EffectKind::Net)),
        "read_only ceiling must drop Net write, got {effects:?}"
    );
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect.kind, EffectKind::Stdio)),
        "stdio observe should pass read_only ceiling, got {effects:?}"
    );
}

#[test]
fn read_only_handoff_keeps_runtime_control_plane_and_drops_user_world_writes() {
    let source = r#"fn main(harness: Harness) {
            harness.agent.open("child-session")
            harness.fs.write_text("artifact.txt", "must not survive")
        }"#;
    let ceiling = CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        ..Default::default()
    };
    let effects = compute_handoff_effects(source, Some(&ceiling));
    assert!(
        effects.iter().any(|effect| {
            matches!(effect.kind, EffectKind::State)
                && effect.scope == EffectScope::Mutate
                && effect.resource.as_deref() == Some("agent-sessions")
        }),
        "runtime-owned child session state disappeared from handoff lineage: {effects:?}"
    );
    assert!(
        effects
            .iter()
            .all(|effect| !matches!(effect.kind, EffectKind::Fs)),
        "runtime classification must not disable the user-world ceiling: {effects:?}"
    );
}

#[test]
fn read_only_handoff_still_drops_unmarked_agent_state_mutation() {
    let source = r#"fn main(harness: Harness) {
            harness.agent.state_write("child-session", "key", "value")
        }"#;
    let ceiling = CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        ..Default::default()
    };
    let effects = compute_handoff_effects(source, Some(&ceiling));
    assert!(
        effects
            .iter()
            .all(|effect| !matches!(effect.kind, EffectKind::State)),
        "unmarked state mutation escaped the read-only ceiling: {effects:?}"
    );
}

#[test]
fn restricted_capabilities_can_deny_runtime_control_plane_state() {
    let source = r#"fn main(harness: Harness) {
            harness.agent.open("child-session")
        }"#;
    let mut ceiling = CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        ..Default::default()
    };
    ceiling
        .capabilities
        .insert("workspace".to_string(), vec!["read_text".to_string()]);
    let effects = compute_handoff_effects(source, Some(&ceiling));
    assert!(
        effects
            .iter()
            .all(|effect| !matches!(effect.kind, EffectKind::State)),
        "runtime classification bypassed the explicit capability ceiling: {effects:?}"
    );
}

#[test]
fn effect_record_round_trips_through_serde() {
    let effects = vec![
        EffectRecord::new(EffectKind::Net, EffectScope::Write)
            .with_resource("https://api.example/v1"),
        EffectRecord::new(EffectKind::Fs, EffectScope::Read).with_resource("/workspace/src"),
        EffectRecord::new(
            EffectKind::Llm {
                provider: Some("anthropic".to_string()),
                model: Some("claude-3-7-sonnet".to_string()),
            },
            EffectScope::Write,
        ),
        EffectRecord::new(
            EffectKind::Tool {
                name: "search".to_string(),
            },
            EffectScope::Read,
        ),
    ];
    let encoded = serde_json::to_string(&effects).expect("encode");
    let decoded: Vec<EffectRecord> = serde_json::from_str(&encoded).expect("decode");
    assert_eq!(decoded, effects);
}

#[test]
fn empty_source_returns_no_effects() {
    let effects = compute_handoff_effects("fn main() {}", None);
    assert!(effects.is_empty(), "got {effects:?}");
}

#[test]
fn effects_from_metadata_round_trips_typed_payload() {
    let effects =
        vec![EffectRecord::new(EffectKind::Net, EffectScope::Write)
            .with_resource("https://api.example")];
    let mut metadata: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    metadata.insert(
        "effects".to_string(),
        serde_json::to_value(&effects).expect("encode"),
    );
    assert_eq!(effects_from_metadata(&metadata), effects);
}

#[test]
fn subset_violations_returns_empty_when_child_covered() {
    let parent = vec![
        EffectRecord::new(EffectKind::Net, EffectScope::Write),
        EffectRecord::new(EffectKind::Fs, EffectScope::Read).with_resource("/workspace"),
    ];
    let child = vec![
        EffectRecord::new(EffectKind::Net, EffectScope::Write)
            .with_resource("https://example.test"),
        EffectRecord::new(EffectKind::Fs, EffectScope::Read).with_resource("/workspace"),
    ];
    assert!(effect_subset_violations(Some(&parent), &child).is_empty());
}

#[test]
fn subset_violations_flags_unmatched_kinds() {
    let parent = vec![EffectRecord::new(EffectKind::Fs, EffectScope::Read)];
    let child = vec![EffectRecord::new(EffectKind::Net, EffectScope::Write)
        .with_resource("https://example.test")];
    let violations = effect_subset_violations(Some(&parent), &child);
    assert_eq!(violations.len(), 1);
    assert!(matches!(violations[0].kind, EffectKind::Net));
}

#[test]
fn subset_violations_flags_scope_escalations() {
    let parent = vec![EffectRecord::new(EffectKind::Fs, EffectScope::Read)];
    let child = vec![EffectRecord::new(EffectKind::Fs, EffectScope::Mutate)];
    let violations = effect_subset_violations(Some(&parent), &child);
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].scope, EffectScope::Mutate);
}

#[test]
fn subset_violations_treats_missing_parent_resource_as_wildcard() {
    let parent = vec![EffectRecord::new(EffectKind::Net, EffectScope::Write)];
    let child = vec![EffectRecord::new(EffectKind::Net, EffectScope::Write)
        .with_resource("https://api.example/v1")];
    assert!(effect_subset_violations(Some(&parent), &child).is_empty());
}

#[test]
fn subset_violations_requires_resource_match_when_parent_declares_one() {
    let parent = vec![EffectRecord::new(EffectKind::Net, EffectScope::Write)
        .with_resource("https://allowed.test")];
    let child = vec![EffectRecord::new(EffectKind::Net, EffectScope::Write)
        .with_resource("https://disallowed.test")];
    let violations = effect_subset_violations(Some(&parent), &child);
    assert_eq!(violations.len(), 1);
}

#[test]
fn subset_violations_skip_when_parent_is_none() {
    let child = vec![EffectRecord::new(EffectKind::Net, EffectScope::Write)];
    assert!(effect_subset_violations(None, &child).is_empty());
}

#[test]
fn subset_violations_empty_parent_flags_every_child_effect() {
    let parent: Vec<EffectRecord> = Vec::new();
    let child = vec![
        EffectRecord::new(EffectKind::Net, EffectScope::Write),
        EffectRecord::new(EffectKind::Fs, EffectScope::Read),
    ];
    let violations = effect_subset_violations(Some(&parent), &child);
    assert_eq!(violations.len(), 2);
}

#[test]
fn subset_violations_empty_child_is_always_allowed() {
    let parent = vec![EffectRecord::new(EffectKind::Net, EffectScope::Write)];
    assert!(effect_subset_violations(Some(&parent), &[]).is_empty());
}

#[test]
fn effect_kind_label_shape() {
    assert_eq!(effect_kind_label(&EffectKind::Net), "net");
    assert_eq!(
        effect_kind_label(&EffectKind::Llm {
            provider: Some("anthropic".to_string()),
            model: Some("claude-3-7-sonnet".to_string()),
        }),
        "llm:anthropic/claude-3-7-sonnet"
    );
    assert_eq!(
        effect_kind_label(&EffectKind::Tool {
            name: "search".to_string()
        }),
        "tool:search"
    );
}

#[test]
fn effect_record_summary_includes_resource() {
    let effect = EffectRecord::new(EffectKind::Net, EffectScope::Write)
        .with_resource("https://example.test/api");
    assert_eq!(
        effect_record_summary(&effect),
        "net:write (https://example.test/api)"
    );
}

#[test]
fn deduplicates_repeated_effects() {
    let source = r#"fn main(harness: Harness) {
            harness.net.get("https://example.test")
            harness.net.get("https://example.test")
            harness.net.get("https://example.test")
        }"#;
    let effects = compute_handoff_effects(source, None);
    let net_count = effects
        .iter()
        .filter(|effect| matches!(effect.kind, EffectKind::Net))
        .count();
    assert_eq!(net_count, 1, "expected dedup, got {effects:?}");
}
