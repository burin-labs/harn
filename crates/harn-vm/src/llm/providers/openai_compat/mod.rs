//! Native transport and catalog adapter for OpenAI-compatible providers.
//!
//! The shared LLM crate owns request encoding. This adapter captures native
//! route policy, publishes encoding receipts, and dispatches the HTTP request.

#[cfg(test)]
use harn_llm::wire::openai::messages;
mod streaming;

#[cfg(test)]
mod tests;

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::value::VmError;
pub(crate) use harn_provider_catalog::capabilities::gpt_generation;

use self::streaming::canonicalizing_delta_tx;

pub(crate) use harn_llm::wire::openai::ensure_openrouter_require_parameters;

/// True for GPT models that expose OpenAI's Responses-API `tool_search` meta-tool
/// and the `defer_loading: true` flag on user tool definitions. Per OpenAI's
/// docs, the feature is gated on GPT 5.4+ (hosted + client-executed modes).
/// We intentionally ignore legacy `gpt-4*`, `gpt-3.5*`, and any non-GPT model;
/// those fall back to the client-executed path from harn#70.
///
/// Retained only as a pure-parse helper for `capabilities::lookup` callers
/// that want to ask the model-ID question without loading the full rule
/// table. The authoritative gate is
/// `capabilities::lookup(provider, model).defer_loading`.
#[allow(dead_code)]
pub(crate) fn gpt_model_supports_tool_search(model: &str) -> bool {
    match gpt_generation(model) {
        Some((major, minor)) => (major, minor) >= (5, 4),
        None => false,
    }
}

/// OpenAI-compatible provider parameterized by name. A single struct handles
/// all OpenAI-style backends — the provider name is used to resolve config
/// (base URL, auth, etc.) from `llm_config`.
pub(crate) struct OpenAiCompatibleProvider {
    provider_name: String,
}

impl OpenAiCompatibleProvider {
    pub(crate) fn new(name: String) -> Self {
        Self {
            provider_name: name,
        }
    }

    fn transform_request_with_caps(
        body: &mut serde_json::Value,
        caps: &crate::llm::capabilities::Capabilities,
    ) {
        if let Some(object) = body.as_object_mut() {
            let allowed_field =
                if caps.honors_chat_template_kwargs || caps.honors_preserve_thinking_kwarg {
                    Some(harn_llm::wire::openai::chat_template_options_field(caps))
                } else {
                    None
                };
            for field in ["chat_template_kwargs", "chat_template_args"] {
                if allowed_field != Some(field) {
                    object.remove(field);
                }
            }
        }
    }
}

impl LlmProvider for OpenAiCompatibleProvider {
    fn name(&self) -> &str {
        &self.provider_name
    }

    /// Apply provider-specific request body transformations.
    fn transform_request(&self, body: &mut serde_json::Value) {
        let model = body
            .get("model")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let caps = crate::llm::managed_supply::capabilities_for(&self.provider_name, model);
        Self::transform_request_with_caps(body, &caps);
    }

    // `supports_defer_loading` and `native_tool_search_variants` are
    // served by the default trait impl, which reads `capabilities.toml`.
    // The `gpt_model_supports_tool_search` helper below is retained for
    // shape detection in `helpers/options.rs::classify_native_shape`
    // (deciding Anthropic- vs OpenAI-wire shape for the mock provider).
}

impl LlmProviderChat for OpenAiCompatibleProvider {
    fn chat<'a>(
        &'a self,
        request: &'a LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> crate::llm::provider::ProviderChatFuture<'a> {
        Box::pin(self.chat_impl(request, delta_tx))
    }
}

impl OpenAiCompatibleProvider {
    /// Build the OpenAI-compatible request body.
    pub(crate) fn build_request_body(opts: &LlmRequestPayload) -> serde_json::Value {
        let caps = crate::llm::managed_supply::capabilities_for(&opts.provider, &opts.model);
        Self::build_request_body_with_caps(opts, &caps)
    }

    pub(crate) fn build_request_body_with_caps(
        opts: &LlmRequestPayload,
        caps: &crate::llm::capabilities::Capabilities,
    ) -> serde_json::Value {
        let wire_model = crate::llm_config::wire_model_id(&opts.model);
        let fast_tier = opts
            .fast
            .then(|| crate::llm::serving_tiers::fast_tier(&opts.model))
            .flatten()
            .filter(crate::llm::serving_tiers::is_usable);
        let tool_search_extensions = opts.native_tools.as_deref().is_some_and(|tools| {
            crate::llm::provider::openai_tool_search_wire_extensions_enabled(
                &opts.provider,
                &opts.model,
                tools,
                opts.provider_overrides.as_ref(),
            )
        });
        let context = harn_llm::wire::openai::OpenAiWireContext {
            capabilities: caps,
            wire_model: &wire_model,
            may_shape_reasoning: crate::llm::catalog_may_shape_requested_reasoning(),
            tool_search_extensions,
            fast_request: fast_tier.as_ref().and_then(|tier| tier.request.as_ref()),
        };
        crate::llm::providers::schema_compat::publish_wire_request(
            harn_llm::wire::openai::encode_request(opts, &context),
        )
    }

    /// The actual chat implementation.
    pub(crate) async fn chat_impl(
        &self,
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        let dialect = crate::llm::api::DialectContract::for_request(request);
        self.chat_impl_with_dialect(request, delta_tx, dialect)
            .await
    }

    pub(crate) async fn chat_impl_with_dialect(
        &self,
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
        dialect: crate::llm::api::DialectContract,
    ) -> Result<LlmResult, VmError> {
        let caps = crate::llm::managed_supply::capabilities_for(&request.provider, &request.model);
        let mut body = dialect.build_openai_request_body_with_caps(request, &caps);
        Self::transform_request_with_caps(&mut body, &caps);

        // For reserved-tool-call-token models the prompt was sent with the
        // delimiters remapped (see `build_request_body`). The streamed live
        // deltas are canonicalized here (across chunk boundaries) so the live
        // display never shows the wire form; the assembled `result.text` is
        // mapped back to canonical in the shared transport funnel
        // (`vm_call_llm_api_with_body`), which is the single boundary covering
        // every route — registered and unregistered, streaming and not.
        let remap_tool_call = caps.reserved_tool_call_token;
        let delta_tx = if remap_tool_call {
            delta_tx.map(canonicalizing_delta_tx)
        } else {
            delta_tx
        };
        let result =
            crate::llm::api::vm_call_llm_api_with_body(request, delta_tx, body, dialect).await?;
        Ok(result)
    }
}
