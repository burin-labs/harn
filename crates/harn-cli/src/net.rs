use std::time::Duration;

use harn_vm::egress::{redact_diagnostic_text, redact_reqwest_error, redirect_policy};

const CLI_HTTP_MAX_REDIRECTS: usize = 10;

pub(crate) fn http_client_builder(surface: &'static str) -> reqwest::ClientBuilder {
    reqwest::Client::builder().redirect(redirect_policy(surface, CLI_HTTP_MAX_REDIRECTS))
}

pub(crate) fn blocking_http_client_builder(
    surface: &'static str,
) -> reqwest::blocking::ClientBuilder {
    reqwest::blocking::Client::builder().redirect(redirect_policy(surface, CLI_HTTP_MAX_REDIRECTS))
}

pub(crate) fn http_client(
    surface: &'static str,
    timeout: Duration,
) -> Result<reqwest::Client, String> {
    http_client_builder(surface)
        .timeout(timeout)
        .build()
        .map_err(|error| {
            format!(
                "failed to build HTTP client for {surface}: {}",
                reqwest_error(&error)
            )
        })
}

pub(crate) fn blocking_http_client(
    surface: &'static str,
    timeout: Duration,
) -> Result<reqwest::blocking::Client, String> {
    blocking_http_client_builder(surface)
        .timeout(timeout)
        .build()
        .map_err(|error| {
            format!(
                "failed to build HTTP client for {surface}: {}",
                reqwest_error(&error)
            )
        })
}

pub(crate) fn reqwest_error(error: &reqwest::Error) -> String {
    redact_reqwest_error(error)
}

pub(crate) fn diagnostic_text(raw: &str) -> String {
    redact_diagnostic_text(raw)
}
