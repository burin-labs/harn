use super::*;

pub(crate) async fn mcp_connect_stdio_impl(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: Option<&str>,
    requested_protocol_version: String,
) -> Result<VmMcpClientHandle, VmError> {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .envs(env);
    if let Some(cwd) = cwd.map(str::trim).filter(|cwd| !cwd.is_empty()) {
        cmd.current_dir(cwd);
    }
    cmd.kill_on_drop(true);

    let transport = rmcp::transport::TokioChildProcess::new(cmd).map_err(|e| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "mcp_connect: failed to spawn '{command}': {e}"
        ))))
    })?;
    let requested_version = sdk_protocol_version(&requested_protocol_version);
    let handler = HarnSdkClientHandler::new(command, requested_version.clone());
    let legacy_version = if requested_version >= rmcp::model::ProtocolVersion::STANDARD_HEADERS {
        rmcp::model::ProtocolVersion::LATEST
    } else {
        requested_version.clone()
    };
    let mut preferred_versions = vec![requested_version];
    for version in rmcp::model::ProtocolVersion::KNOWN_VERSIONS.iter().rev() {
        if version.as_str() != requested_protocol_version {
            preferred_versions.push(version.clone());
        }
    }
    let lifecycle = rmcp::service::ClientLifecycleMode::Auto {
        preferred_versions,
        legacy_version: Some(legacy_version),
    };
    use rmcp::service::ClientServiceExt;
    let running = handler
        .clone()
        .serve_with_lifecycle(transport, lifecycle)
        .await
        .map_err(|error| VmError::Runtime(format!("MCP SDK initialization failed: {error}")))?;
    let peer_info = running
        .peer_info()
        .ok_or_else(|| VmError::Runtime("MCP SDK did not retain negotiated server info".into()))?;
    let discovery_result = serde_json::to_value(peer_info.as_ref())
        .map_err(|error| VmError::Runtime(format!("MCP SDK server info error: {error}")))?;

    let handle = VmMcpClientHandle {
        name: command.to_string(),
        inner: Arc::new(Mutex::new(Some(McpClientInner::Sdk(SdkMcpClientInner {
            running,
            handler,
        })))),
        last_roots: Arc::new(Mutex::new(Vec::new())),
        discovery_result: Arc::new(Mutex::new(Some(discovery_result))),
        cache_hints: Arc::new(Mutex::new(BTreeMap::new())),
    };
    Ok(handle)
}

pub(crate) async fn mcp_connect_http_impl(
    spec: &McpServerSpec,
) -> Result<VmMcpClientHandle, VmError> {
    let builder = reqwest::Client::builder()
        .redirect(crate::egress::redirect_policy("mcp_http_redirect", 10));
    let client = crate::egress::install_ssrf_guard(builder)
        .build()
        .map_err(|e| VmError::Runtime(format!("MCP HTTP client error: {e}")))?;
    let options = resolve_connect_protocol_options(spec.protocol_version.as_deref())?;
    if options.protocol_version != PROTOCOL_VERSION {
        return Err(VmError::Runtime(format!(
            "mcp_connect: HTTP transport requires protocol_version {PROTOCOL_VERSION:?}; older versions are negotiated only by the SDK-managed stdio transport"
        )));
    }
    let resolved_auth = resolve_http_auth_token_source(spec).await;

    let handle = VmMcpClientHandle {
        name: spec.name.clone(),
        inner: Arc::new(Mutex::new(Some(McpClientInner::Http(HttpMcpClientInner {
            client,
            url: spec.url.clone(),
            auth_token: resolved_auth.token,
            auth_token_source: resolved_auth.source,
            token_exchange: spec.token_exchange.clone().map(Arc::new),
            protocol_version: options.protocol_version,
            next_id: 1,
            proxy_server_name: spec.proxy_server_name.clone(),
            tool_headers: BTreeMap::new(),
            fixtures: None,
        })))),
        last_roots: Arc::new(Mutex::new(Vec::new())),
        discovery_result: Arc::new(Mutex::new(None)),
        cache_hints: Arc::new(Mutex::new(BTreeMap::new())),
    };

    discover_server(&handle).await?;
    Ok(handle)
}

pub(crate) async fn resolve_http_auth_token_source(spec: &McpServerSpec) -> ResolvedHttpAuthToken {
    resolve_http_auth_token_source_with(spec, |server_url| async move {
        crate::mcp_oauth::resolve_bearer(&server_url).await
    })
    .await
}

pub(crate) async fn resolve_http_auth_token_source_with<R, Fut>(
    spec: &McpServerSpec,
    resolver: R,
) -> ResolvedHttpAuthToken
where
    R: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<Option<String>, String>>,
{
    if let Some(token) = spec.auth_token.as_deref().filter(|token| !token.is_empty()) {
        return ResolvedHttpAuthToken {
            token: Some(token.to_string()),
            source: HttpAuthTokenSource::Config,
        };
    }
    if spec.url.is_empty() {
        return ResolvedHttpAuthToken {
            token: None,
            source: HttpAuthTokenSource::None,
        };
    }
    match resolver(spec.url.clone()).await.unwrap_or(None) {
        Some(token) => ResolvedHttpAuthToken {
            token: Some(token),
            source: HttpAuthTokenSource::OAuthStore,
        },
        None => ResolvedHttpAuthToken {
            token: None,
            source: HttpAuthTokenSource::None,
        },
    }
}

pub(crate) async fn discover_server(handle: &VmMcpClientHandle) -> Result<(), VmError> {
    let discover = handle
        .call_raw("server/discover", serde_json::json!({}))
        .await?;
    let discover_result = parse_jsonrpc_result(discover)?;
    *handle.discovery_result.lock().await = Some(discover_result);
    Ok(())
}

pub async fn connect_mcp_server_from_spec(
    spec: &McpServerSpec,
) -> Result<VmMcpClientHandle, VmError> {
    let mut handle = match spec.transport {
        McpTransport::Stdio => {
            let options = resolve_connect_protocol_options(spec.protocol_version.as_deref())?;
            mcp_connect_stdio_impl(
                &spec.command,
                &spec.args,
                &spec.env,
                spec.cwd.as_deref(),
                options.protocol_version,
            )
            .await?
        }
        McpTransport::Http => mcp_connect_http_impl(spec).await?,
    };
    handle.name = spec.name.clone();
    Ok(handle)
}

pub async fn connect_mcp_server_from_json(
    value: &serde_json::Value,
) -> Result<VmMcpClientHandle, VmError> {
    let spec: McpServerSpec = serde_json::from_value(value.clone())
        .map_err(|e| VmError::Runtime(format!("Invalid MCP server config: {e}")))?;
    connect_mcp_server_from_spec(&spec).await
}
