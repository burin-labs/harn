//! Harness-owned HTTP fixture dispatch.
//!
//! Typed harnesses can cross executor threads, so this path carries their
//! fixture registry explicitly instead of relying on the legacy ambient
//! thread-local registry.

use crate::value::{VmError, VmValue};

pub(in crate::http) async fn http_verb_handler(
    mocks: &crate::http::HttpMockRegistry,
    method: &str,
    has_body: bool,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let url = required_arg(
        &args,
        0,
        &format!("http_{}", method.to_ascii_lowercase()),
        "URL",
    )?;
    let mut options = if has_body {
        match (args.get(1), args.get(2)) {
            (Some(VmValue::Dict(options)), None) => (**options).clone(),
            (_, Some(VmValue::Dict(options))) => (**options).clone(),
            _ => crate::value::DictMap::new(),
        }
    } else {
        super::get_options_arg(&args, 1)
    };
    if has_body && !(matches!(args.get(1), Some(VmValue::Dict(_))) && args.get(2).is_none()) {
        let body = args.get(1).map(VmValue::display).unwrap_or_default();
        crate::value::VmDictExt::put_str(&mut options, "body", body);
    }
    vm_execute_http_request_with_mocks(mocks, method, &url, &options).await
}

pub(in crate::http) async fn download(
    mocks: &crate::http::HttpMockRegistry,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let url = required_arg(&args, 0, "http_download", "URL")?;
    let dst_path = required_arg(&args, 1, "http_download", "destination path")?;
    let options = super::get_options_arg(&args, 2);
    super::vm_http_download(Some(mocks), &url, &dst_path, &options).await
}

pub(in crate::http) async fn stream_open(
    mocks: &crate::http::HttpMockRegistry,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let url = required_arg(&args, 0, "http_stream_open", "URL")?;
    let options = super::get_options_arg(&args, 1);
    super::vm_http_stream_open(Some(mocks), &url, &options).await
}

pub(in crate::http) async fn session_request(
    mocks: &crate::http::HttpMockRegistry,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if args.len() < 3 {
        return Err(super::vm_error(
            "http_session_request: requires session, method, and URL",
        ));
    }
    let session_id = super::handle_from_value(&args[0], "http_session_request")?;
    let method = required_arg(&args, 1, "http_session_request", "method")?.to_uppercase();
    let url = required_arg(&args, 2, "http_session_request", "URL")?;
    let options = super::get_options_arg(&args, 3);
    execute_session_request(mocks, &session_id, &method, &url, &options).await
}

fn required_arg(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    label: &str,
) -> Result<String, VmError> {
    let value = args.get(index).map(VmValue::display).unwrap_or_default();
    if value.is_empty() {
        return Err(super::vm_error(format!("{builtin}: {label} is required")));
    }
    Ok(value)
}

pub(in crate::http) async fn vm_execute_http_request_with_mocks(
    mocks: &crate::http::HttpMockRegistry,
    method: &str,
    url: &str,
    options: &crate::value::DictMap,
) -> Result<VmValue, VmError> {
    if let Some(session_id) = super::session_from_options(options) {
        return execute_session_request(mocks, &session_id, method, url, options).await;
    }

    let config = super::parse_http_options(options);
    let client = super::pooled_http_client(&config)?;
    super::vm_execute_http_request_with_client(Some(mocks), client, &config, method, url, options)
        .await
}

pub(super) fn consume_http_mock(
    harness_mocks: Option<&crate::http::HttpMockRegistry>,
    method: &str,
    final_url: &str,
    parts: &super::HttpRequestParts,
) -> Option<crate::http::mock::MockResponse> {
    harness_mocks
        .and_then(|mocks| {
            mocks.consume(
                method,
                final_url,
                parts.recorded_headers.clone(),
                parts.body.clone(),
            )
        })
        .or_else(|| {
            crate::http::mock::consume_http_mock(
                method,
                final_url,
                parts.recorded_headers.clone(),
                parts.body.clone(),
            )
        })
}

async fn execute_session_request(
    mocks: &crate::http::HttpMockRegistry,
    session_id: &str,
    method: &str,
    url: &str,
    options: &crate::value::DictMap,
) -> Result<VmValue, VmError> {
    let session = super::HTTP_SESSIONS.with(|sessions| sessions.borrow().get(session_id).cloned());
    let Some(session) = session else {
        return Err(super::vm_error(format!(
            "http_session_request: unknown HTTP session '{session_id}'"
        )));
    };
    let merged_options = super::merge_options(&session.options, options);
    let config = super::parse_http_options(&merged_options);
    super::vm_execute_http_request_with_client(
        Some(mocks),
        session.client,
        &config,
        method,
        url,
        &merged_options,
    )
    .await
}
