use super::*;

// `ProviderPayloadSchema` carries types that aren't `Eq` (e.g. JSON values),
// so `HarnConnectorContract` can only be `PartialEq`.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, Debug, PartialEq)]
pub struct HarnConnectorContract {
    pub module_path: PathBuf,
    pub provider_id: ProviderId,
    pub kinds: Vec<TriggerKind>,
    pub payload_schema: ProviderPayloadSchema,
    /// Exact provider method ids exported by `methods()` when the connector
    /// publishes an outbound operation inventory.
    pub method_ids: Option<Vec<String>>,
    pub has_poll_tick: bool,
}

pub async fn load_contract(module_path: &Path) -> Result<HarnConnectorContract, ConnectorError> {
    let (base_vm, exports) = load_module_runtime(module_path).await?;
    abi::validate_runtime_export_abi(&exports)?;
    let provider_id =
        parse_provider_id(required_export_call(&base_vm, &exports, "provider_id", &[]).await?)?;
    let kinds = parse_kinds(required_export_call(&base_vm, &exports, "kinds", &[]).await?)?;
    let payload_schema = parse_payload_schema(
        required_export_call(&base_vm, &exports, "payload_schema", &[]).await?,
    )?;
    let method_ids = match exports.get("methods") {
        Some(_) => Some(parse_method_ids(
            required_export_call(&base_vm, &exports, "methods", &[]).await?,
        )?),
        None => None,
    };
    Ok(HarnConnectorContract {
        module_path: module_path.to_path_buf(),
        provider_id,
        kinds,
        payload_schema,
        method_ids,
        has_poll_tick: exports.contains_key("poll_tick"),
    })
}

fn parse_method_ids(value: VmValue) -> Result<Vec<String>, ConnectorError> {
    let json = vm_value_to_json(&value);
    let items = json
        .as_array()
        .ok_or_else(|| ConnectorError::HarnRuntime("methods() must return a list".to_string()))?;
    let mut ids = Vec::with_capacity(items.len());
    let mut seen = std::collections::BTreeSet::new();
    for item in items {
        let name = item
            .as_object()
            .and_then(|descriptor| descriptor.get("name"))
            .and_then(JsonValue::as_str)
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| {
                ConnectorError::HarnRuntime(
                    "methods() entries must be objects with a non-empty string name".to_string(),
                )
            })?;
        if !seen.insert(name) {
            return Err(ConnectorError::HarnRuntime(format!(
                "methods() repeats operation id '{name}'"
            )));
        }
        ids.push(name.to_string());
    }
    Ok(ids)
}
