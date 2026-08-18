//! Behaviour tests for the reusable session-store primitive.
//!
//! The runners exercise the memory and SQLite backends against the same scenarios.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::json;
use tempfile::TempDir;

use harn_session_store::*;

#[derive(Clone)]
struct TestRedactor;

impl EventRedactor for TestRedactor {
    fn redact_json_in_place(&self, value: &mut serde_json::Value) {
        if let Some(object) = value.as_object_mut() {
            if object.contains_key("api_key") {
                object.insert("api_key".to_string(), json!("[redacted]"));
            }
            for value in object.values_mut() {
                if value
                    .as_str()
                    .is_some_and(|text| text.contains("known-secret-value"))
                {
                    *value = json!("[redacted]");
                }
            }
        }
    }

    fn redact_headers(
        &self,
        headers: &std::collections::BTreeMap<String, String>,
    ) -> std::collections::BTreeMap<String, String> {
        headers
            .iter()
            .map(|(name, value)| {
                let value = if name == "authorization" {
                    "[redacted]".to_string()
                } else {
                    value.clone()
                };
                (name.clone(), value)
            })
            .collect()
    }
}

#[derive(Clone)]
struct IdentityClobberingRedactor;

impl EventRedactor for IdentityClobberingRedactor {
    fn redact_json_in_place(&self, _value: &mut serde_json::Value) {}

    fn redact_headers(
        &self,
        headers: &std::collections::BTreeMap<String, String>,
    ) -> std::collections::BTreeMap<String, String> {
        let mut headers = headers.clone();
        headers.insert("run_id".to_string(), "[redacted]".to_string());
        headers
    }
}

#[derive(Clone)]
struct SwitchableRedactor {
    enabled: Arc<AtomicBool>,
    clobber_identity: bool,
}

impl EventRedactor for SwitchableRedactor {
    fn redact_json_in_place(&self, value: &mut serde_json::Value) {
        if self.enabled.load(Ordering::SeqCst) {
            TestRedactor.redact_json_in_place(value);
        }
    }

    fn redact_headers(
        &self,
        headers: &std::collections::BTreeMap<String, String>,
    ) -> std::collections::BTreeMap<String, String> {
        if !self.enabled.load(Ordering::SeqCst) {
            return headers.clone();
        }
        let mut headers = TestRedactor.redact_headers(headers);
        if self.clobber_identity {
            headers.insert("run_id".to_string(), "[redacted]".to_string());
        }
        headers
    }
}

#[derive(Clone)]
struct TestSemanticEmbedder;

impl Embedder for TestSemanticEmbedder {
    fn embed(&self, text: &str) -> Vec<f32> {
        let folded = text.to_ascii_lowercase();
        if folded.contains("shipping")
            || folded.contains("release")
            || folded.contains("deployment")
        {
            vec![1.0, 0.0]
        } else {
            vec![0.0, 1.0]
        }
    }

    fn dim(&self) -> usize {
        2
    }

    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "test-semantic"
    }

    // Claimed explicitly, because the claim is what these tests are about:
    // they cover how the store routes a `Semantic`/`Hybrid` query once a
    // semantic backend is present, which is unreachable from the floor. This
    // is a hand-built double over a two-word vocabulary, not a product
    // backend, so it is deliberately not held to
    // `search::conformance::assert_semantic_claim_is_earned`.
    fn is_semantic(&self) -> bool {
        true
    }
}

fn dummy_signer(seed: u8) -> SessionSigner {
    SessionSigner::from_seed([seed; 32])
}

fn fresh_memory(hooks: StoreHooks) -> Arc<dyn SessionImporter> {
    Arc::new(MemorySessionStore::with_hooks(hooks))
}

fn fresh_sqlite(hooks: StoreHooks, dir: &TempDir) -> Arc<dyn SessionImporter> {
    let path = dir.path().join("sessions.sqlite");
    Arc::new(SqliteSessionStore::open_with_hooks(path, hooks).expect("open sqlite"))
}

async fn run_with_hooks<F, Fut>(hooks: StoreHooks, body: F)
where
    F: Fn(Arc<dyn SessionImporter>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    body(fresh_memory(hooks.clone())).await;
    let dir = TempDir::new().expect("tempdir");
    body(fresh_sqlite(hooks, &dir)).await;
}

mod core;
mod search;
