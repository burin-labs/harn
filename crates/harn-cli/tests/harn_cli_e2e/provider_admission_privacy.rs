//! Account-dependent provider terms must not become unconditional no-training claims.

use crate::test_util::process::harn_e2e_command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[tokio::test]
pub(crate) async fn provider_endpoint_configuration_precedes_policy_without_transport() {
    let temp = tempfile::tempdir().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let observed = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new().fallback(axum::routing::any(move || {
        let observed = observed.clone();
        async move {
            observed.fetch_add(1, Ordering::SeqCst);
            axum::http::StatusCode::OK
        }
    }));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let endpoint = format!("http://{address}/v1");
    reqwest::get(&endpoint)
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        requests.swap(0, Ordering::SeqCst),
        1,
        "request observer must be live"
    );
    let request = serde_json::json!({
        "provider": "zai", "model": "glm-5.3",
        "boundary": {"reach": "any_hosted", "allow_training_discounts": false}
    });
    for (override_url, configuration_error) in [
        (
            Some("https://userinfo:credential-marker@[invalid/path-marker"),
            Some("invalid_url"),
        ),
        (Some("file:///credential-marker"), Some("not_absolute_http")),
        (Some(endpoint.as_str()), None),
        (None, None),
    ] {
        let mut command = harn_e2e_command();
        command
            .current_dir(temp.path())
            .env("XDG_CONFIG_HOME", temp.path())
            .env_remove("HARN_INFERENCE_BOUNDARY_JSON")
            .env_remove("HARN_HOST_PROVIDERS_CONFIG")
            .env_remove("HARN_PROVIDERS_CONFIG")
            .env_remove("ZAI_REGION")
            .env_remove("ZAI_BASE_URL")
            .args(["provider", "admission", "--request", &request.to_string()]);
        if let Some(endpoint) = override_url {
            command.env("ZAI_BASE_URL", endpoint);
        }
        let output = tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.success(), configuration_error.is_none());
        if let Some(reason) = configuration_error {
            assert_eq!(value, serde_json::json!({"configuration_error": reason}));
        } else if override_url.is_some() {
            assert_eq!(value["status"], "unknown");
            assert_eq!(
                value["governing_rule"],
                "inference_boundary.training_unknown"
            );
        } else {
            assert_eq!(value["status"], "admitted");
        }
        for bytes in [&output.stdout, &output.stderr] {
            let text = String::from_utf8_lossy(bytes);
            for marker in ["userinfo", "credential-marker", "path-marker"] {
                assert!(
                    !text.contains(marker),
                    "diagnostic exposed supplied endpoint values"
                );
            }
        }
        assert_eq!(
            requests.load(Ordering::SeqCst),
            0,
            "admission must not contact the provider"
        );
    }
    server.abort();
    let _ = server.await;
}

#[test]
pub(crate) fn admission_requires_account_evidence_for_conditional_provider_terms() {
    let temp = tempfile::tempdir().unwrap();
    for (provider, model, expected) in [
        ("gemini", "gemini-4-argon", "unknown"),
        ("mistral", "mistral-medium-3-5", "unknown"),
        ("openai", "gpt-5.6-luna", "admitted"),
    ] {
        let request = serde_json::json!({
            "provider": provider,
            "model": model,
            "boundary": {"reach": "any_hosted", "allow_training_discounts": false}
        });
        let result = harn_e2e_command()
            .current_dir(temp.path())
            .env("XDG_CONFIG_HOME", temp.path())
            .env_remove("HARN_INFERENCE_BOUNDARY_JSON")
            .env_remove("HARN_HOST_PROVIDERS_CONFIG")
            .env_remove("HARN_PROVIDERS_CONFIG")
            .env_remove("GEMINI_BASE_URL")
            .env_remove("MISTRAL_BASE_URL")
            .env_remove("OPENAI_BASE_URL")
            .args(["provider", "admission", "--request", &request.to_string()])
            .output()
            .expect("run provider admission");
        assert!(
            result.status.success(),
            "admission command failed for {provider}"
        );
        let snapshot: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(snapshot["status"], expected, "{provider}: {snapshot}");
        if expected == "unknown" {
            assert_eq!(
                snapshot["governing_rule"],
                "inference_boundary.training_unknown"
            );
            let note = snapshot["data_controls_note"].as_str().unwrap();
            if provider == "gemini" {
                assert!(
                    note.contains("Contributor Tier sharing is turned off"),
                    "{snapshot}"
                );
            } else {
                assert!(note.contains("cannot confirm"), "{provider}: {snapshot}");
            }
        }
    }
}

#[tokio::test]
pub(crate) async fn unknown_gemini_account_is_refused_before_transport() {
    let temp = tempfile::tempdir().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let observed = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new().fallback(axum::routing::post(move || {
        let observed = observed.clone();
        async move {
            observed.fetch_add(1, Ordering::SeqCst);
            axum::Json(serde_json::json!({
                "id": "local-positive-control",
                "status": "completed",
                "steps": [{"type": "model_output", "content": [{"type": "text", "text": "accepted"}]}],
                "usage": {"total_input_tokens": 1, "total_output_tokens": 1}
            }))
        }
    }));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = temp.path().join("host.toml");
    std::fs::write(
        &config,
        format!("[providers.gemini]\nbase_url = 'http://{address}'\n"),
    )
    .unwrap();
    let source = "fn main(harness: Harness) { harness.llm.call(\"probe\", nil, {provider: \"gemini\", model: \"gemini-4-argon\", stream: false}) }";
    let call = |bounded: bool| {
        let mut command = harn_e2e_command();
        command
            .current_dir(temp.path())
            .env("XDG_CONFIG_HOME", temp.path())
            .env("HARN_HOST_PROVIDERS_CONFIG", &config)
            .env("HARN_EGRESS_ALLOW_LOOPBACK", "1")
            .env("HARN_LLM_CALLS_DISABLED", "0")
            .env("HARN_LLM_RATE_LIMIT_DURABLE", "0")
            .env("GEMINI_API_KEY", "synthetic")
            .env_remove("HARN_PROVIDERS_CONFIG")
            .env_remove("GEMINI_BASE_URL")
            .env_remove("HARN_INFERENCE_BOUNDARY_JSON")
            .args(["run", "-e", source]);
        if bounded {
            command.env(
                "HARN_INFERENCE_BOUNDARY_JSON",
                r#"{"reach":"any_hosted","allow_training_discounts":false}"#,
            );
        }
        command
    };
    let mut allowed = call(false);
    let positive = tokio::task::spawn_blocking(move || allowed.output().unwrap())
        .await
        .unwrap();
    assert!(
        positive.status.success(),
        "{}",
        String::from_utf8_lossy(&positive.stderr)
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "positive control reached provider"
    );
    let mut bounded = call(true);
    let refused = tokio::task::spawn_blocking(move || bounded.output().unwrap())
        .await
        .unwrap();
    assert!(
        !refused.status.success(),
        "unknown account must not reach provider"
    );
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(
        error.contains("inference_boundary.training_unknown"),
        "{error}"
    );
    assert!(error.contains("Contributor Tier"), "{error}");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "bounded call sent no request"
    );
    server.abort();
}
