use harn_vm::connectors::testkit::{
    scoped_secret_id, ConnectorTestkit, HttpMockGuard, HttpMockResponse, MemorySecretProvider,
};
use harn_vm::secrets::{SecretBytes, SecretId};
use time::OffsetDateTime;

fn parse_ts(value: &str) -> OffsetDateTime {
    OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).unwrap()
}

#[tokio::test]
async fn connector_testkit_is_usable_from_external_tests() {
    let secret_id = scoped_secret_id("github", "tenant-a", "binding-a", "token");
    let secrets = MemorySecretProvider::new("github").with_secret(secret_id.clone(), "token-v1");
    let kit = ConnectorTestkit::with_secrets(parse_ts("2026-04-19T00:00:00Z"), secrets).await;
    let ctx = kit.ctx();

    let token = ctx.secrets.get(&secret_id).await.expect("scoped secret");
    assert_eq!(
        token.with_exposed(|bytes| bytes.to_vec()),
        b"token-v1".to_vec()
    );

    ctx.secrets
        .put(
            &SecretId::new("github", "plain"),
            SecretBytes::from("plain"),
        )
        .await
        .expect("put secret");

    let http = HttpMockGuard::new();
    http.push(
        "GET",
        "https://api.example.com/*",
        vec![HttpMockResponse::new(200, "ok")],
    );
    assert!(http.calls().is_empty());
}
