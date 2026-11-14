//! Verified unary bindings over the existing SDK-managed MCP connection.

use std::num::NonZeroU32;
use std::sync::Arc;

use rmcp::model::{CallToolRequestParams, CallToolResponse};
use serde_json::Value;
use tokio::sync::Semaphore;

use super::{McpClientInner, VmMcpClientHandle};
use crate::codegen::{CodegenOutput, ValidatedCodegenIr};
use crate::tool_registry::{
    McpOutputEnvelope, NativeMcpBindingProfile, PreparedToolCatalog, ToolApplicationError,
    ToolAudience, ToolCatalog, ToolContractPhase, ToolInvocationOutcome, ToolThrownClassification,
    HARN_MCP_TOOL_CONTRACT_META_KEY, HARN_NATIVE_BINDING_META_KEY,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMcpFailure {
    Profile,
    Discovery,
    Capability,
    Disconnected,
    AlreadyBound,
    Operation,
    Input,
    Output,
    Handler,
    Incomplete,
    Transport,
    Rpc,
    Protocol,
    Deadline,
}

#[cfg(test)]
mod tests;

impl std::fmt::Display for NativeMcpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "native MCP {:?} failure", self)
    }
}

impl std::error::Error for NativeMcpFailure {}

pub(crate) struct NativeBindingState {
    ir: ValidatedCodegenIr,
    prepared: PreparedToolCatalog,
    permits: Arc<Semaphore>,
}

/// An immutable verified mode of one ordinary MCP client, not another transport.
#[derive(Clone)]
pub struct VerifiedNativeMcpBinding {
    client: VmMcpClientHandle,
    state: Arc<NativeBindingState>,
}

/// Validated business data and the protocol's unchanged optional metadata.
pub struct NativeMcpReply {
    pub outcome: ToolInvocationOutcome,
    pub metadata: Option<Value>,
}

impl VmMcpClientHandle {
    /// Retrieve only this connection's immutable independently verified mode.
    pub async fn verified_native_binding(
        &self,
    ) -> Result<VerifiedNativeMcpBinding, NativeMcpFailure> {
        let guard = self.inner.lock().await;
        let Some(McpClientInner::Sdk(inner)) = guard.as_ref() else {
            return Err(NativeMcpFailure::Disconnected);
        };
        let state = inner
            .verified_native
            .clone()
            .ok_or(NativeMcpFailure::Profile)?;
        Ok(VerifiedNativeMcpBinding {
            client: self.clone(),
            state,
        })
    }

    /// Verify independently compiled identities and discovery before enabling
    /// concurrent native calls. Raw compatibility calls are then refused.
    pub fn bind_native(
        &self,
        ir: ValidatedCodegenIr,
        max_concurrency: NonZeroU32,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<VerifiedNativeMcpBinding, NativeMcpFailure>>
                + Send
                + '_,
        >,
    > {
        // Keep the SDK's complete generic request/interactive state machine
        // inside its owning crate rather than exporting it into host adapters.
        Box::pin(self.bind_native_inner(ir, max_concurrency))
    }

    async fn bind_native_inner(
        &self,
        ir: ValidatedCodegenIr,
        max_concurrency: NonZeroU32,
    ) -> Result<VerifiedNativeMcpBinding, NativeMcpFailure> {
        if max_concurrency.get() as usize > Semaphore::MAX_PERMITS {
            return Err(NativeMcpFailure::Capability);
        }
        {
            let guard = self.inner.lock().await;
            match guard.as_ref() {
                Some(McpClientInner::Sdk(inner)) if inner.verified_native.is_none() => {}
                Some(McpClientInner::Sdk(_)) => return Err(NativeMcpFailure::AlreadyBound),
                Some(McpClientInner::Http(_)) => return Err(NativeMcpFailure::Capability),
                None => return Err(NativeMcpFailure::Disconnected),
            }
        }
        let discovery = self
            .discovery_result
            .lock()
            .await
            .clone()
            .ok_or(NativeMcpFailure::Discovery)?;
        let profile: NativeMcpBindingProfile = serde_json::from_value(
            discovery
                .get("_meta")
                .and_then(|meta| meta.get(HARN_NATIVE_BINDING_META_KEY))
                .cloned()
                .ok_or(NativeMcpFailure::Profile)?,
        )
        .map_err(|_| NativeMcpFailure::Profile)?;
        let listed = self
            .call("tools/list", serde_json::json!({}))
            .await
            .map_err(|_| NativeMcpFailure::Discovery)?;
        if listed
            .get("nextCursor")
            .is_some_and(|cursor| !cursor.is_null())
        {
            return Err(NativeMcpFailure::Discovery);
        }
        let tools = listed
            .get("tools")
            .and_then(Value::as_array)
            .ok_or(NativeMcpFailure::Discovery)?;
        profile
            .verify(&ir, max_concurrency, tools)
            .map_err(|_| NativeMcpFailure::Profile)?;
        let mut catalog: ToolCatalog = serde_json::from_str(&ir.ir().catalog_canonical_json)
            .map_err(|_| NativeMcpFailure::Profile)?;
        catalog.tools.retain(|tool| {
            ir.ir()
                .operations
                .iter()
                .any(|operation| operation.name == tool.name)
        });
        let state = Arc::new(NativeBindingState {
            ir,
            prepared: PreparedToolCatalog::prepare(catalog)
                .map_err(|_| NativeMcpFailure::Profile)?,
            permits: Arc::new(Semaphore::new(max_concurrency.get() as usize)),
        });
        let mut guard = self.inner.lock().await;
        let Some(McpClientInner::Sdk(inner)) = guard.as_mut() else {
            return Err(NativeMcpFailure::Disconnected);
        };
        if inner.verified_native.is_some() {
            return Err(NativeMcpFailure::AlreadyBound);
        }
        if inner.running.peer_info().is_none_or(|info| {
            info.protocol_version.as_str() != crate::mcp_protocol::PROTOCOL_VERSION
        }) {
            return Err(NativeMcpFailure::Capability);
        }
        inner.verified_native = Some(Arc::clone(&state));
        Ok(VerifiedNativeMcpBinding {
            client: self.clone(),
            state,
        })
    }
}

impl VerifiedNativeMcpBinding {
    /// The original public catalog, not the worker's unary transport projection.
    pub fn catalog(&self) -> &ToolCatalog {
        self.state.prepared.catalog()
    }

    /// Inputs and adapter governance are checked before the SDK sends a call.
    /// Dropping this future uses the SDK's request-specific cancellation owner.
    /// A failed transport is never replayed here.
    pub async fn call(
        &self,
        audience: ToolAudience,
        operation: &str,
        input: &Value,
    ) -> Result<NativeMcpReply, NativeMcpFailure> {
        let declared = self
            .state
            .ir
            .ir()
            .operations
            .iter()
            .find(|candidate| candidate.name == operation)
            .ok_or(NativeMcpFailure::Operation)?;
        let entry = self
            .state
            .prepared
            .entry(operation)
            .ok_or(NativeMcpFailure::Operation)?;
        if !entry.governance.allows(audience) {
            return Err(NativeMcpFailure::Operation);
        }
        self.state
            .prepared
            .decode_input(operation, input)
            .map_err(|_| NativeMcpFailure::Input)?;
        if !input.is_object() {
            return Err(NativeMcpFailure::Input);
        }
        let _permit = Arc::clone(&self.state.permits)
            .acquire_owned()
            .await
            .map_err(|_| NativeMcpFailure::Disconnected)?;
        let peer = {
            let guard = self.client.inner.lock().await;
            let Some(McpClientInner::Sdk(inner)) = guard.as_ref() else {
                return Err(NativeMcpFailure::Disconnected);
            };
            if !inner
                .verified_native
                .as_ref()
                .is_some_and(|state| Arc::ptr_eq(state, &self.state))
                || inner.running.is_closed()
            {
                return Err(NativeMcpFailure::Disconnected);
            }
            inner.running.peer().clone()
        };
        let progress = super::client_progress::issue_token(&self.client.name, operation);
        let _progress_guard =
            progress
                .as_ref()
                .map(|token| super::client_progress::ProgressTokenGuard {
                    token: token.token.clone(),
                });
        let params = super::builtins::tool_call_params(operation, input.clone(), progress.as_ref());
        let typed_params: CallToolRequestParams =
            serde_json::from_value(params.clone()).map_err(|_| NativeMcpFailure::Input)?;
        let started_at = crate::clock_mock::instant_now();
        let response = tokio::time::timeout(super::MCP_TIMEOUT, peer.call_tool_once(typed_params))
            .await
            .map_err(|_| NativeMcpFailure::Deadline)
            .and_then(|result| {
                result.map_err(|error| match error {
                    rmcp::service::ServiceError::McpError(_) => NativeMcpFailure::Rpc,
                    rmcp::service::ServiceError::UnexpectedResponse => NativeMcpFailure::Protocol,
                    _ => NativeMcpFailure::Transport,
                })
            });
        let recorded = match &response {
            Ok(result) => {
                serde_json::json!({"jsonrpc":"2.0","result":rmcp::model::ServerResult::from(result.clone())})
            }
            Err(error) => {
                serde_json::json!({"jsonrpc":"2.0","error":{"message":error.to_string()}})
            }
        };
        crate::testbench::tape::record_mcp_json_rpc(
            &self.client.name,
            "tools/call",
            &serde_json::json!({"jsonrpc":"2.0","method":"tools/call","params":params}),
            &recorded,
            crate::clock_mock::instant_now()
                .duration_since(started_at)
                .as_millis()
                .min(u64::MAX as u128) as u64,
        );
        let response = response?;
        let CallToolResponse::Complete(response) = response else {
            return Err(NativeMcpFailure::Incomplete);
        };
        let metadata = response
            .meta
            .as_ref()
            .map(|meta| serde_json::to_value(meta).expect("MCP metadata is JSON"));
        let application = metadata
            .as_ref()
            .and_then(|meta| meta.get(HARN_MCP_TOOL_CONTRACT_META_KEY))
            .and_then(|contract| contract.get("applicationError"));
        if response.is_error == Some(true) {
            let error: ToolApplicationError =
                serde_json::from_value(application.cloned().ok_or(NativeMcpFailure::Handler)?)
                    .map_err(|_| NativeMcpFailure::Handler)?;
            if error.tool != operation || response.structured_content.is_some() {
                return Err(NativeMcpFailure::Handler);
            }
            let error = match error.outcome {
                Some(outcome) => self
                    .state
                    .prepared
                    .declared_failure(operation, &error.data, outcome)
                    .map_err(|_| NativeMcpFailure::Handler)?,
                None => match self
                    .state
                    .prepared
                    .classify_thrown_json(operation, &error.data)
                {
                    ToolThrownClassification::Application(error) => error,
                    _ => return Err(NativeMcpFailure::Handler),
                },
            };
            return Ok(NativeMcpReply {
                outcome: ToolInvocationOutcome::ApplicationError(error),
                metadata,
            });
        }
        if application.is_some() {
            return Err(NativeMcpFailure::Output);
        }
        let content = response
            .structured_content
            .ok_or(NativeMcpFailure::Output)?;
        let CodegenOutput::Value { mcp_envelope, .. } = &declared.output else {
            return Err(NativeMcpFailure::Output);
        };
        let json = match mcp_envelope {
            McpOutputEnvelope::Bare => content,
            McpOutputEnvelope::ResultProperty => {
                let mut object = content
                    .as_object()
                    .filter(|object| object.len() == 1)
                    .cloned()
                    .ok_or(NativeMcpFailure::Output)?;
                object.remove("result").ok_or(NativeMcpFailure::Output)?
            }
        };
        self.state
            .prepared
            .validate_output(operation, &json)
            .map_err(|_| NativeMcpFailure::Output)?;
        let value = self
            .state
            .prepared
            .prepared_wire_type(operation, ToolContractPhase::Output)
            .map_err(|_| NativeMcpFailure::Output)?
            .ok_or(NativeMcpFailure::Output)?
            .decode(&json)
            .map_err(|_| NativeMcpFailure::Output)?;
        Ok(NativeMcpReply {
            outcome: ToolInvocationOutcome::Success {
                value,
                json,
                text: None,
            },
            metadata,
        })
    }

    pub async fn disconnect(&self) -> Result<(), NativeMcpFailure> {
        self.client
            .disconnect()
            .await
            .map_err(|_| NativeMcpFailure::Transport)
    }

    async fn invoke_vm(
        &self,
        operation: &str,
        input: &crate::VmValue,
    ) -> Result<crate::VmValue, crate::VmError> {
        use crate::value::{VmDictExt, VmError, VmValue};
        let failed = |failure: NativeMcpFailure| {
            VmError::NonReplayable(Box::new(VmError::Runtime(failure.to_string())))
        };
        crate::run_tape::refuse_in_prefix("native MCP call", "MCP calls are not on the tape")?;
        let json = self
            .state
            .prepared
            .encode_wire_value(operation, ToolContractPhase::Input, input)
            .map_err(|_| failed(NativeMcpFailure::Input))?;
        let result = self
            .call(
                crate::tool_registry::preparation_scope::current_audience(),
                operation,
                &json,
            )
            .await
            .map_err(failed)?;
        match result.outcome {
            ToolInvocationOutcome::Success { value, .. } => Ok(value),
            ToolInvocationOutcome::ApplicationError(error) => {
                let value = self
                    .state
                    .prepared
                    .prepared_wire_type(operation, ToolContractPhase::ApplicationError)
                    .map_err(|_| failed(NativeMcpFailure::Handler))?
                    .ok_or_else(|| failed(NativeMcpFailure::Handler))?
                    .decode(&error.data)
                    .map_err(|_| failed(NativeMcpFailure::Handler))?;
                if let Some(outcome) = error.outcome {
                    let mut envelope = crate::value::DictMap::new();
                    envelope.put_str(
                        "schema",
                        crate::tool_registry::handler_result::AGENT_TOOL_HANDLER_RESULT_SCHEMA,
                    );
                    envelope.put_str(
                        "outcome",
                        match outcome {
                            crate::tool_registry::ToolApplicationOutcome::Error => "error",
                            crate::tool_registry::ToolApplicationOutcome::Rejected => "rejected",
                        },
                    );
                    envelope.put_str("text", "native tool returned declared application data");
                    envelope.put("data", value);
                    Ok(VmValue::dict(envelope))
                } else {
                    Err(VmError::NonReplayable(Box::new(VmError::DeclaredThrown(
                        value,
                    ))))
                }
            }
        }
    }
}

pub(super) fn register(vm: &mut crate::Vm) {
    use crate::VmValue;
    vm.register_async_capability_method(
        harn_builtin_meta::CapabilityId::Tools,
        "mcp_native_catalog",
        |_ctx, args| async move {
            let client =
                super::builtins::client_arg(&args, "mcp_native_catalog", "first argument")?;
            let binding = client
                .verified_native_binding()
                .await
                .map_err(|error| crate::VmError::Runtime(error.to_string()))?;
            let catalog =
                serde_json::to_value(binding.catalog()).expect("prepared catalog is JSON");
            let value = crate::schema::json_to_vm_value(&catalog);
            if crate::tool_registry::result_to_json(&value).ok().as_ref() != Some(&catalog) {
                return Err(crate::VmError::Runtime(
                    "native catalog cannot be represented exactly in this VM".into(),
                ));
            }
            Ok(value)
        },
    );
    vm.register_async_capability_method(
        harn_builtin_meta::CapabilityId::Tools,
        "mcp_native_call",
        |_ctx, args| async move {
            let client = super::builtins::client_arg(&args, "mcp_native_call", "first argument")?;
            let Some(VmValue::String(operation)) = args.get(1) else {
                return Err(crate::VmError::TypeError(
                    "native operation must be a string".into(),
                ));
            };
            let input = args
                .get(2)
                .ok_or_else(|| crate::VmError::TypeError("native input is required".into()))?;
            let value = client
                .verified_native_binding()
                .await
                .map_err(|error| crate::VmError::Runtime(error.to_string()))?
                .invoke_vm(operation, input)
                .await?;
            let mut reply = crate::value::DictMap::new();
            use crate::value::VmDictExt;
            reply.put("value", value);
            Ok(VmValue::dict(reply))
        },
    );
}
