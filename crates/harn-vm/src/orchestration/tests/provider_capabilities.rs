//! Endpoint measurements through execution-scoped configuration and catalog projection.

use crate::llm::capabilities::{self, CapabilityProbeStatus};
use crate::llm_config::{parse_config_toml, RuntimeProviderEndpointOverrides};
use crate::orchestration::scope_llm_runtime_overrides_with_provider_endpoints;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn one_response(status: &'static str, body: &'static str) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind probe stub");
    let addr = listener.local_addr().expect("probe stub address");
    let hits = Arc::new(AtomicUsize::new(0));
    let server_hits = hits.clone();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept probe request");
        let mut request = vec![0; 4096];
        let _ = socket.read(&mut request).await.expect("read probe request");
        server_hits.fetch_add(1, Ordering::SeqCst);
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("write probe response");
    });
    (format!("http://{addr}"), hits)
}

#[tokio::test(flavor = "current_thread")]
async fn live_template_measurement_overrides_a_stale_catalog_answer_once() {
    let (base_url, hits) = one_response("200 OK", r#"{"chat_template_caps":{"supports_tools":true,"supports_tool_calls":false,"supports_parallel_tool_calls":true}}"#).await;
    let provider = "llamacpp";
    let model = format!("stale-native-{}", uuid::Uuid::now_v7());
    let config = parse_config_toml(&format!(
        "[models.\"{model}\"]\nname = \"Measured fixture\"\nprovider = \"llamacpp\"\ncontext_window = 8192\n"
    )).expect("probe model catalog");
    let declared = capabilities::parse_capabilities_toml(&format!(
        "[[provider.llamacpp]]\nmodel_match = \"{model}\"\nnative_tools = true\n"
    ))
    .expect("stale declared capabilities");
    let unmeasured = crate::provider_catalog::artifact_embedded(Some(&config), Some(&declared));
    assert!(
        unmeasured
            .models
            .iter()
            .find(|row| row.id == model)
            .unwrap()
            .tool_support
            .native
    );
    let endpoints = RuntimeProviderEndpointOverrides::single(provider, base_url)
        .expect("verified fixture endpoint");
    scope_llm_runtime_overrides_with_provider_endpoints(
        Some(config),
        Some(declared),
        endpoints,
        async {
            assert!(capabilities::ensure_runtime_probe(provider, &model)
                .await
                .unwrap());
            assert!(!capabilities::ensure_runtime_probe(provider, &model)
                .await
                .unwrap());
            assert_eq!(
                hits.load(Ordering::SeqCst),
                1,
                "probe must be single-flight cached"
            );
            let caps = capabilities::lookup(provider, &model);
            assert!(
                !caps.native_tools,
                "loaded template must beat the stale catalog row"
            );
            assert_eq!(caps.preferred_tool_format.as_deref(), Some("json"));
            assert_eq!(caps.tool_mode_parity.as_deref(), Some("text_only"));
            assert_eq!(
                caps.runtime_probe.as_ref().map(|receipt| receipt.status),
                Some(CapabilityProbeStatus::Measured)
            );
            let measured = crate::provider_catalog::artifact();
            let row = measured
                .models
                .iter()
                .find(|row| row.id == model)
                .expect("measured row");
            assert!(!row.tool_support.native);
            assert_eq!(
                row.tool_support.parity_source.as_deref(),
                Some("runtime_probe")
            );
        },
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn unsupported_probe_keeps_the_conservative_answer_and_records_why() {
    let (base_url, _) = one_response("404 Not Found", r#"{"error":"not found"}"#).await;
    let provider = "mlx";
    let model = format!("unknown-{}", uuid::Uuid::now_v7());
    let endpoints = RuntimeProviderEndpointOverrides::single(provider, base_url)
        .expect("verified fixture endpoint");
    scope_llm_runtime_overrides_with_provider_endpoints(None, None, endpoints, async {
        assert!(capabilities::ensure_runtime_probe(provider, &model)
            .await
            .unwrap());
        let caps = capabilities::lookup(provider, &model);
        assert!(!caps.native_tools);
        let receipt = caps.runtime_probe.expect("unavailable probe receipt");
        assert_eq!(receipt.status, CapabilityProbeStatus::Unavailable);
        assert!(receipt.detail.contains("HTTP 404"), "{}", receipt.detail);
    })
    .await;
}
