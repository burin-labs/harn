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
        daily_usd: Some(5.0),
        hourly_usd: Some(2.0),
        run_usd: Some(1.0),
        frontier_escalations: Some(1),
        max_tokens: Some(2048),
        max_runtime_seconds: Some(300),
        tools: Some(vec!["shell".to_string()]),
        capabilities: Some(Vec::new()),
        permissions: Some(vec!["workspace:read_text".to_string()]),
        host_requirements: Some(Vec::new()),
    };

    let first = activate_persona(Some(&manifest), "agents/reviewer", &attenuation, 100).unwrap();
    let second = activate_persona(Some(&manifest), "agents/reviewer", &attenuation, 200).unwrap();

    assert!(first.changed);
    assert!(!second.changed);
    let activation = second.activation.unwrap();
    assert_eq!(activation.activated_at_ms, 100);
    assert_eq!(
        activation.effective_policy.autonomy_tier,
        PersonaAutonomyTier::Suggest
    );
    assert_eq!(activation.effective_policy.tools, vec!["shell"]);
    assert!(activation.effective_policy.capabilities.is_empty());
    assert_eq!(
        activation.effective_policy.permissions,
        vec!["workspace:read_text"]
    );
    assert_eq!(
        activation.effective_policy.host_requirements,
        Vec::<String>::new()
    );
    assert_eq!(
        activation
            .effective_policy
            .model_policy
            .default_model
            .as_deref(),
        Some("cheap")
    );
    assert_eq!(activation.effective_policy.budget.daily_usd, Some(5.0));
    assert_ne!(
        activation.exported_policy_digest,
        activation.effective_policy_digest
    );
    assert!(activation.package.content_hash.starts_with("sha256:"));
    assert_eq!(activation.package.alias, "agents");

    let listed = list_persona_activations(Some(&manifest)).unwrap();
    assert_eq!(listed, vec![activation]);
    let ledger = fs::read(activation_ledger_path(tmp.path())).unwrap();
    assert_eq!(ledger.last(), Some(&b'\n'));
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
            "daily_usd",
            PersonaAttenuation {
                daily_usd: Some(10.01),
                ..PersonaAttenuation::default()
            },
        ),
        (
            "hourly_usd",
            PersonaAttenuation {
                hourly_usd: Some(4.01),
                ..PersonaAttenuation::default()
            },
        ),
        (
            "run_usd",
            PersonaAttenuation {
                run_usd: Some(2.01),
                ..PersonaAttenuation::default()
            },
        ),
        (
            "frontier_escalations",
            PersonaAttenuation {
                frontier_escalations: Some(4),
                ..PersonaAttenuation::default()
            },
        ),
        (
            "max_tokens",
            PersonaAttenuation {
                max_tokens: Some(4097),
                ..PersonaAttenuation::default()
            },
        ),
        (
            "max_runtime_seconds",
            PersonaAttenuation {
                max_runtime_seconds: Some(601),
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
        (
            "permission",
            PersonaAttenuation {
                permissions: Some(vec!["network:fetch".to_string()]),
                ..PersonaAttenuation::default()
            },
        ),
        (
            "host requirement",
            PersonaAttenuation {
                host_requirements: Some(vec!["network.fetch".to_string()]),
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
fn activation_requires_content_hash_and_matching_materialized_content() {
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
    let error = activate_persona(
        Some(&manifest),
        "agents/reviewer",
        &PersonaAttenuation::default(),
        100,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        PersonaActivationError::MissingContentHash(_)
    ));

    lock.packages[0].content_hash = Some(" ".to_string());
    write_lock(missing_hash.path(), &lock);
    let error = activate_persona(
        Some(&manifest),
        "agents/reviewer",
        &PersonaAttenuation::default(),
        100,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        PersonaActivationError::MissingContentHash(_)
    ));

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
    assert!(matches!(
        error,
        PersonaActivationError::PackageIntegrity { .. }
    ));
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

    fs::write(&ledger_path, b"{\"schema_version\":2,\"activations\":{}}\n").unwrap();
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
