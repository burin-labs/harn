use std::io::Write as _;

use serde::Serialize;

use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

/// Serialises the dispatch path so concurrent in-process callers do not race on
/// the env vars that carry the Rust-collected adapter/catalog facts.
static LORA_RENDER_DISPATCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn render_embedded_lora_report<T: Serialize>(
    report: &T,
    payload_env: &'static str,
    pretty_env: &'static str,
    script_name: &'static str,
    json: bool,
    label: &str,
) -> i32 {
    let outcome =
        match run_embedded_lora_report(report, payload_env, pretty_env, script_name, json, label)
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                eprintln!("error: {error}");
                return 1;
            }
        };
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if !outcome.stdout.is_empty() {
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
    outcome.exit_code
}

pub(crate) async fn run_embedded_lora_report<T: Serialize>(
    report: &T,
    payload_env: &'static str,
    pretty_env: &'static str,
    script_name: &'static str,
    json: bool,
    label: &str,
) -> Result<crate::commands::run::RunOutcome, String> {
    let payload_json = match serde_json::to_string(report) {
        Ok(json) => json,
        Err(error) => return Err(format!("failed to serialise {label} payload: {error}")),
    };
    let pretty_json = match serde_json::to_string_pretty(report) {
        Ok(json) => json,
        Err(error) => return Err(format!("failed to render {label} JSON: {error}")),
    };

    let _guard = LORA_RENDER_DISPATCH_LOCK.lock().await;
    let _payload = ScopedEnvVar::set(payload_env, &payload_json);
    let _pretty = ScopedEnvVar::set(pretty_env, &pretty_json);
    Ok(dispatch::run_embedded_script(script_name, Vec::new(), json).await)
}
