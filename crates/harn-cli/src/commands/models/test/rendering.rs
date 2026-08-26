//! Host bridge for the `harn models test` renderer.
//!
//! This module owns the envelope boundary between the Rust smoke test and the
//! embedded Harn renderer. Keeping that boundary here gives every caller the
//! same scoped environment, serialization failure, and terminal forwarding
//! behavior.

use std::io::Write as _;

use serde::Serialize;

use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

/// Env var carrying the smoke-test outcome (success result or error) handed to
/// the embedded `cli/models/test` script. The script selects its format from
/// `HARN_OUTPUT_JSON`.
const TEST_RESULT_ENV: &str = "HARN_MODELS_TEST_RESULT_JSON";

/// Serializes the dispatch path so concurrent in-process callers cannot race
/// on the process-global envelope variable.
static DISPATCH_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Envelope the Rust shim hands to the `.harn` script. It mirrors the success
/// and failure shapes the embedded renderer presents to users.
#[derive(Debug, Serialize)]
struct TestEnvelope<'a> {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a harn_vm::llm::ModelSmokeTestResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

pub(super) async fn render(
    result: &Result<harn_vm::llm::ModelSmokeTestResult, String>,
    json_mode: bool,
) -> i32 {
    let envelope = match result {
        Ok(value) => TestEnvelope {
            ok: true,
            result: Some(value),
            error: None,
        },
        Err(error) => TestEnvelope {
            ok: false,
            result: None,
            error: Some(error),
        },
    };
    let envelope_json = match serde_json::to_string(&envelope) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to serialise models-test envelope: {error}");
            return 1;
        }
    };

    let _guard = DISPATCH_TEST_LOCK.lock().await;
    let _payload = ScopedEnvVar::set(TEST_RESULT_ENV, &envelope_json);
    let outcome = dispatch::run_embedded_script("models/test", Vec::new(), json_mode).await;
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if !outcome.stdout.is_empty() {
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
    outcome.exit_code
}

#[cfg(test)]
mod tests {
    use super::{DISPATCH_TEST_LOCK, TEST_RESULT_ENV};
    use crate::dispatch;
    use crate::env_guard::ScopedEnvVar;

    #[tokio::test]
    async fn renderer_preserves_unavailable_zero_and_sub_dollar_model_test_costs() {
        let envelope = serde_json::json!({
            "ok": true,
            "result": {
                "model_id": "provider-model",
                "provider": "provider",
                "latency_ms": 10,
                "input_tokens": 14,
                "output_tokens": 6,
                "estimated_cost_usd": null,
            },
        });

        let _guard = DISPATCH_TEST_LOCK.lock().await;
        let _payload = ScopedEnvVar::set(
            TEST_RESULT_ENV,
            &serde_json::to_string(&envelope).expect("models-test envelope json"),
        );

        let human = dispatch::run_embedded_script("models/test", Vec::new(), false).await;
        assert_eq!(human.exit_code, 0, "stderr={}", human.stderr);
        assert!(
            human.stdout.contains("estimated_cost_usd=unavailable"),
            "stdout={}",
            human.stdout
        );

        let json = dispatch::run_embedded_script("models/test", Vec::new(), true).await;
        assert_eq!(json.exit_code, 0, "stderr={}", json.stderr);
        let value: serde_json::Value =
            serde_json::from_str(&json.stdout).expect("models-test JSON result");
        assert_eq!(value["estimated_cost_usd"], serde_json::Value::Null);

        let zero_envelope = serde_json::json!({
            "ok": true,
            "result": {
                "model_id": "provider-model",
                "provider": "provider",
                "latency_ms": 10,
                "input_tokens": 14,
                "output_tokens": 6,
                "estimated_cost_usd": 0.0,
            },
        });
        let _zero_payload = ScopedEnvVar::set(
            TEST_RESULT_ENV,
            &serde_json::to_string(&zero_envelope).expect("models-test zero-cost envelope json"),
        );

        let zero_human = dispatch::run_embedded_script("models/test", Vec::new(), false).await;
        assert_eq!(zero_human.exit_code, 0, "stderr={}", zero_human.stderr);
        assert!(
            zero_human.stdout.contains("estimated_cost_usd=0"),
            "stdout={}",
            zero_human.stdout
        );

        let zero_json = dispatch::run_embedded_script("models/test", Vec::new(), true).await;
        assert_eq!(zero_json.exit_code, 0, "stderr={}", zero_json.stderr);
        let zero_value: serde_json::Value =
            serde_json::from_str(&zero_json.stdout).expect("models-test zero-cost JSON result");
        assert_eq!(zero_value["estimated_cost_usd"].as_f64(), Some(0.0));

        let paid_envelope = serde_json::json!({
            "ok": true,
            "result": {
                "model_id": "provider-model",
                "provider": "provider",
                "latency_ms": 10,
                "input_tokens": 89,
                "output_tokens": 32,
                "estimated_cost_usd": 0.000747,
            },
        });
        let _paid_payload = ScopedEnvVar::set(
            TEST_RESULT_ENV,
            &serde_json::to_string(&paid_envelope).expect("models-test paid envelope json"),
        );

        let paid_human = dispatch::run_embedded_script("models/test", Vec::new(), false).await;
        assert_eq!(paid_human.exit_code, 0, "stderr={}", paid_human.stderr);
        assert!(
            paid_human.stdout.contains("estimated_cost_usd=0.000747"),
            "stdout={}",
            paid_human.stdout
        );

        let paid_json = dispatch::run_embedded_script("models/test", Vec::new(), true).await;
        assert_eq!(paid_json.exit_code, 0, "stderr={}", paid_json.stderr);
        let paid_value: serde_json::Value =
            serde_json::from_str(&paid_json.stdout).expect("models-test paid JSON result");
        assert_eq!(paid_value["estimated_cost_usd"].as_f64(), Some(0.000747));
    }
}
