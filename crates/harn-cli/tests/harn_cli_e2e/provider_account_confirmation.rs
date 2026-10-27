//! Account evidence must bind both independent Gemini settings to the selected key.

use crate::test_util::process::harn_e2e_command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[tokio::test]
async fn gemini_confirmation_requires_billing_and_contributor_sharing_off() {
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
                "id": "local-confirmation-control",
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
    std::fs::write(temp.path().join("main.harn"), source).unwrap();
    for (billing, sharing, credential, duplicate, project_restricts, allowed) in [
        (Some(true), Some(false), "synthetic", false, false, true),
        (Some(true), Some(true), "synthetic", false, false, false),
        (Some(true), None, "synthetic", false, false, false),
        (None, Some(false), "synthetic", false, false, false),
        (Some(false), Some(false), "synthetic", false, false, false),
        (
            Some(true),
            Some(false),
            "different-synthetic-key",
            false,
            false,
            false,
        ),
        (Some(true), Some(false), "synthetic", true, false, false),
        (Some(true), Some(false), "synthetic", false, true, false),
    ] {
        let mut attest = harn_e2e_command();
        attest
            .current_dir(temp.path())
            .env("XDG_CONFIG_HOME", temp.path())
            .env("GEMINI_API_KEY", "synthetic")
            .env_remove("HARN_PROVIDER_ACCOUNT_ATTESTATIONS_JSON")
            .env_remove("HARN_INFERENCE_BOUNDARY_JSON")
            .args(["provider", "attest-account", "--provider", "gemini"]);
        if let Some(value) = billing {
            attest.args(["--billing-enabled", if value { "true" } else { "false" }]);
        }
        if let Some(value) = sharing {
            attest.args([
                "--contributor-sharing-enabled",
                if value { "true" } else { "false" },
            ]);
        }
        let output = tokio::task::spawn_blocking(move || attest.output().unwrap())
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let attestation = if duplicate {
            serde_json::json!([record, record])
        } else {
            serde_json::json!([record])
        };
        if !project_restricts {
            let request = serde_json::json!({"provider": "gemini", "model": "gemini-4-argon", "boundary": {"reach": "any_hosted", "allow_training_discounts": false}});
            let mut preview = harn_e2e_command();
            preview
                .current_dir(temp.path())
                .env("XDG_CONFIG_HOME", temp.path())
                .env("HARN_HOST_PROVIDERS_CONFIG", &config)
                .env("GEMINI_API_KEY", credential)
                .env(
                    "HARN_PROVIDER_ACCOUNT_ATTESTATIONS_JSON",
                    attestation.to_string(),
                )
                .env_remove("HARN_INFERENCE_BOUNDARY_JSON")
                .env_remove("HARN_PROVIDERS_CONFIG")
                .env_remove("GEMINI_BASE_URL")
                .args(["provider", "admission", "--request", &request.to_string()]);
            let output = tokio::task::spawn_blocking(move || preview.output().unwrap())
                .await
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let snapshot: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(
                snapshot["status"],
                if allowed { "admitted" } else { "unknown" },
                "{snapshot}"
            );
            assert_eq!(
                snapshot["required_account_facts"],
                serde_json::json!(["billing_enabled", "contributor_sharing_disabled"])
            );
            assert_eq!(
                snapshot["account_confirmation_applied"]
                    .as_bool()
                    .unwrap_or(false),
                allowed,
                "{snapshot}"
            );
        }
        if project_restricts {
            std::fs::write(
                temp.path().join("harn.toml"),
                r#"
[llm.providers.gemini.data_controls]
control_scope = "none"
retention_default = "unspecified"
training_default = "trains"
checked_on = "2026-10-27"
sources = ["https://example.invalid/project-restriction"]

[llm.models.gemini-4-argon]
name = "Gemini 4 Argon"
provider = "gemini"
context_window = 1048576

[llm.models.gemini-4-argon.data_controls]
training_default = "does_not_train"
checked_on = "2026-10-27"
sources = ["https://example.invalid/project-model-claim"]
"#,
            )
            .unwrap();
        }
        let mut command = harn_e2e_command();
        command
            .current_dir(temp.path())
            .env("XDG_CONFIG_HOME", temp.path())
            .env("HARN_HOST_PROVIDERS_CONFIG", &config)
            .env("HARN_EGRESS_ALLOW_LOOPBACK", "1")
            .env("HARN_LLM_CALLS_DISABLED", "0")
            .env("HARN_LLM_RATE_LIMIT_DURABLE", "0")
            .env("GEMINI_API_KEY", credential)
            .env(
                "HARN_PROVIDER_ACCOUNT_ATTESTATIONS_JSON",
                attestation.to_string(),
            )
            .env(
                "HARN_INFERENCE_BOUNDARY_JSON",
                r#"{"reach":"any_hosted","allow_training_discounts":false}"#,
            )
            .env_remove("HARN_PROVIDERS_CONFIG")
            .env_remove("GEMINI_BASE_URL")
            .args(["run", "main.harn"]);
        let output = tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap();
        assert_eq!(
            output.status.success(),
            allowed,
            "billing={billing:?} sharing={sharing:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if !allowed {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let expected_rule = if project_restricts {
                "inference_boundary.training_default"
            } else {
                "inference_boundary.training_unknown"
            };
            assert!(stderr.contains(expected_rule), "{stderr}");
        }
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "only the fully confirmed account may reach the provider"
        );
    }
    server.abort();
}

#[cfg(unix)]
#[test]
fn account_confirmation_is_not_exposed_to_scripts_or_child_processes() {
    let temp = tempfile::tempdir().unwrap();
    let source = r#"
fn main(harness: Harness) {
  const child = harness.process.run({program: "/usr/bin/env", args: []})
  harness.stdio.log(json_stringify({
    script_hidden: harness.env.get("HARN_PROVIDER_ACCOUNT_ATTESTATIONS_JSON") == nil,
    child_hidden: !child.stdout.contains("HARN_PROVIDER_ACCOUNT_ATTESTATIONS_JSON="),
    sentinel_retained: child.stdout.contains("ACCOUNT_TEST_SENTINEL=retained"),
  }))
}
"#;
    let output = harn_e2e_command()
        .current_dir(temp.path())
        .env("HARN_PROVIDER_ACCOUNT_ATTESTATIONS_JSON", "[]")
        .env("ACCOUNT_TEST_SENTINEL", "retained")
        .env_remove("HARN_INFERENCE_BOUNDARY_JSON")
        .args(["run", "-e", source])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"script_hidden\":true"), "{stdout}");
    assert!(stdout.contains("\"child_hidden\":true"), "{stdout}");
    assert!(stdout.contains("\"sentinel_retained\":true"), "{stdout}");

    let malformed = harn_e2e_command()
        .current_dir(temp.path())
        .env("HARN_PROVIDER_ACCOUNT_ATTESTATIONS_JSON", "not-valid-json")
        .args(["run", "-e", source])
        .output()
        .unwrap();
    assert!(!malformed.status.success());
    let stderr = String::from_utf8_lossy(&malformed.stderr);
    assert!(
        stderr.contains("inference_boundary.host_account_attestations_malformed"),
        "{stderr}"
    );
    assert!(!stderr.contains("not-valid-json"));
}
