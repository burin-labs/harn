use super::*;
use crate::secrets::SecretVersion;

#[tokio::test]
async fn selected_reference_survives_transport_without_an_ambient_store() {
    let id = SecretId::new("fixture", "provider").with_version(SecretVersion::Exact(2));
    let parent = MemorySecretProvider::new("parent")
        .with_secret(id.clone(), b"inert-parent-canary")
        .with_secret(SecretId::new("fixture", "unselected"), b"must-not-cross");
    let handoff = ParentSecretHandoff::capture(&parent, [id.clone()])
        .await
        .unwrap();
    assert!(!format!("{handoff:?}").contains("inert-parent-canary"));
    let mut pipe = Vec::new();
    handoff.write_to(&mut pipe).unwrap();
    let child = ParentSecretHandoff::read_from(pipe.as_slice())
        .unwrap()
        .into_provider();
    assert!(child
        .get(&id)
        .await
        .unwrap()
        .with_exposed(|value| value == b"inert-parent-canary"));
    assert!(child
        .get(&SecretId::new("fixture", "unselected"))
        .await
        .unwrap_err()
        .is_not_found());
    assert!(child
        .put(&id, SecretBytes::from("replacement"))
        .await
        .is_err());
    assert!(!child.persists_writes());
    assert!(child
        .get(&id)
        .await
        .unwrap()
        .with_exposed(|value| value == b"inert-parent-canary"));
}

#[tokio::test]
async fn mixed_latest_and_exact_references_keep_their_parent_values_without_invented_versions() {
    let latest = SecretId::new("fixture", "versioned");
    let first = latest.clone().with_version(SecretVersion::Exact(1));
    let second = latest.clone().with_version(SecretVersion::Exact(2));
    let parent = MemorySecretProvider::new("parent")
        .with_secret(first.clone(), b"inert-old")
        .with_secret(second.clone(), b"inert-new");
    let handoff = ParentSecretHandoff::capture(&parent, [latest.clone(), first.clone()])
        .await
        .unwrap();
    let mut bytes = Vec::new();
    handoff.write_to(&mut bytes).unwrap();
    let child = ParentSecretHandoff::read_from(bytes.as_slice())
        .unwrap()
        .into_provider();
    assert!(child
        .get(&latest)
        .await
        .unwrap()
        .with_exposed(|value| value == b"inert-new"));
    assert!(child
        .get(&first)
        .await
        .unwrap()
        .with_exposed(|value| value == b"inert-old"));
    assert!(
        child.get(&second).await.unwrap_err().is_not_found(),
        "unselected numeric versions must not be fabricated"
    );
    let listed = child
        .list(&SecretId::new("fixture", "versioned"))
        .await
        .unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().any(|entry| entry.id == latest));
    assert!(listed.iter().any(|entry| entry.id == first));
}

#[test]
fn malformed_and_unbounded_frames_fail_without_echoing_the_payload() {
    for payload in [
        b"inert-parent-canary".as_slice(),
        b"{}",
        b"{\"version\":2,\"secrets\":[]}",
    ] {
        let mut pipe = (payload.len() as u32).to_be_bytes().to_vec();
        pipe.extend_from_slice(payload);
        let error = ParentSecretHandoff::read_from(pipe.as_slice()).unwrap_err();
        assert!(!error.to_string().contains("inert-parent-canary"));
    }
    assert!(
        ParentSecretHandoff::read_from((MAX_FRAME_BYTES as u32 + 1).to_be_bytes().as_slice())
            .is_err()
    );
    assert!(ParentSecretHandoff::read_from([0, 0, 0, 9, b'{'].as_slice()).is_err());
}

#[tokio::test]
async fn absent_or_duplicate_parent_references_are_refused() {
    let id = SecretId::new("fixture", "provider");
    let parent = MemorySecretProvider::new("parent").with_secret(id.clone(), b"inert-canary");
    assert!(ParentSecretHandoff::capture(&parent, [id.clone(), id])
        .await
        .is_err());
    assert!(
        ParentSecretHandoff::capture(&parent, [SecretId::new("fixture", "absent")])
            .await
            .unwrap_err()
            .is_not_found()
    );
}

#[tokio::test]
async fn every_configured_reader_uses_the_closed_parent_store() {
    let id = SecretId::new("fixture", "provider");
    let parent = MemorySecretProvider::new("parent").with_secret(id.clone(), b"inert-canary");
    let handoff = ParentSecretHandoff::capture(&parent, [id.clone()])
        .await
        .unwrap();
    with_parent_secret_handoff(Some(handoff), async {
        let plan = super::super::SecretChainPlan::configured();
        assert_eq!(plan.providers, ["parent-handoff"]);
        assert_eq!(plan.excluded.len(), 2);
        assert!(plan
            .excluded
            .iter()
            .all(|entry| { entry.reason == super::super::SecretProviderExclusion::ParentHandoff }));
        // Both the explicit-namespace Harness factory and the process-wide
        // connector factory must resolve through the same received owner.
        for child in [
            super::super::configured_default_chain("fixture").unwrap(),
            super::super::configured_secret_chain().unwrap(),
        ] {
            assert!(child
                .get(&id)
                .await
                .unwrap()
                .with_exposed(|value| value == b"inert-canary"));
            let error = child
                .get(&SecretId::new("fixture", "unselected"))
                .await
                .unwrap_err();
            let super::super::SecretError::NotFoundInChain(absence) = error else {
                panic!("closed store must preserve consultation and exclusion evidence");
            };
            assert_eq!(absence.consulted.len(), 1);
            assert_eq!(absence.excluded, plan.excluded);
        }
    })
    .await;
    assert!(
        active_parent_provider().is_none(),
        "scope must not contaminate later sessions"
    );
}

#[tokio::test]
async fn serialized_expansion_is_bounded_before_allocating_an_unbounded_frame() {
    let parent = MemorySecretProvider::new("parent");
    let mut ids = Vec::new();
    let mut parent = parent;
    // JSON byte arrays expand this selected raw snapshot beyond the wire bound.
    for index in 0..5 {
        let id = SecretId::new("fixture", format!("large-{index}"));
        parent.insert(id.clone(), vec![255; MAX_SECRET_BYTES]);
        ids.push(id);
    }
    let handoff = ParentSecretHandoff::capture(&parent, ids).await.unwrap();
    let mut pipe = Vec::new();
    assert!(handoff.write_to(&mut pipe).is_err());
    assert!(
        pipe.is_empty(),
        "refused frame must not partially enter the child pipe"
    );
}

#[tokio::test]
async fn received_store_preserves_command_scope_and_value_free_grant_receipts() {
    use crate::security::session_environment::{
        EnvironmentPolicyKind, GrantAudience, GrantSourceSpec, GrantSpec, SessionEnvironment,
    };
    let id = SecretId::new("fixture", "provider");
    let parent = MemorySecretProvider::new("parent").with_secret(id.clone(), b"inert-canary");
    let handoff = ParentSecretHandoff::capture(&parent, [id]).await.unwrap();
    with_parent_secret_handoff(Some(handoff), async {
        let grant = GrantSpec {
            name: "provider".into(),
            source: GrantSourceSpec::SecretStore {
                account: "fixture".into(),
                key: "provider".into(),
            },
            expose_as_env: Some("PROBE_PROVIDER_KEY".into()),
            for_command: Some("approved-probe".into()),
            expose_to: GrantAudience::Session,
        };
        let session = SessionEnvironment::launch_from_snapshot(
            EnvironmentPolicyKind::Granted,
            vec![grant],
            BTreeMap::new(),
            &|_| None,
        )
        .unwrap();
        let resolve = |account: &str, key: &str| {
            super::super::resolve_secret_ref_to_string(&format!("harn-secret://{account}/{key}"))
                .unwrap()
        };
        assert!(session.env_exposure(&resolve).unwrap().is_empty());
        assert!(session
            .env_exposure_for_command("unapproved-probe", &resolve)
            .unwrap()
            .is_empty());
        assert_eq!(
            session
                .env_exposure_for_command("approved-probe", &resolve)
                .unwrap(),
            [("PROBE_PROVIDER_KEY".into(), "inert-canary".into())]
        );
        let receipts = session.receipts();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].source_kind, "secret_store");
        assert_eq!(receipts[0].for_command.as_deref(), Some("approved-probe"));
        let encoded = serde_json::to_string(&receipts).unwrap();
        assert!(!encoded.contains("inert-canary"));
        assert!(!encoded.contains("fixture"));
    })
    .await;
}
