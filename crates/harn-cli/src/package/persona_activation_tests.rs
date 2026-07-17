use std::sync::Barrier;

use super::*;
use crate::package::test_support::*;
use crate::package::{LockFile, MANIFEST};

fn write_root(root: &Path) -> PathBuf {
    let manifest = root.join(MANIFEST);
    fs::write(&manifest, "[package]\nname = \"consumer\"\n").unwrap();
    manifest
}

fn install_one(root: &Path) -> PathBuf {
    let manifest = write_root(root);
    install_test_persona_package(root, "agents", vec!["reviewer".to_string()], &["reviewer"]);
    manifest
}

#[test]
fn activation_attenuates_and_pins_the_installed_policy_idempotently() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = install_one(tmp.path());
    let attenuation = PersonaAttenuation {
        autonomy_tier: Some(PersonaAutonomyTier::Suggest),
        tools: Some(vec!["shell".to_string()]),
        capabilities: Some(Vec::new()),
    };

    let first = activate_persona(Some(&manifest), "agents/reviewer", &attenuation, 100).unwrap();
    let second = activate_persona(Some(&manifest), "agents/reviewer", &attenuation, 200).unwrap();

    assert!(first.changed);
    assert!(!second.changed);
    assert_eq!(second.schema_version, ACTIVATION_RECEIPT_SCHEMA_VERSION);
    let activation = second.activation.unwrap();
    assert_eq!(activation.activated_at_ms, 100);
    assert_eq!(
        activation.effective_policy.autonomy_tier,
        PersonaAutonomyTier::Suggest
    );
    assert_eq!(activation.effective_policy.tools, vec!["shell"]);
    assert!(activation.effective_policy.capabilities.is_empty());
    assert!(activation.migration.is_none());
    assert_ne!(
        activation.exported_policy_digest,
        activation.effective_policy_digest
    );
    assert!(activation.package.content_hash.starts_with("sha256:"));
    assert_eq!(activation.package.alias, "agents");

    let root = load_root_persona_catalog(Some(&manifest)).unwrap();
    let discovered = resolve_discoverable_persona_in_root(&root, "agents/reviewer").unwrap();
    let materialized = materialize_activated_persona(&discovered, &activation).unwrap();
    assert_eq!(
        materialized.model_policy.default_model.as_deref(),
        Some("cheap")
    );
    assert_eq!(materialized.budget.daily_usd, Some(10.0));
    assert_eq!(
        materialized.receipt_policy,
        Some(PersonaReceiptPolicy::Required)
    );

    let listed = list_persona_activations(Some(&manifest)).unwrap();
    assert_eq!(listed, vec![activation]);
    let ledger = fs::read(activation_ledger_path(tmp.path())).unwrap();
    assert_eq!(ledger.last(), Some(&b'\n'));
}

#[test]
fn activation_attenuates_model_implied_llm_authority() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = install_one(tmp.path());

    let inherited = activate_persona(
        Some(&manifest),
        "agents/reviewer",
        &PersonaAttenuation::default(),
        100,
    )
    .unwrap();
    assert_eq!(
        inherited.activation.unwrap().effective_policy.capabilities,
        vec!["llm.call", "workspace.read_text"]
    );

    let denied = activate_persona(
        Some(&manifest),
        "agents/reviewer",
        &PersonaAttenuation {
            capabilities: Some(Vec::new()),
            ..Default::default()
        },
        200,
    )
    .unwrap();
    assert!(denied
        .activation
        .unwrap()
        .effective_policy
        .capabilities
        .is_empty());
}

#[test]
fn activation_rejects_every_authority_expansion_without_writing_state() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = install_one(tmp.path());
    let expansions = vec![
        (
            "autonomy",
            PersonaAttenuation {
                autonomy_tier: Some(PersonaAutonomyTier::ActAuto),
                ..PersonaAttenuation::default()
            },
        ),
        (
            "tool",
            PersonaAttenuation {
                tools: Some(vec!["network".to_string()]),
                ..PersonaAttenuation::default()
            },
        ),
        (
            "capability",
            PersonaAttenuation {
                capabilities: Some(vec!["network.fetch".to_string()]),
                ..PersonaAttenuation::default()
            },
        ),
        (
            "empty tool",
            PersonaAttenuation {
                tools: Some(vec![" ".to_string()]),
                ..PersonaAttenuation::default()
            },
        ),
    ];

    for (label, attenuation) in expansions {
        let error = activate_persona(Some(&manifest), "agents/reviewer", &attenuation, 100)
            .expect_err(label);
        assert!(
            matches!(error, PersonaActivationError::InvalidAttenuation(_)),
            "{label}: {error}"
        );
    }
    assert!(list_persona_activations(Some(&manifest))
        .unwrap()
        .is_empty());
}

#[test]
fn activation_observes_unhashed_content_and_rejects_invalid_or_tampered_packages() {
    let missing_hash = tempfile::tempdir().unwrap();
    let manifest = write_root(missing_hash.path());
    let mut lock = install_test_persona_package(
        missing_hash.path(),
        "agents",
        vec!["reviewer".to_string()],
        &["reviewer"],
    );
    lock.packages[0].content_hash = None;
    write_lock(missing_hash.path(), &lock);
    let activation = activate_persona(
        Some(&manifest),
        "agents/reviewer",
        &PersonaAttenuation::default(),
        100,
    )
    .unwrap()
    .activation
    .unwrap();
    assert!(activation.package.content_hash.starts_with("sha256:"));

    lock.packages[0].content_hash = Some(" ".to_string());
    write_lock(missing_hash.path(), &lock);
    let error = activate_persona(
        Some(&manifest),
        "agents/reviewer",
        &PersonaAttenuation::default(),
        100,
    )
    .unwrap_err();
    assert!(error.to_string().contains("content hash"), "{error}");

    let tampered = tempfile::tempdir().unwrap();
    let manifest = install_one(tampered.path());
    fs::write(
        current_packages_dir(tampered.path()).join("agents/workflow.harn"),
        "pub pipeline run(task) -> dict { return {tampered: true} }\n",
    )
    .unwrap();
    let error = activate_persona(
        Some(&manifest),
        "agents/reviewer",
        &PersonaAttenuation::default(),
        100,
    )
    .unwrap_err();
    assert!(error.to_string().contains("content changed"), "{error}");
}

#[test]
fn exported_contract_digest_covers_runtime_entry_and_triggers() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = install_one(tmp.path());
    let root = load_root_persona_catalog(Some(&manifest)).unwrap();
    let discovered = resolve_discoverable_persona_in_root(&root, "agents/reviewer").unwrap();
    let provenance = discovered.installed_provenance().unwrap();
    let original =
        policy_digest(&exported_policy_contract(&discovered.persona, provenance).unwrap()).unwrap();
    let mut changed = discovered.persona.clone();
    changed.entry_workflow = Some("alternate.harn#run".to_string());
    changed.triggers.push("github.issue_opened".to_string());
    let changed = policy_digest(&exported_policy_contract(&changed, provenance).unwrap()).unwrap();

    assert_ne!(original, changed);
}

#[test]
fn root_personas_are_not_activation_targets() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join(MANIFEST);
    fs::write(
        &manifest,
        format!(
            "[package]\nname = \"consumer\"\n\n{}",
            persona_manifest("reviewer", "workflow.harn#run")
        ),
    )
    .unwrap();
    fs::write(
        tmp.path().join("workflow.harn"),
        "pub pipeline run(task) -> dict { return {ok: true} }\n",
    )
    .unwrap();

    let error = activate_persona(
        Some(&manifest),
        "reviewer",
        &PersonaAttenuation::default(),
        100,
    )
    .unwrap_err();
    assert!(matches!(error, PersonaActivationError::RootPersona(_)));
}

#[test]
fn deactivation_survives_package_removal_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = install_one(tmp.path());
    activate_persona(
        Some(&manifest),
        "agents/reviewer",
        &PersonaAttenuation::default(),
        100,
    )
    .unwrap();
    fs::remove_dir_all(current_packages_dir(tmp.path()).join("agents")).unwrap();

    let first = deactivate_persona(Some(&manifest), "agents/reviewer", 200).unwrap();
    let second = deactivate_persona(Some(&manifest), "agents/reviewer", 300).unwrap();

    assert!(first.changed);
    assert_eq!(first.activation.unwrap().persona_id, "agents/reviewer");
    assert!(!second.changed);
    assert!(second.activation.is_none());
}

#[test]
fn activation_ledger_rejects_malformed_unknown_and_tampered_state() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = install_one(tmp.path());
    let ledger_path = activation_ledger_path(tmp.path());
    fs::create_dir_all(ledger_path.parent().unwrap()).unwrap();

    fs::write(&ledger_path, b"not-json\n").unwrap();
    assert!(matches!(
        load_activation_ledger(tmp.path()).unwrap_err(),
        PersonaActivationError::InvalidLedger { .. }
    ));

    fs::write(
        &ledger_path,
        b"{\"schema_version\":999,\"activations\":{}}\n",
    )
    .unwrap();
    assert!(matches!(
        load_activation_ledger(tmp.path()).unwrap_err(),
        PersonaActivationError::UnsupportedSchema { .. }
    ));

    fs::remove_file(&ledger_path).unwrap();
    activate_persona(
        Some(&manifest),
        "agents/reviewer",
        &PersonaAttenuation::default(),
        100,
    )
    .unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&ledger_path).unwrap()).unwrap();
    value["activations"]["agents/reviewer"]["effective_policy"]["autonomy_tier"] =
        serde_json::Value::String("shadow".to_string());
    fs::write(&ledger_path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let error = load_activation_ledger(tmp.path()).unwrap_err();
    assert!(matches!(
        error,
        PersonaActivationError::InvalidLedger { .. }
    ));
    assert!(error.to_string().contains("digest"));
}

#[test]
fn schema_v1_activation_is_auditable_but_requires_reactivation() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = install_one(tmp.path());
    let current = activate_persona(
        Some(&manifest),
        "agents/reviewer",
        &PersonaAttenuation::default(),
        100,
    )
    .unwrap()
    .activation
    .unwrap();
    let root = load_root_persona_catalog(Some(&manifest)).unwrap();
    let discovered = resolve_discoverable_persona_in_root(&root, "agents/reviewer").unwrap();
    let provenance = discovered.installed_provenance().unwrap();
    let legacy_policy = LegacyPersonaEffectivePolicyV1 {
        autonomy_tier: discovered.persona.autonomy_tier.unwrap(),
        receipt_policy: discovered.persona.receipt_policy.unwrap(),
        tools: normalize_set(&discovered.persona.tools),
        capabilities: normalized_persona_capabilities(&discovered.persona),
        permissions: normalize_set(&provenance.permissions),
        host_requirements: normalize_set(&provenance.host_requirements),
        model_policy: discovered.persona.model_policy.clone(),
        budget: discovered.persona.budget.clone(),
    };
    let legacy_digest = policy_digest(&legacy_policy).unwrap();
    let legacy = LegacyPersonaActivationLedgerV1 {
        schema_version: LEGACY_ACTIVATION_SCHEMA_VERSION,
        activations: std::collections::BTreeMap::from([(
            current.persona_id.clone(),
            LegacyPersonaActivationRecordV1 {
                persona_id: current.persona_id.clone(),
                package: current.package.clone(),
                exported_policy_digest: current.exported_policy_digest.clone(),
                effective_policy_digest: legacy_digest.clone(),
                effective_policy: legacy_policy,
                activated_at_ms: current.activated_at_ms,
            },
        )]),
    };
    let ledger_path = activation_ledger_path(tmp.path());
    let mut legacy_value = serde_json::to_value(&legacy).unwrap();
    legacy_value["activations"]["agents/reviewer"]["package"]
        .as_object_mut()
        .unwrap()
        .remove("lock_digest");
    fs::write(
        &ledger_path,
        serde_json::to_vec_pretty(&legacy_value).unwrap(),
    )
    .unwrap();

    let migrated = load_activation_ledger(tmp.path()).unwrap();
    assert_eq!(migrated.schema_version, ACTIVATION_SCHEMA_VERSION);
    let activation = migrated.activations.get("agents/reviewer").unwrap();
    let migration = activation.migration.as_ref().unwrap();
    assert_eq!(
        migration.status,
        PersonaActivationMigrationStatus::ReactivationRequired
    );
    assert_eq!(migration.legacy_effective_policy_digest, legacy_digest);
    assert!(activation.package.lock_digest.is_empty());
    assert!(matches!(
        materialize_activated_persona(&discovered, activation).unwrap_err(),
        PersonaActivationError::StaleActivation { .. }
    ));

    let refreshed = activate_persona(
        Some(&manifest),
        "agents/reviewer",
        &PersonaAttenuation::default(),
        200,
    )
    .unwrap();
    assert!(refreshed.changed);
    assert!(refreshed.activation.unwrap().migration.is_none());
    assert_eq!(
        load_activation_ledger(tmp.path()).unwrap().schema_version,
        2
    );
}

#[test]
fn concurrent_activation_writers_preserve_all_records() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = write_root(tmp.path());
    create_test_package_generation(tmp.path());
    let mut lock = LockFile::default();
    add_test_persona_package(
        tmp.path(),
        &mut lock,
        "alpha",
        vec!["reviewer".to_string()],
        &["reviewer"],
    );
    add_test_persona_package(
        tmp.path(),
        &mut lock,
        "beta",
        vec!["reviewer".to_string()],
        &["reviewer"],
    );
    let barrier = Barrier::new(2);

    std::thread::scope(|scope| {
        for persona_id in ["alpha/reviewer", "beta/reviewer"] {
            let manifest = &manifest;
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                activate_persona(
                    Some(manifest),
                    persona_id,
                    &PersonaAttenuation::default(),
                    100,
                )
                .unwrap();
            });
        }
    });

    let ids = list_persona_activations(Some(&manifest))
        .unwrap()
        .into_iter()
        .map(|activation| activation.persona_id)
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["alpha/reviewer", "beta/reviewer"]);
}

fn write_lock(root: &Path, lock: &LockFile) {
    let body = toml::to_string_pretty(lock).unwrap();
    fs::write(root.join("harn.lock"), &body).unwrap();
    write_test_generation_lock(root, &body);
}
