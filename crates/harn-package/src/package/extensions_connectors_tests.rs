use super::*;
use harn_vm::secrets::SecretId;

fn provider(id: &str, outbound: &str) -> String {
    format!(
        r#"
[[providers]]
id = "{id}"
connector = {{ rust = "builtin" }}
[providers.setup]
required_secrets = [
  {{ id = "{id}/{outbound}", direction = "outbound" }},
  {{ id = "{id}/webhook-signing-secret", direction = "inbound" }},
]
"#
    )
}

#[test]
fn linked_discovery_selects_outbound_grants_after_root_precedence() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dependency = root.join("vendor/connectors");
    fs::create_dir_all(&dependency).unwrap();
    fs::write(
        dependency.join(MANIFEST),
        format!(
            "[package]\nname = \"connectors\"\nversion = \"1.0.0\"\n{}{}",
            provider("github", "package-token"),
            provider("gitlab", "package-token"),
        ),
    )
    .unwrap();
    fs::write(
        root.join(MANIFEST),
        format!(
            "[package]\nname = \"consumer\"\n[dependencies]\nconnectors = {{ path = \"vendor/connectors\" }}\n{}{}",
            provider("github", "root-token"), provider("github", "shadowed-token"),
        ),
    ).unwrap();
    let workspace = PackageWorkspace::for_test(root, root.join(".cache"));
    assert_eq!(
        install_packages_in(&workspace, false, None, false).unwrap(),
        1
    );

    let resolved = crate::try_load_provider_connectors(&root.join("main.harn")).unwrap();
    assert_eq!(resolved.configs.len(), 2);
    assert_eq!(
        resolved.outbound_secret_ids(),
        BTreeSet::from([
            SecretId::new("github", "root-token"),
            SecretId::new("gitlab", "package-token"),
        ])
    );
    let root_only = crate::try_load_root_provider_connectors(&root.join("main.harn")).unwrap();
    assert_eq!(root_only.configs.len(), 1);
    assert_eq!(
        root_only.outbound_secret_ids(),
        BTreeSet::from([SecretId::new("github", "root-token"),])
    );
}

#[test]
fn linked_discovery_retains_generation_lease_across_publication() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dependency = root.join("vendor/connectors");
    fs::create_dir_all(&dependency).unwrap();
    fs::write(
        dependency.join(MANIFEST),
        format!(
            "[package]\nname = \"connectors\"\nversion = \"1.0.0\"\n{}",
            provider("github", "package-token"),
        ),
    )
    .unwrap();
    fs::write(root.join(MANIFEST),
        "[package]\nname = \"consumer\"\n[dependencies]\nconnectors = { path = \"vendor/connectors\" }\n",
    ).unwrap();
    let workspace = PackageWorkspace::for_test(root, root.join(".cache"));
    install_packages_in(&workspace, false, None, false).unwrap();
    let resolved = crate::try_load_provider_connectors(&root.join("main.harn")).unwrap();
    assert_eq!(resolved.configs.len(), 1);
    let snapshot = resolved
        ._package_snapshot
        .as_ref()
        .expect("dependency generation retained");
    let old_generation = snapshot.generation().to_string();
    let old_module_dir = resolved.configs[0].manifest_dir.clone();
    let lease_path = harn_modules::package_snapshot::generation_root(root, &old_generation)
        .join(harn_modules::package_snapshot::GENERATION_LEASE_FILE);
    let lease = File::options()
        .read(true)
        .write(true)
        .open(&lease_path)
        .unwrap();
    assert!(
        matches!(lease.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "returned discovery must hold a real shared lease"
    );

    install_packages_in(&workspace, false, Some("connectors"), false).unwrap();
    let current = harn_modules::package_snapshot::PackageSnapshot::acquire(root)
        .unwrap()
        .unwrap();
    assert_ne!(current.generation(), old_generation);
    assert!(
        old_module_dir.join(MANIFEST).is_file(),
        "publication must preserve the leased generation"
    );
    assert!(matches!(
        lease.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    drop(resolved);
    lease
        .try_lock()
        .expect("dropping discovery releases generation custody");
}

#[test]
fn linked_discovery_refuses_missing_and_malformed_dependencies() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::write(
        root.join(MANIFEST),
        "[package]\nname = \"consumer\"\n[dependencies]\nmissing = { path = \"vendor/missing\" }\n",
    )
    .unwrap();
    let anchor = root.join("main.harn");
    let missing = crate::try_load_provider_connectors(&anchor)
        .err()
        .expect("missing lock must fail");
    assert!(missing.to_string().contains("harn.lock"));
    fs::write(root.join(LOCK_FILE), "version = [\n").unwrap();
    assert!(
        crate::try_load_provider_connectors(&anchor).is_err(),
        "malformed lock must fail"
    );
    fs::write(root.join(MANIFEST), "[[providers]\n").unwrap();
    assert!(
        crate::try_load_provider_connectors(&anchor).is_err(),
        "malformed manifest must fail"
    );
}
