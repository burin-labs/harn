use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{json, Value as JsonValue};

use crate::cli::{AppArgs, AppCommand};
use crate::commands::app_host_assets::{host_document, sandbox_document};
use crate::commands::run::{RunFileAppServe, RunFileMcpServeMode};
use crate::commands::serve::ScriptMcpRuntime;

const MCP_APP_MIME: &str = "text/html;profile=mcp-app";
const MCP_APP_EXTENSION: &str = "io.modelcontextprotocol/ui";
const MAX_APP_HTML_BYTES: usize = 2 * 1024 * 1024;
const MAX_APP_RPC_BYTES: usize = 16 * 1024 * 1024;
const SANDBOX_DOCUMENT_CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; frame-src 'self'; style-src 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; media-src 'self' data: blob:; base-uri 'none'; form-action 'none'";

pub(crate) async fn run(args: AppArgs) {
    match args.command {
        AppCommand::Run(args) => {
            if !args.bind.ip().is_loopback() {
                crate::command_error(
                    "harn app run only binds to loopback; use an authenticated deployment host for remote access",
                );
            }
            crate::commands::run::run_file_mcp_serve(
                &args.file,
                None,
                RunFileMcpServeMode::App(Box::new(RunFileAppServe {
                    bind: args.bind,
                    resource: args.resource,
                    open: args.open,
                })),
            )
            .await;
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppDescriptor {
    resource_uri: String,
    title: String,
    html: String,
    meta: JsonValue,
}

#[derive(Clone)]
struct AppHostState {
    runtime: ScriptMcpRuntime,
    descriptor: Arc<AppDescriptor>,
    app_tools: Arc<BTreeSet<String>>,
    host_origin: String,
    sandbox_origin: String,
}

pub(crate) async fn run_script_app_server(
    server: harn_vm::McpServer,
    vm: harn_vm::Vm,
    bind: SocketAddr,
    requested_resource: Option<String>,
    open: bool,
) -> Result<(), String> {
    let runtime = ScriptMcpRuntime::start(server, vm);
    let (descriptor, app_tools) = discover_app(&runtime, requested_resource.as_deref()).await?;
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|error| format!("failed to bind {bind}: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("failed to read local address: {error}"))?;
    let host_origin = format!("http://{address}");
    let sandbox_origin = format!("http://localhost:{}", address.port());
    let state = AppHostState {
        runtime,
        descriptor: Arc::new(descriptor),
        app_tools: Arc::new(app_tools),
        host_origin: host_origin.clone(),
        sandbox_origin,
    };
    let router = Router::new()
        .route("/", get(host_page))
        .route("/sandbox", get(sandbox_page))
        .route("/app", get(app_descriptor))
        .route("/rpc", post(app_rpc))
        .route("/health", get(|| async { Json(json!({"ok": true})) }))
        .layer(DefaultBodyLimit::max(MAX_APP_RPC_BYTES))
        .with_state(state);

    eprintln!(
        "[harn] app: serving {} at {host_origin}",
        requested_resource.as_deref().unwrap_or("first declared UI")
    );
    if open {
        if let Err(error) = webbrowser::open(&host_origin) {
            eprintln!("[harn] app: could not open browser: {error}");
        }
    }
    axum::serve(listener, router)
        .await
        .map_err(|error| format!("local app server failed: {error}"))
}

async fn rpc(
    runtime: &ScriptMcpRuntime,
    id: i64,
    method: &str,
    params: JsonValue,
) -> Result<JsonValue, String> {
    runtime
        .call(
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": mcp_app_params(params)}),
        )
        .await?
        .ok_or_else(|| format!("{method} returned no response"))
}

async fn discover_app(
    runtime: &ScriptMcpRuntime,
    requested_resource: Option<&str>,
) -> Result<(AppDescriptor, BTreeSet<String>), String> {
    rpc(
        runtime,
        1,
        harn_vm::mcp_protocol::METHOD_SERVER_DISCOVER,
        json!({}),
    )
    .await?;
    let tools_response = rpc(runtime, 2, "tools/list", json!({})).await?;
    let tools = tools_response
        .pointer("/result/tools")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "tools/list returned no tools".to_string())?;
    let mut linked_resources = Vec::new();
    let mut app_tools = BTreeSet::new();
    for tool in tools {
        let Some(name) = tool.get("name").and_then(JsonValue::as_str) else {
            continue;
        };
        let visibility = tool_ui_visibility(tool);
        let callable_by_app = visibility
            .is_none_or(|entries| entries.iter().any(|entry| entry.as_str() == Some("app")));
        if callable_by_app {
            app_tools.insert(name.to_string());
        }
        if let Some(uri) = tool_ui_resource_uri(tool) {
            linked_resources.push(uri.to_string());
        }
    }

    let resources_response = rpc(runtime, 3, "resources/list", json!({})).await?;
    let resources = resources_response
        .pointer("/result/resources")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "resources/list returned no resources".to_string())?;

    let uri = if let Some(uri) = requested_resource {
        uri.to_string()
    } else if let Some(uri) = linked_resources.first() {
        uri.clone()
    } else {
        resources
            .iter()
            .find(|resource| {
                resource.get("mimeType").and_then(JsonValue::as_str) == Some(MCP_APP_MIME)
            })
            .and_then(|resource| resource.get("uri"))
            .and_then(JsonValue::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                "script declared no MCP App resource; link a tool with meta.ui.resourceUri or register a text/html;profile=mcp-app resource".to_string()
            })?
    };
    if !uri.starts_with("ui://") {
        return Err(format!("app resource must use ui://, got {uri}"));
    }

    let content_response = rpc(runtime, 4, "resources/read", json!({"uri": uri})).await?;
    let content = content_response
        .pointer("/result/contents/0")
        .ok_or_else(|| format!("resources/read returned no content for {uri}"))?;
    if content.get("mimeType").and_then(JsonValue::as_str) != Some(MCP_APP_MIME) {
        return Err(format!(
            "app resource {uri} must use MIME type {MCP_APP_MIME}"
        ));
    }
    let html = content
        .get("text")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("app resource {uri} must contain UTF-8 HTML text"))?;
    if html.len() > MAX_APP_HTML_BYTES {
        return Err(format!(
            "app resource {uri} is {} bytes; maximum is {MAX_APP_HTML_BYTES}",
            html.len()
        ));
    }
    let meta = content.get("_meta").cloned().unwrap_or_else(|| json!({}));
    validate_resource_meta(&meta)?;
    let title = resources
        .iter()
        .find(|resource| resource.get("uri").and_then(JsonValue::as_str) == Some(uri.as_str()))
        .and_then(|resource| resource.get("title").or_else(|| resource.get("name")))
        .and_then(JsonValue::as_str)
        .unwrap_or("Harn App")
        .to_string();
    Ok((
        AppDescriptor {
            resource_uri: uri,
            title,
            html: html.to_string(),
            meta,
        },
        app_tools,
    ))
}

fn tool_ui_resource_uri(tool: &JsonValue) -> Option<&str> {
    tool.pointer("/_meta/ui/resourceUri")
        .and_then(JsonValue::as_str)
}

fn tool_ui_visibility(tool: &JsonValue) -> Option<&[JsonValue]> {
    tool.pointer("/_meta/ui/visibility")
        .and_then(JsonValue::as_array)
        .map(Vec::as_slice)
}

fn mcp_app_params(params: JsonValue) -> JsonValue {
    let mut params = params.as_object().cloned().unwrap_or_default();
    let mut meta = params
        .remove("_meta")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    meta.insert(
        harn_vm::mcp_protocol::MCP_META_KEY_PROTOCOL_VERSION.to_string(),
        json!(harn_vm::mcp_protocol::PROTOCOL_VERSION),
    );
    meta.insert(
        harn_vm::mcp_protocol::MCP_META_KEY_CLIENT_INFO.to_string(),
        json!({
            "name": "harn-app",
            "version": env!("CARGO_PKG_VERSION"),
        }),
    );
    meta.insert(
        harn_vm::mcp_protocol::MCP_META_KEY_CLIENT_CAPABILITIES.to_string(),
        json!({
            "extensions": {
                MCP_APP_EXTENSION: {"mimeTypes": [MCP_APP_MIME]}
            }
        }),
    );
    params.insert("_meta".to_string(), JsonValue::Object(meta));
    JsonValue::Object(params)
}

fn validate_resource_meta(meta: &JsonValue) -> Result<(), String> {
    let Some(ui) = meta.get("ui") else {
        return Ok(());
    };
    let ui = ui
        .as_object()
        .ok_or_else(|| "app resource _meta.ui must be an object".to_string())?;
    if let Some(csp) = ui.get("csp") {
        let csp = csp
            .as_object()
            .ok_or_else(|| "app resource _meta.ui.csp must be an object".to_string())?;
        for key in [
            "connectDomains",
            "resourceDomains",
            "frameDomains",
            "baseUriDomains",
        ] {
            let Some(values) = csp.get(key) else {
                continue;
            };
            let values = values
                .as_array()
                .ok_or_else(|| format!("app resource CSP {key} must be a list"))?;
            for value in values {
                let source = value
                    .as_str()
                    .ok_or_else(|| format!("app resource CSP {key} entries must be strings"))?;
                validate_csp_source(source)?;
            }
        }
    }
    if let Some(permissions) = ui.get("permissions") {
        let permissions = permissions
            .as_object()
            .ok_or_else(|| "app resource permissions must be an object".to_string())?;
        for (permission, value) in permissions {
            if !matches!(
                permission.as_str(),
                "camera" | "microphone" | "geolocation" | "clipboardWrite"
            ) || !value.is_object()
            {
                return Err(format!("unsupported app browser permission {permission:?}"));
            }
        }
    }
    Ok(())
}

fn validate_csp_source(source: &str) -> Result<(), String> {
    if source.is_empty()
        || source
            .chars()
            .any(|character| character.is_whitespace() || matches!(character, ';' | '\'' | '"'))
    {
        return Err(format!("invalid app CSP source {source:?}"));
    }
    let parseable = source.replace("://*.", "://wildcard.");
    let url = url::Url::parse(&parseable)
        .map_err(|_| format!("app CSP source must be an absolute origin: {source:?}"))?;
    if !matches!(url.scheme(), "http" | "https" | "ws" | "wss")
        || url.host_str().is_none()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(format!("app CSP source must be an origin: {source:?}"));
    }
    Ok(())
}

async fn host_page(State(state): State<AppHostState>) -> Response {
    let mut response = Html(host_document(
        &state.descriptor.title,
        &state.sandbox_origin,
    ))
    .into_response();
    let policy = format!(
        "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; frame-src {}; base-uri 'none'; form-action 'none'",
        state.sandbox_origin
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(&policy).expect("sandbox origin forms a valid CSP"),
    );
    add_document_headers(&mut response);
    response
}

async fn sandbox_page() -> Response {
    let mut response = Html(sandbox_document()).into_response();
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(SANDBOX_DOCUMENT_CSP),
    );
    add_document_headers(&mut response);
    response
}

fn add_document_headers(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
}

async fn app_descriptor(State(state): State<AppHostState>) -> Json<AppDescriptor> {
    Json((*state.descriptor).clone())
}

async fn app_rpc(
    State(state): State<AppHostState>,
    headers: HeaderMap,
    Json(request): Json<JsonValue>,
) -> Response {
    if !has_expected_origin(&headers, &state.host_origin) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "app RPC origin is not allowed"})),
        )
            .into_response();
    }
    let method = request
        .get("method")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    if !matches!(method, "tools/call" | "resources/read" | "ping") {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": format!("app method is not allowed: {method}")})),
        )
            .into_response();
    }
    if method == "tools/call" {
        let tool = request.pointer("/params/name").and_then(JsonValue::as_str);
        if !tool.is_some_and(|name| state.app_tools.contains(name)) {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "tool is not visible to this app"})),
            )
                .into_response();
        }
    }
    let mut request = request;
    let params = mcp_app_params(request.get("params").cloned().unwrap_or_else(|| json!({})));
    let Some(request_object) = request.as_object_mut() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "app RPC request must be a JSON object"})),
        )
            .into_response();
    };
    request_object.insert("params".to_string(), params);
    match state.runtime.call(request).await {
        Ok(Some(response)) => Json(response).into_response(),
        Ok(None) => StatusCode::ACCEPTED.into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": error})),
        )
            .into_response(),
    }
}

fn has_expected_origin(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        == Some(expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_document_escapes_dynamic_values_as_json() {
        let html = host_document("A </script> title", "http://localhost:1234");
        assert!(!html.contains("const title = \"A </script>"));
        assert!(html.contains("http://localhost:1234"));
    }

    #[test]
    fn resource_meta_rejects_csp_directive_injection() {
        let malicious = json!({
            "ui": {"csp": {"connectDomains": ["https://ok.test; img-src *"]}}
        });
        assert!(validate_resource_meta(&malicious).is_err());
        let valid = json!({
            "ui": {"csp": {"connectDomains": ["https://api.example.test"]}}
        });
        assert!(validate_resource_meta(&valid).is_ok());
    }

    #[test]
    fn app_rpc_requires_the_exact_host_origin() {
        let mut headers = HeaderMap::new();
        assert!(!has_expected_origin(&headers, "http://127.0.0.1:4321"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://attacker.example"),
        );
        assert!(!has_expected_origin(&headers, "http://127.0.0.1:4321"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:4321"),
        );
        assert!(has_expected_origin(&headers, "http://127.0.0.1:4321"));
    }

    #[test]
    fn tool_link_uses_stable_nested_mcp_apps_metadata() {
        let current = json!({"_meta": {"ui": {
            "resourceUri": "ui://current",
            "visibility": ["app"]
        }}});
        assert_eq!(tool_ui_resource_uri(&current), Some("ui://current"));
        assert_eq!(
            tool_ui_visibility(&current),
            Some([json!("app")].as_slice())
        );
    }

    #[test]
    fn app_metadata_preserves_request_scoped_fields() {
        let params = mcp_app_params(json!({"_meta": {"progressToken": "progress-1"}}));
        assert_eq!(params["_meta"]["progressToken"], json!("progress-1"));
        assert_eq!(
            params["_meta"][harn_vm::mcp_protocol::MCP_META_KEY_PROTOCOL_VERSION],
            json!(harn_vm::mcp_protocol::PROTOCOL_VERSION)
        );
    }

    #[test]
    fn sandbox_outer_policy_leaves_media_control_to_the_inner_policy() {
        assert!(SANDBOX_DOCUMENT_CSP.contains("img-src 'self' data: blob:"));
        assert!(SANDBOX_DOCUMENT_CSP.contains("media-src 'self' data: blob:"));
        assert!(!SANDBOX_DOCUMENT_CSP.contains("connect-src"));
    }
}
