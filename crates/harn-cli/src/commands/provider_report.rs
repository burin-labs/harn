use serde::Serialize;

use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

/// Serialises provider report rendering so concurrent in-process callers don't
/// race on the global env vars the embedded-script shim sets.
static DISPATCH_PROVIDER_REPORT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone, Copy)]
pub(crate) struct ProviderReportDispatch {
    pub(crate) script_name: &'static str,
    pub(crate) payload_name: &'static str,
    pub(crate) payload_env: &'static str,
    pub(crate) pretty_env: &'static str,
}

impl ProviderReportDispatch {
    pub(crate) async fn render<T: Serialize>(self, json: bool, markdown: bool, report: &T) -> i32 {
        if !markdown {
            return dispatch_provider_report(self, json, report).await;
        }
        let Some(markdown_env) = provider_report_markdown_env(self.script_name) else {
            eprintln!(
                "error: {} does not support Markdown rendering",
                self.payload_name
            );
            return 1;
        };
        dispatch_provider_report_with_env(
            self,
            false,
            &[(dispatch::JSON_MODE_ENV, "0"), (markdown_env, "1")],
            report,
        )
        .await
    }
}

fn provider_report_markdown_env(script_name: &str) -> Option<&'static str> {
    match script_name {
        "providers/tool_scorecard" => Some("HARN_PROVIDER_TOOL_SCORECARD_MARKDOWN"),
        _ => None,
    }
}

pub(crate) async fn dispatch_provider_report<T: Serialize>(
    dispatch_info: ProviderReportDispatch,
    json: bool,
    report: &T,
) -> i32 {
    dispatch_provider_report_with_env(dispatch_info, json, &[], report).await
}

pub(crate) async fn dispatch_provider_report_with_env<T: Serialize>(
    dispatch_info: ProviderReportDispatch,
    json: bool,
    extra_env: &[(&'static str, &str)],
    report: &T,
) -> i32 {
    let payload_json = match serde_json::to_string(report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!(
                "error: failed to serialise {} payload: {error}",
                dispatch_info.payload_name
            );
            return 1;
        }
    };
    let payload_pretty = match serde_json::to_string_pretty(report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!(
                "error: failed to render {} payload: {error}",
                dispatch_info.payload_name
            );
            return 1;
        }
    };
    let _guard = DISPATCH_PROVIDER_REPORT_LOCK.lock().await;
    let _payload_guard = ScopedEnvVar::set(dispatch_info.payload_env, &payload_json);
    let _pretty_guard = ScopedEnvVar::set(dispatch_info.pretty_env, &payload_pretty);
    let _extra_guards = extra_env
        .iter()
        .map(|(key, value)| ScopedEnvVar::set(key, value))
        .collect::<Vec<_>>();
    dispatch::dispatch_to_embedded_script(dispatch_info.script_name, Vec::new(), json).await
}
