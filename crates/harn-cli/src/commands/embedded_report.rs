//! Render a Rust-collected report through its embedded `.harn` renderer.
//!
//! Commands that build a typed report in Rust hand it to a `.harn` script for
//! presentation: the payload crosses on an env var, the script reads it and
//! prints. The mechanism is not specific to any one command, so it lives here
//! rather than inside the first command that needed it.

use std::io::Write as _;

use serde::Serialize;

use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

/// Serialises the dispatch path so concurrent in-process callers do not race
/// on the env vars that carry the report payload. The payload crosses as
/// process state, so two renders in flight at once would read each other's.
static EMBEDDED_REPORT_DISPATCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn render_embedded_report<T: Serialize>(
    report: &T,
    payload_env: &'static str,
    pretty_env: &'static str,
    script_name: &'static str,
    json: bool,
    label: &str,
) -> i32 {
    let outcome = match run_embedded_report(
        report,
        payload_env,
        pretty_env,
        script_name,
        json,
        label,
    )
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

pub(crate) async fn run_embedded_report<T: Serialize>(
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

    let _guard = EMBEDDED_REPORT_DISPATCH_LOCK.lock().await;
    let _payload = ScopedEnvVar::set(payload_env, &payload_json);
    let _pretty = ScopedEnvVar::set(pretty_env, &pretty_json);
    Ok(dispatch::run_embedded_script(script_name, Vec::new(), json).await)
}
