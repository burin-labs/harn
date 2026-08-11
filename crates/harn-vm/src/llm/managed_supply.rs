//! Managed provider supply: one contract separating logical capability
//! identity from the HTTP/auth adapter that transports a request.
//!
//! Gateways opt in through [`crate::llm_config::ProviderDef::managed_supply`].
//! Callers keep selecting ordinary catalog models; this module resolves that
//! model's owning provider once, fingerprints Harn's complete resolved
//! capability projection, and emits a versioned request extension. A gateway
//! pinned to the same Harn contract can verify the fingerprint and choose only
//! a compatible physical route without either side copying capability tables.

use serde::{Deserialize, Serialize};

use crate::value::{VmError, VmValue};

mod provider_wire;

pub use provider_wire::{
    hosted_openai_request, HostedAudioFormat, HostedChatMessage, HostedChatRequest, HostedContent,
    HostedContentPart, HostedFunctionDefinition, HostedFunctionTool, HostedImageDetail,
    HostedImageUrl, HostedInputAudio, HostedNamedFunction, HostedNamedToolChoice,
    HostedOpenAiRequest, HostedRole, HostedStreamOptions, HostedToolCall, HostedToolCallFunction,
    HostedToolChoice, HostedToolChoiceMode, HostedToolKind,
};

pub const MANAGED_SUPPLY_WIRE_KEY: &str = "harn_managed_supply";
pub const MANAGED_SUPPLY_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedSupplyContractError {
    message: String,
}

impl ManagedSupplyContractError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ManagedSupplyContractError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ManagedSupplyContractError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedSupplyLogicalRoute {
    pub provider: String,
    pub model: String,
    pub capability_fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedSupplyRequest {
    pub version: u32,
    pub logical_route: ManagedSupplyLogicalRoute,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSupplyCostBasis {
    Actual,
    ConservativeEstimate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSupplyCapabilityMode {
    Exact,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSupplyRoutingOutcome {
    Skipped,
    Timeout,
    TransportError,
    InvalidResponse,
    ProviderError,
    BudgetExceeded,
    ContentPolicy,
    Success,
}

/// Stable audit projection for one hosted routing attempt.
///
/// Provider payloads, credential names, and gateway-internal policy objects do
/// not belong on this cross-product wire. `detail_code` is a bounded gateway
/// classification such as `missing_provider_secret`; it must never contain a
/// provider error message or secret material.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedSupplyRoutingAttempt {
    pub provider: String,
    pub model: String,
    pub outcome: ManagedSupplyRoutingOutcome,
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedSupplyServedRoute {
    pub provider: String,
    pub model: String,
    pub capability_fingerprint: String,
}

/// Authoritative terminal receipt emitted by the managed gateway.
///
/// Decimal money remains a string on the wire so gateways can preserve exact
/// scale. Harn converts it to `f64` only at its existing public usage seam.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedSupplyReceipt {
    pub version: u32,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_request_id: Option<String>,
    pub served_route: ManagedSupplyServedRoute,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: String,
    pub cost_basis: ManagedSupplyCostBasis,
    pub capability_mode: ManagedSupplyCapabilityMode,
    #[serde(default)]
    pub routing_attempts: Vec<ManagedSupplyRoutingAttempt>,
}

fn invalid(message: impl std::fmt::Display) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(message.to_string())))
}

fn validate_catalog_route(
    provider: &str,
    model: &str,
    fingerprint: &str,
    label: &str,
) -> Result<(), ManagedSupplyContractError> {
    if provider.trim().is_empty()
        || model.trim().is_empty()
        || fingerprint.len() != 64
        || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ManagedSupplyContractError::new(format!(
            "managed supply {label} is missing a canonical route identity or valid capability fingerprint"
        )));
    }
    crate::llm_config::model_catalog_entry(model)
        .filter(|entry| entry.provider == provider)
        .ok_or_else(|| {
            ManagedSupplyContractError::new(format!(
                "managed supply {label} route {provider}:{model} is not a canonical Harn catalog route"
            ))
        })?;
    let expected = capability_fingerprint(provider, model);
    if fingerprint != expected {
        return Err(ManagedSupplyContractError::new(format!(
            "managed supply {label} capability fingerprint does not match Harn's catalog for {provider}:{model}"
        )));
    }
    Ok(())
}

/// Validate the complete caller-owned request identity against Harn's active
/// catalog. Hosted gateways call this boundary instead of copying catalog and
/// fingerprint validation rules.
pub fn validate_request(request: &ManagedSupplyRequest) -> Result<(), ManagedSupplyContractError> {
    if request.version != MANAGED_SUPPLY_VERSION {
        return Err(ManagedSupplyContractError::new(format!(
            "managed supply request version {} is unsupported; expected {MANAGED_SUPPLY_VERSION}",
            request.version
        )));
    }
    validate_catalog_route(
        &request.logical_route.provider,
        &request.logical_route.model,
        &request.logical_route.capability_fingerprint,
        "request logical",
    )
}

/// Produce the canonical receipt route for a gateway-selected physical route,
/// rejecting unknown or capability-incompatible routes before provider
/// egress. This is the one compatibility decision shared by every hosted
/// adapter.
pub fn compatible_served_route(
    request: &ManagedSupplyRequest,
    provider: &str,
    model: &str,
) -> Result<ManagedSupplyServedRoute, ManagedSupplyContractError> {
    validate_request(request)?;
    let served = ManagedSupplyServedRoute {
        provider: provider.to_string(),
        model: model.to_string(),
        capability_fingerprint: capability_fingerprint(provider, model),
    };
    validate_catalog_route(
        &served.provider,
        &served.model,
        &served.capability_fingerprint,
        "served",
    )?;
    if served.capability_fingerprint != request.logical_route.capability_fingerprint {
        return Err(ManagedSupplyContractError::new(format!(
            "managed supply served route {}:{} is not capability-compatible with requested logical route {}:{}",
            served.provider,
            served.model,
            request.logical_route.provider,
            request.logical_route.model
        )));
    }
    Ok(served)
}

pub fn is_managed_transport(provider: &str) -> bool {
    crate::llm_config::provider_config(provider)
        .and_then(|definition| definition.managed_supply)
        .is_some()
}

/// Resolve the catalog-owned capability route for one transport request.
/// Ordinary providers return their input unchanged. Managed transports reject
/// unknown models because guessing a capability owner would make prompt and
/// tool decisions unverifiable.
pub fn logical_route(transport_provider: &str, model: &str) -> Result<(String, String), VmError> {
    let Some(definition) = crate::llm_config::provider_config(transport_provider) else {
        return Ok((transport_provider.to_string(), model.to_string()));
    };
    let Some(declaration) = definition.managed_supply else {
        return Ok((transport_provider.to_string(), model.to_string()));
    };
    if definition.protocol.is_some() {
        return Err(invalid(format!(
            "managed supply provider {transport_provider:?} must use the HTTP provider transport, not protocol {:?}",
            definition.protocol
        )));
    }
    if declaration.version != MANAGED_SUPPLY_VERSION {
        return Err(invalid(format!(
            "managed supply provider {transport_provider:?} declares unsupported version {}; expected {MANAGED_SUPPLY_VERSION}",
            declaration.version
        )));
    }
    let catalog = crate::llm_config::model_catalog_entry(model).ok_or_else(|| {
        invalid(format!(
            "managed supply provider {transport_provider:?} requires a known Harn catalog model; {model:?} has no unambiguous capability owner"
        ))
    })?;
    Ok((catalog.provider, model.to_string()))
}

pub fn capability_fingerprint(provider: &str, model: &str) -> String {
    let caps = crate::llm::capabilities::lookup(provider, model);
    let projected = crate::llm::config_builtins::capabilities_to_vm_value(provider, model, &caps);
    let mut json = crate::llm::helpers::vm_value_to_json(&projected);
    if let Some(object) = json.as_object_mut() {
        // Route identity already travels beside the fingerprint. Hash only the
        // live render/dispatch contract so two physical routes with the same
        // effective capabilities remain interchangeable. Batch fields cannot
        // affect a live managed-supply chat request and would otherwise make
        // provider batch-product differences spuriously reject a route.
        object.remove("provider");
        object.remove("model");
        object.remove("batch");
        object.retain(|key, _| key != "batch_api" && !key.starts_with("batch_"));
    }
    let bytes = serde_json::to_vec(&json).expect("capability projection is JSON serializable");
    blake3::hash(&bytes).to_hex().to_string()
}

/// Capability lookup for code paths whose transport request has already
/// passed [`logical_route`] preflight. The shared transport re-validates and
/// returns the typed error before network egress, so this infallible projection
/// is reserved for request shaping and render contexts that cannot return an
/// error in their existing interface.
pub(crate) fn capabilities_for(
    transport_provider: &str,
    model: &str,
) -> crate::llm::capabilities::Capabilities {
    let (provider, model) = logical_route(transport_provider, model)
        .unwrap_or_else(|_| (transport_provider.to_string(), model.to_string()));
    crate::llm::capabilities::lookup(&provider, &model)
}

pub fn request_for(
    transport_provider: &str,
    model: &str,
) -> Result<Option<ManagedSupplyRequest>, VmError> {
    if !is_managed_transport(transport_provider) {
        return Ok(None);
    }
    let (provider, model) = logical_route(transport_provider, model)?;
    Ok(Some(ManagedSupplyRequest {
        version: MANAGED_SUPPLY_VERSION,
        logical_route: ManagedSupplyLogicalRoute {
            capability_fingerprint: capability_fingerprint(&provider, &model),
            provider,
            model,
        },
    }))
}

pub(crate) fn attach_request_extension(
    body: &mut serde_json::Value,
    transport_provider: &str,
    model: &str,
) -> Result<(), VmError> {
    let Some(request) = request_for(transport_provider, model)? else {
        return Ok(());
    };
    let object = body
        .as_object_mut()
        .ok_or_else(|| invalid("managed supply request body must be a JSON object"))?;
    object.insert(
        MANAGED_SUPPLY_WIRE_KEY.to_string(),
        serde_json::to_value(request).expect("managed supply request is JSON serializable"),
    );
    Ok(())
}

fn receipt_from_metadata(
    metadata: Option<&serde_json::Value>,
) -> Result<Option<ManagedSupplyReceipt>, VmError> {
    let Some(value) = metadata.and_then(|value| value.get(MANAGED_SUPPLY_WIRE_KEY)) else {
        return Ok(None);
    };
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|error| {
            invalid(format!(
                "managed supply response has an invalid {MANAGED_SUPPLY_WIRE_KEY} receipt: {error}"
            ))
        })
}

fn validate_receipt_fields(
    receipt: &ManagedSupplyReceipt,
) -> Result<f64, ManagedSupplyContractError> {
    if receipt.version != MANAGED_SUPPLY_VERSION {
        return Err(ManagedSupplyContractError::new(format!(
            "managed supply response version {} is unsupported; expected {MANAGED_SUPPLY_VERSION}",
            receipt.version
        )));
    }
    if receipt.request_id.trim().is_empty() || receipt.input_tokens < 0 || receipt.output_tokens < 0
    {
        return Err(ManagedSupplyContractError::new(
            "managed supply response receipt is missing a request identity or has invalid token counts",
        ));
    }
    validate_catalog_route(
        &receipt.served_route.provider,
        &receipt.served_route.model,
        &receipt.served_route.capability_fingerprint,
        "response served",
    )?;
    for attempt in &receipt.routing_attempts {
        if attempt.provider.trim().is_empty()
            || attempt.model.trim().is_empty()
            || attempt
                .detail_code
                .as_ref()
                .is_some_and(|code| code.trim().is_empty())
        {
            return Err(ManagedSupplyContractError::new(
                "managed supply response has an invalid routing attempt",
            ));
        }
    }
    let cost = receipt.cost_usd.parse::<f64>().map_err(|_| {
        ManagedSupplyContractError::new(
            "managed supply response receipt cost_usd must be a decimal string",
        )
    })?;
    if !cost.is_finite() || cost < 0.0 {
        return Err(ManagedSupplyContractError::new(
            "managed supply response receipt cost_usd must be finite and non-negative",
        ));
    }
    Ok(cost)
}

/// Validate an authoritative terminal receipt against its exact originating
/// request. Gateways can use this before serialization; clients use the same
/// decision before projecting identity, tokens, and cost into normal usage.
pub fn validate_receipt(
    request: &ManagedSupplyRequest,
    receipt: &ManagedSupplyReceipt,
) -> Result<(), ManagedSupplyContractError> {
    validate_request(request)?;
    validate_receipt_fields(receipt)?;
    if receipt.served_route.capability_fingerprint != request.logical_route.capability_fingerprint {
        return Err(ManagedSupplyContractError::new(format!(
            "managed supply response served route {}:{} is not capability-compatible with requested logical route {}:{}",
            receipt.served_route.provider,
            receipt.served_route.model,
            request.logical_route.provider,
            request.logical_route.model
        )));
    }
    Ok(())
}

/// Apply the terminal gateway receipt to the normal result identity and usage
/// fields. Managed transports require the receipt; a successful HTTP payload
/// without it is not accepted as an authoritative completion.
pub(crate) fn apply_terminal_receipt(
    result: &mut crate::llm::api::LlmResult,
    transport_provider: &str,
    requested_model: &str,
) -> Result<(), VmError> {
    if !is_managed_transport(transport_provider) {
        return Ok(());
    }
    let receipt = receipt_from_metadata(result.telemetry.provider_metadata.as_ref())?
        .ok_or_else(|| invalid("managed supply response is missing its terminal receipt"))?;
    let request = request_for(transport_provider, requested_model)?
        .expect("managed transport produces a managed-supply request");
    validate_receipt(&request, &receipt).map_err(invalid)?;
    result.provider = receipt.served_route.provider.clone();
    result.model = receipt.served_route.model.clone();
    result.input_tokens = receipt.input_tokens;
    result.output_tokens = receipt.output_tokens;
    result.telemetry.request_id = receipt
        .provider_request_id
        .clone()
        .or_else(|| Some(receipt.request_id.clone()));
    Ok(())
}

pub(crate) fn authoritative_cost_usd(result: &crate::llm::api::LlmResult) -> Option<f64> {
    let receipt = receipt_from_metadata(result.telemetry.provider_metadata.as_ref())
        .ok()
        .flatten()?;
    validate_receipt_fields(&receipt).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_route_keeps_one_identity_and_emits_no_extension() {
        assert_eq!(
            logical_route("openai", "gpt-4o-mini").expect("direct route"),
            ("openai".to_string(), "gpt-4o-mini".to_string())
        );
        assert_eq!(
            request_for("openai", "gpt-4o-mini").expect("direct request"),
            None
        );
        let mut body = serde_json::json!({"model": "gpt-4o-mini"});
        attach_request_extension(&mut body, "openai", "gpt-4o-mini")
            .expect("direct request remains unchanged");
        assert!(body.get(MANAGED_SUPPLY_WIRE_KEY).is_none());
    }

    #[test]
    fn capability_fingerprint_is_stable_and_capability_sensitive() {
        let first = capability_fingerprint("openai", "gpt-4o-mini");
        assert_eq!(first, capability_fingerprint("openai", "gpt-4o-mini"));
        assert_ne!(
            first,
            capability_fingerprint("anthropic", "claude-haiku-4-5")
        );
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn gateway_validator_owns_canonical_request_and_route_compatibility() {
        let request = ManagedSupplyRequest {
            version: MANAGED_SUPPLY_VERSION,
            logical_route: ManagedSupplyLogicalRoute {
                provider: "groq".to_string(),
                model: "llama-3.3-70b-versatile".to_string(),
                capability_fingerprint: capability_fingerprint("groq", "llama-3.3-70b-versatile"),
            },
        };
        validate_request(&request).expect("canonical logical route");
        let served = compatible_served_route(&request, "groq", "llama-3.3-70b-versatile")
            .expect("same-capability route");
        assert_eq!(served.provider, "groq");

        let error = compatible_served_route(&request, "anthropic", "claude-haiku-4-5-20251001")
            .expect_err("incompatible route");
        assert!(error.to_string().contains("not capability-compatible"));
    }

    #[test]
    fn gateway_validator_rejects_copied_or_stale_capability_fingerprint() {
        let request = ManagedSupplyRequest {
            version: MANAGED_SUPPLY_VERSION,
            logical_route: ManagedSupplyLogicalRoute {
                provider: "groq".to_string(),
                model: "llama-3.3-70b-versatile".to_string(),
                capability_fingerprint: "0".repeat(64),
            },
        };
        let error = validate_request(&request).expect_err("stale fingerprint");
        assert!(error.to_string().contains("does not match Harn's catalog"));
    }

    #[test]
    fn provider_contract_parses_and_survives_overlay_merge() {
        let parsed = crate::llm_config::parse_config_toml(
            r#"
[providers.gateway]
base_url = "https://gateway.example.invalid/v1"
chat_endpoint = "/chat/completions"
managed_supply = { version = 1 }
"#,
        )
        .expect("managed-supply provider config");
        let mut merged = crate::llm_config::ProvidersConfig::default();
        merged.merge_from(&parsed);
        assert_eq!(
            merged.providers["gateway"].managed_supply,
            Some(crate::llm_config::ManagedSupplyProviderDef { version: 1 })
        );
    }

    #[test]
    fn provider_contract_rejects_unknown_nested_fields() {
        let error = crate::llm_config::parse_config_toml(
            r"
[providers.gateway]
managed_supply = { version = 1, copied_capability_table = true }
",
        )
        .expect_err("managed-supply shape must remain a closed typed contract");
        assert!(error.to_string().contains("copied_capability_table"));
    }
}
