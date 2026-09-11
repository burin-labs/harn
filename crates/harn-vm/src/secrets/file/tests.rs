use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use super::*;
use crate::secrets::{SecretAuditContext, SecretScope};

fn store() -> (tempfile::TempDir, FileSecretProvider) {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let provider = FileSecretProvider::new(directory.path().join("secrets.json")).unwrap();
    (directory, provider)
}

fn delete_request(id: SecretId) -> SecretDeleteRequest {
    SecretDeleteRequest {
        id,
        scope: SecretScope::system(),
        audit: SecretAuditContext::default(),
    }
}

#[test]
fn flat_keys_match_swift_encoding_and_keep_versions_distinct() {
    // Produced with Foundation's actual HarnSecretKey character sets, including
    // non-ASCII letters and combining marks rather than an ASCII-only example.
    for (plain, encoded) in [
        ("ascii", "ascii"),
        ("café", "caf%C3%A9"),
        ("用户", "%E7%94%A8%E6%88%B7"),
        ("a\u{301}", "a%CC%81"),
        ("💡", "%F0%9F%92%A1"),
        ("space :@#?%", "space%20%3A%40%23%3F%25"),
    ] {
        let id = SecretId::new(plain, plain);
        let key = format!("{encoded}/{encoded}");
        assert_eq!(storage_key(&id), key);
        assert_eq!(decode_storage_key(&key), Some(id));
    }
    let id = SecretId::new("n/slash", "name/slash#v7").with_version(SecretVersion::Exact(9));
    assert_eq!(storage_key(&id), "n%2Fslash/name/slash%23v7#v9");
    assert_eq!(decode_storage_key(&storage_key(&id)), Some(id));
    assert!(decode_storage_key("namespace/%GG").is_none());
    assert!(decode_storage_key("namespace/%FF").is_none());
}

#[tokio::test]
async fn independent_instances_preserve_other_keys_and_revoke_exact_values() {
    let (_directory, first) = store();
    let second = FileSecretProvider::new(first.path.clone()).unwrap();
    let latest = SecretId::new("n/slash", "token");
    let exact = latest.clone().with_version(SecretVersion::Exact(7));
    first
        .put(&latest, SecretBytes::from(b"latest".as_slice()))
        .await
        .unwrap();
    second
        .put(&exact, SecretBytes::from(b"version".as_slice()))
        .await
        .unwrap();
    assert_eq!(
        first
            .get(&latest)
            .await
            .unwrap()
            .with_exposed(|bytes| bytes.to_vec()),
        b"latest"
    );
    assert_eq!(
        second
            .get(&exact)
            .await
            .unwrap()
            .with_exposed(|bytes| bytes.to_vec()),
        b"version"
    );
    assert_eq!(
        first
            .list(&SecretId::new("n/slash", "tok"))
            .await
            .unwrap()
            .len(),
        2
    );
    first
        .delete_scoped(delete_request(latest.clone()))
        .await
        .unwrap();
    assert!(second.get(&latest).await.unwrap_err().is_not_found());
    assert!(second.get(&exact).await.is_ok());
    first.delete_scoped(delete_request(latest)).await.unwrap();
    assert_eq!(
        fs::metadata(&first.path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[tokio::test]
async fn injected_provider_resolves_references_and_observes_revoke() {
    let (_directory, provider) = store();
    let id = SecretId::new("application", "token");
    provider
        .put(&id, SecretBytes::from(b"synthetic-only".as_slice()))
        .await
        .unwrap();
    let harness = crate::Harness::default().with_secret_provider(Arc::new(provider.clone()));
    crate::secrets::with_active_secret_provider(harness.secret_provider().cloned(), async {
        assert_eq!(
            crate::secrets::resolve_secret_ref_to_string("harn-secret://application/token")
                .unwrap(),
            Some("synthetic-only".into())
        );
        provider.delete_scoped(delete_request(id)).await.unwrap();
        assert!(
            crate::secrets::resolve_secret_ref_to_string("harn-secret://application/token")
                .unwrap_err()
                .is_not_found()
        );
    })
    .await;
}

#[tokio::test]
async fn malformed_and_exposed_files_are_not_replaced_or_reported_absent() {
    let (_directory, provider) = store();
    let id = SecretId::new("n", "token");
    let malformed = b"{\"synthetic-private-canary\": not-json}";
    fs::write(&provider.path, malformed).unwrap();
    fs::set_permissions(&provider.path, fs::Permissions::from_mode(0o600)).unwrap();
    let error = provider
        .put(&id, SecretBytes::from(b"new".as_slice()))
        .await
        .unwrap_err();
    assert!(!error.is_not_found());
    assert!(!error.to_string().contains("synthetic-private-canary"));
    assert_eq!(fs::read(&provider.path).unwrap(), malformed);

    fs::write(&provider.path, b"{}").unwrap();
    fs::set_permissions(&provider.path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(provider
        .get(&id)
        .await
        .unwrap_err()
        .to_string()
        .contains("owner-only"));
    assert!(provider
        .put(&id, SecretBytes::from(b"new".as_slice()))
        .await
        .is_err());
    assert_eq!(fs::read(&provider.path).unwrap(), b"{}");
}

#[tokio::test]
async fn corrupt_value_and_symlink_remain_errors() {
    let (_directory, provider) = store();
    let id = SecretId::new("n", "token");
    fs::write(&provider.path, br#"{"n/token":"not-base64!"}"#).unwrap();
    fs::set_permissions(&provider.path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(provider
        .get(&id)
        .await
        .unwrap_err()
        .to_string()
        .contains("base64"));
    let link = provider.path.with_file_name("link.json");
    std::os::unix::fs::symlink(&provider.path, &link).unwrap();
    let other = FileSecretProvider::new(link).unwrap();
    assert!(!other.get(&id).await.unwrap_err().is_not_found());
    assert!(other
        .put(&id, SecretBytes::from(b"new".as_slice()))
        .await
        .is_err());
    assert_eq!(
        fs::read(&provider.path).unwrap(),
        br#"{"n/token":"not-base64!"}"#
    );
}

#[tokio::test]
async fn sqlite_host_write_is_preserved_by_provider_read_modify_write() {
    let (_directory, provider) = store();
    prepare_directory(provider.path.parent().unwrap()).unwrap();
    let lock_path = PathBuf::from(format!("{}.lock.sqlite3", provider.path.display()));
    let _lock_file = open_private(&lock_path, true)
        .map_err(SecretError::from)
        .unwrap();
    let mut host = Connection::open(&lock_path).unwrap();
    host.execute_batch("PRAGMA journal_mode=DELETE").unwrap();
    let transaction = host
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    // Assert the host's wire protocol and preservation deterministically.
    // Contention deadlines and crashed lock owners belong to process-level
    // integration proof, not a race against a short unit-test wall clock.
    crate::atomic_io::atomic_write_with_mode(&provider.path, br#"{"n/host":"aG9zdA=="}"#, 0o600)
        .unwrap();
    transaction.commit().unwrap();
    provider
        .put(
            &SecretId::new("n", "harn"),
            SecretBytes::from(b"harn".as_slice()),
        )
        .await
        .unwrap();
    let persisted: BTreeMap<String, String> =
        serde_json::from_slice(&fs::read(&provider.path).unwrap()).unwrap();
    assert_eq!(persisted.len(), 2);
    assert_eq!(persisted["n/host"], "aG9zdA==");
    assert_eq!(persisted["n/harn"], "aGFybg==");
}

#[tokio::test]
async fn a_successful_file_delete_cannot_hide_a_remaining_read_only_copy() {
    use crate::secrets::{ChainSecretProvider, MemorySecretProvider};
    let (_directory, file) = store();
    let id = SecretId::new("n", "token");
    file.put(&id, SecretBytes::from(b"file-copy".as_slice()))
        .await
        .unwrap();
    let read_only =
        MemorySecretProvider::new("read-only").with_secret(id.clone(), "remaining-copy");
    let chain = ChainSecretProvider::new("n", vec![Arc::new(file.clone()), Arc::new(read_only)]);
    assert!(chain
        .delete_scoped(delete_request(id.clone()))
        .await
        .is_err());
    assert!(file.get(&id).await.unwrap_err().is_not_found());
    assert_eq!(
        chain
            .get(&id)
            .await
            .unwrap()
            .with_exposed(|bytes| bytes.to_vec()),
        b"remaining-copy"
    );

    let absent = MemorySecretProvider::new("empty-read-only");
    let chain = ChainSecretProvider::new("n", vec![Arc::new(file), Arc::new(absent)]);
    chain.delete_scoped(delete_request(id)).await.unwrap();
}
