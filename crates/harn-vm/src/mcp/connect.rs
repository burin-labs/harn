use super::*;

pub(crate) async fn mcp_connect_stdio_impl(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    protocol_mode: McpProtocolMode,
    protocol_version: String,
) -> Result<VmMcpClientHandle, VmError> {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .envs(env);
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "mcp_connect: failed to spawn '{command}': {e}"
        ))))
    })?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| VmError::Runtime("mcp_connect: failed to open stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| VmError::Runtime("mcp_connect: failed to open stdout".into()))?;

    let handle = VmMcpClientHandle {
        name: command.to_string(),
        inner: Arc::new(Mutex::new(Some(McpClientInner::Stdio(
            StdioMcpClientInner {
                child,
                stdin,
                reader: BufReader::new(stdout),
                next_id: 1,
                protocol_mode,
                protocol_version,
                response_deadline: MCP_TIMEOUT,
            },
        )))),
        last_roots: Arc::new(Mutex::new(Vec::new())),
        initialize_result: Arc::new(Mutex::new(None)),
        cache_hints: Arc::new(Mutex::new(BTreeMap::new())),
    };

    initialize_client(&handle).await?;
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
    let options = resolve_connect_protocol_options(
        spec.protocol_mode.as_deref(),
        spec.protocol_version.as_deref(),
    )?;
    let resolved_auth = resolve_http_auth_token_source(spec).await;

    let handle = VmMcpClientHandle {
        name: spec.name.clone(),
        inner: Arc::new(Mutex::new(Some(McpClientInner::Http(HttpMcpClientInner {
            client,
            url: spec.url.clone(),
            auth_token: resolved_auth.token,
            auth_token_source: resolved_auth.source,
            token_exchange: spec.token_exchange.clone().map(Arc::new),
            protocol_mode: options.protocol_mode,
            protocol_version: options.protocol_version,
            session_id: None,
            next_id: 1,
            proxy_server_name: spec.proxy_server_name.clone(),
            get_stream_task: None,
            tool_headers: BTreeMap::new(),
        })))),
        last_roots: Arc::new(Mutex::new(Vec::new())),
        initialize_result: Arc::new(Mutex::new(None)),
        cache_hints: Arc::new(Mutex::new(BTreeMap::new())),
    };

    initialize_client(&handle).await?;
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

pub(crate) async fn initialize_client(handle: &VmMcpClientHandle) -> Result<(), VmError> {
    if handle.protocol_mode().await? == McpProtocolMode::Modern {
        let discover = handle
            .call_raw("server/discover", serde_json::json!({}))
            .await?;
        if is_method_not_found_response(&discover) {
            handle.switch_to_legacy_protocol().await?;
            return initialize_legacy_client(handle).await;
        }
        let discover_result = parse_jsonrpc_result(discover)?;
        *handle.initialize_result.lock().await = Some(discover_result);
        return Ok(());
    }

    initialize_legacy_client(handle).await
}

pub(crate) async fn initialize_legacy_client(handle: &VmMcpClientHandle) -> Result<(), VmError> {
    let protocol_version = handle.protocol_version().await?;
    let initialize_result = handle
        .call("initialize", legacy_initialize_params(&protocol_version))
        .await?;
    *handle.initialize_result.lock().await = Some(initialize_result);

    handle
        .notify("notifications/initialized", serde_json::json!({}))
        .await?;

    Ok(())
}

pub async fn connect_mcp_server_from_spec(
    spec: &McpServerSpec,
) -> Result<VmMcpClientHandle, VmError> {
    let mut handle = match spec.transport {
        McpTransport::Stdio => {
            let options = resolve_connect_protocol_options(
                spec.protocol_mode.as_deref(),
                spec.protocol_version.as_deref(),
            )?;
            mcp_connect_stdio_impl(
                &spec.command,
                &spec.args,
                &spec.env,
                options.protocol_mode,
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
