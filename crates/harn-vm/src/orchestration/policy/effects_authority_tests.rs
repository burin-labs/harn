use super::*;

fn capability_ceiling(capability: &str, operations: &[&str]) -> CapabilityPolicy {
    CapabilityPolicy {
        capabilities: BTreeMap::from([(
            capability.to_string(),
            operations
                .iter()
                .map(|operation| operation.to_string())
                .collect(),
        )]),
        ..CapabilityPolicy::default()
    }
}

#[test]
fn authority_effect_policy_preserves_access_and_resource_scope() {
    let plan_admission = EffectRecord::new(EffectKind::Authority, EffectScope::Write)
        .with_resource("plan_admission");
    let native_approval = EffectRecord::new(EffectKind::Authority, EffectScope::Write)
        .with_resource("native_approval");

    let exact = capability_ceiling("authority", &["write@plan_admission"]);
    assert!(effect_allowed_by_ceiling(&plan_admission, &exact));
    assert!(!effect_allowed_by_ceiling(&native_approval, &exact));

    let broad = capability_ceiling("authority", &["write"]);
    assert!(effect_allowed_by_ceiling(&plan_admission, &broad));
    assert!(effect_allowed_by_ceiling(&native_approval, &broad));

    let connector_only = capability_ceiling("connector", &["call"]);
    assert!(!effect_allowed_by_ceiling(&plan_admission, &connector_only));
    let existing_host_interaction =
        EffectRecord::new(EffectKind::Host, EffectScope::Write).with_resource("human-approval");
    let existing_host_introspection =
        EffectRecord::new(EffectKind::Host, EffectScope::Read).with_resource("runtime-tools");
    assert!(effect_allowed_by_ceiling(
        &existing_host_interaction,
        &connector_only
    ));
    assert!(effect_allowed_by_ceiling(
        &existing_host_introspection,
        &connector_only
    ));
}
