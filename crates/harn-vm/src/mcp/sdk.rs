//! Official MCP SDK adapter for Harn-owned client policy.
//!
//! The SDK owns protocol lifecycle, framing, request association, cancellation,
//! standard metadata, and version negotiation. This adapter owns the Harn
//! behaviors invoked by inbound server requests and notifications.

#![expect(
    deprecated,
    reason = "rmcp 3.1 resolves stable MRTR inputs through its deprecated direct-request handler types"
)]

use super::*;
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientInfo, ClientNotification, ClientRequest,
    CreateMessageRequestParams, CreateMessageResult, CustomNotification, CustomRequest,
    ElicitRequestParams, ElicitResult, ElicitationCapability, ErrorCode, ErrorData as McpError,
    FormElicitationCapability, GetPromptRequestParams, Implementation, ListRootsResult,
    LoggingMessageNotificationParam, ProgressNotificationParam, ProtocolVersion,
    ReadResourceRequestParams, ResourceUpdatedNotificationParam, Root, RootsCapabilities,
    SamplingCapability, ServerResult, UrlElicitationCapability,
};
use rmcp::service::{
    NotificationContext, RequestContext, RoleClient, RunningService, ServiceError,
};
use rmcp::ClientHandler;

#[derive(Clone)]
pub(crate) struct HarnSdkClientHandler {
    server_name: Arc<str>,
    fixtures: Arc<tokio::sync::RwLock<Option<Arc<crate::harness::CapabilityFixtureState>>>>,
    protocol_version: ProtocolVersion,
}

impl HarnSdkClientHandler {
    pub(crate) fn new(server_name: impl Into<Arc<str>>, protocol_version: ProtocolVersion) -> Self {
        Self {
            server_name: server_name.into(),
            fixtures: Arc::new(tokio::sync::RwLock::new(None)),
            protocol_version,
        }
    }

    pub(crate) async fn set_fixtures(&self, fixtures: Arc<crate::harness::CapabilityFixtureState>) {
        *self.fixtures.write().await = Some(fixtures);
    }

    pub(crate) async fn fixtures(&self) -> Option<Arc<crate::harness::CapabilityFixtureState>> {
        self.fixtures.read().await.clone()
    }

    fn request(
        method: &str,
        id: &rmcp::model::RequestId,
        params: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": serde_json::to_value(id).unwrap_or(serde_json::Value::Null),
            "method": method,
            "params": params,
        })
    }

    fn typed_result<T: serde::de::DeserializeOwned>(
        response: serde_json::Value,
    ) -> Result<T, McpError> {
        if let Some(error) = response.get("error") {
            return Err(serde_json::from_value(error.clone()).unwrap_or_else(|_| {
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    error
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("Harn MCP client handler failed")
                        .to_string(),
                    error.get("data").cloned(),
                )
            }));
        }
        serde_json::from_value(
            response
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
        .map_err(|error| {
            McpError::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Harn MCP handler returned an invalid SDK result: {error}"),
                None,
            )
        })
    }

    fn relay_notification<T: serde::Serialize>(&self, method: &str, params: &T) {
        let params = serde_json::to_value(params).unwrap_or(serde_json::Value::Null);
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        match method {
            "notifications/progress" => relay_progress_notification(&self.server_name, &message),
            "notifications/message" => relay_log_notification(&self.server_name, &message),
            _ => relay_resource_notification(&self.server_name, method, &message),
        }
    }
}

impl ClientHandler for HarnSdkClientHandler {
    async fn create_message(
        &self,
        params: CreateMessageRequestParams,
        context: RequestContext<RoleClient>,
    ) -> Result<CreateMessageResult, McpError> {
        let request = Self::request(
            crate::mcp_sampling::SAMPLING_METHOD,
            &context.id,
            serde_json::to_value(params).map_err(|error| {
                McpError::new(ErrorCode::INVALID_PARAMS, error.to_string(), None)
            })?,
        );
        Self::typed_result(
            crate::mcp_sampling::dispatch_inbound_sampling(&self.server_name, &request).await,
        )
    }

    async fn create_elicitation(
        &self,
        params: ElicitRequestParams,
        context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, McpError> {
        let request = Self::request(
            crate::mcp_elicit::ELICITATION_METHOD,
            &context.id,
            serde_json::to_value(params).map_err(|error| {
                McpError::new(ErrorCode::INVALID_PARAMS, error.to_string(), None)
            })?,
        );
        let fixtures = self.fixtures.read().await.clone();
        Self::typed_result(
            crate::mcp_elicit::dispatch_inbound_elicitation(
                &self.server_name,
                &request,
                fixtures.as_deref(),
            )
            .await,
        )
    }

    async fn list_roots(
        &self,
        _context: RequestContext<RoleClient>,
    ) -> Result<ListRootsResult, McpError> {
        Ok(ListRootsResult::new(
            current_mcp_roots()
                .into_iter()
                .map(|root| Root::new(root.uri).with_name(root.name))
                .collect(),
        ))
    }

    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.relay_notification("notifications/progress", &params);
    }

    async fn on_logging_message(
        &self,
        params: LoggingMessageNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.relay_notification("notifications/message", &params);
    }

    async fn on_resource_updated(
        &self,
        params: ResourceUpdatedNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.relay_notification("notifications/resources/updated", &params);
    }

    async fn on_resource_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.relay_notification(
            "notifications/resources/list_changed",
            &serde_json::json!({}),
        );
    }

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.relay_notification("notifications/tools/list_changed", &serde_json::json!({}));
    }

    async fn on_prompt_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.relay_notification("notifications/prompts/list_changed", &serde_json::json!({}));
    }

    async fn on_custom_notification(
        &self,
        notification: CustomNotification,
        _context: NotificationContext<RoleClient>,
    ) {
        self.relay_notification(
            &notification.method,
            &notification.params.unwrap_or(serde_json::Value::Null),
        );
    }

    fn get_info(&self) -> ClientInfo {
        let mut capabilities = ClientCapabilities::default();
        capabilities.elicitation = Some(
            ElicitationCapability::new()
                .with_form(FormElicitationCapability::new().with_schema_validation(true))
                .with_url(UrlElicitationCapability::new()),
        );
        capabilities.roots = Some(RootsCapabilities::default());
        capabilities.sampling = Some(SamplingCapability::default());
        capabilities
            .extensions
            .get_or_insert_default()
            .insert(TASKS_EXTENSION_ID.to_string(), Default::default());
        ClientInfo::new(
            capabilities,
            Implementation::new("harn", env!("CARGO_PKG_VERSION")),
        )
        .with_protocol_version(self.protocol_version.clone())
    }
}

pub(crate) struct SdkMcpClientInner {
    pub(crate) running: RunningService<RoleClient, HarnSdkClientHandler>,
    pub(crate) handler: HarnSdkClientHandler,
}

pub(crate) fn sdk_protocol_version(value: &str) -> ProtocolVersion {
    serde_json::from_value(serde_json::Value::String(value.to_string()))
        .expect("validated MCP protocol versions deserialize through the SDK")
}

pub(crate) fn sdk_service_error(error: ServiceError) -> VmError {
    match error {
        ServiceError::McpError(error) => {
            jsonrpc_error_to_vm_error(&serde_json::to_value(error).unwrap_or_else(|_| {
                serde_json::json!({
                    "code": -32603,
                    "message": "MCP SDK error",
                })
            }))
        }
        other => VmError::Runtime(format!("MCP SDK: {other}")),
    }
}

pub(crate) async fn sdk_call_raw(
    inner: &SdkMcpClientInner,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, VmError> {
    let typed_result = match method {
        "tools/call" => {
            let params: CallToolRequestParams = sdk_request_params(method, &params)?;
            let response = tokio::time::timeout(MCP_TIMEOUT, inner.running.call_tool_once(params))
                .await
                .map_err(|_| VmError::Runtime("MCP SDK tools/call timed out".to_string()))?
                .map_err(sdk_service_error)?;
            Some(ServerResult::from(response))
        }
        "prompts/get" => {
            let params: GetPromptRequestParams = sdk_request_params(method, &params)?;
            let response = tokio::time::timeout(MCP_TIMEOUT, inner.running.get_prompt(params))
                .await
                .map_err(|_| VmError::Runtime("MCP SDK prompts/get timed out".to_string()))?
                .map_err(sdk_service_error)?;
            Some(ServerResult::GetPromptResult(response))
        }
        "resources/read" => {
            let params: ReadResourceRequestParams = sdk_request_params(method, &params)?;
            let response = tokio::time::timeout(MCP_TIMEOUT, inner.running.read_resource(params))
                .await
                .map_err(|_| VmError::Runtime("MCP SDK resources/read timed out".to_string()))?
                .map_err(sdk_service_error)?;
            Some(ServerResult::ReadResourceResult(response))
        }
        _ => None,
    };
    if let Some(result) = typed_result {
        let result = serde_json::to_value(result)
            .map_err(|error| VmError::Runtime(format!("MCP SDK serialization error: {error}")))?;
        return Ok(serde_json::json!({"jsonrpc": "2.0", "result": result}));
    }

    let result = inner
        .running
        .send_request_with_option(
            ClientRequest::from(CustomRequest::new(method, Some(params))),
            rmcp::service::PeerRequestOptions::with_timeout(MCP_TIMEOUT)
                .with_max_total_timeout(MCP_TIMEOUT),
        )
        .await
        .map_err(sdk_service_error)?
        .await_response()
        .await
        .map_err(sdk_service_error)?;
    let result = match result {
        rmcp::model::ServerResult::CustomResult(result) => result.0,
        typed => serde_json::to_value(typed)
            .map_err(|error| VmError::Runtime(format!("MCP SDK serialization error: {error}")))?,
    };
    Ok(serde_json::json!({"jsonrpc": "2.0", "result": result}))
}

fn sdk_request_params<T>(method: &str, params: &serde_json::Value) -> Result<T, VmError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(params.clone())
        .map_err(|error| VmError::Runtime(format!("MCP SDK {method} params error: {error}")))
}

pub(crate) async fn sdk_notify(
    inner: &SdkMcpClientInner,
    method: &str,
    params: serde_json::Value,
) -> Result<(), VmError> {
    inner
        .running
        .send_notification(ClientNotification::from(CustomNotification::new(
            method,
            Some(params),
        )))
        .await
        .map_err(sdk_service_error)
}
