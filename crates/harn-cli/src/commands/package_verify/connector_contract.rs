use super::*;

pub(super) async fn check_one_connector(
    provider_id: harn_vm::ProviderId,
    manifest_dir: &Path,
    module: &str,
    service: Option<&package::ConnectorServiceManifest>,
    contract_version: u32,
    fixtures: &[ConnectorContractFixture],
    run_poll_tick: bool,
) -> Result<CheckedConnector, String> {
    use harn_vm::Connector as _;

    let module_path = harn_vm::resolve_module_import_path(manifest_dir, module);
    if !module_path.is_file() {
        return Err(format!(
            "provider '{}' connector module '{}' does not exist",
            provider_id.as_str(),
            module_path.display()
        ));
    }
    let effect_policy_diagnostics = connector_effect_policy_diagnostics(&module_path)?;
    if !effect_policy_diagnostics.is_empty() {
        return Err(format!(
            "provider '{}' connector module '{}' violates connector effect policy:\n{}",
            provider_id.as_str(),
            module_path.display(),
            effect_policy_diagnostics
                .into_iter()
                .map(|diagnostic| format!("- {diagnostic}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    let contract = harn_vm::load_harn_connector_contract(&module_path)
        .await
        .map_err(|error| {
            format!(
                "failed to load connector module '{}' for provider '{}': {error}",
                module_path.display(),
                provider_id.as_str()
            )
        })?;
    if contract.provider_id != provider_id {
        return Err(format!(
            "provider '{}' resolves to connector module '{}' which declares provider_id '{}'",
            provider_id.as_str(),
            module_path.display(),
            contract.provider_id.as_str()
        ));
    }
    if contract.kinds.is_empty() {
        return Err(format!(
            "provider '{}' kinds() must return at least one trigger kind",
            provider_id.as_str()
        ));
    }
    if contract.payload_schema.harn_schema_name.trim().is_empty() {
        return Err(format!(
            "provider '{}' payload_schema().harn_schema_name must not be empty",
            provider_id.as_str()
        ));
    }
    if !contract.payload_schema.json_schema.is_null()
        && !contract.payload_schema.json_schema.is_object()
    {
        return Err(format!(
            "provider '{}' payload_schema().json_schema must be an object when present",
            provider_id.as_str()
        ));
    }
    if contract.kinds.iter().any(|kind| kind.as_str() == "poll") && !contract.has_poll_tick {
        return Err(format!(
            "provider '{}' declares kind 'poll' but does not export poll_tick(ctx)",
            provider_id.as_str()
        ));
    }
    validate_method_inventory(
        provider_id.as_str(),
        service,
        contract_version,
        contract.method_ids.as_deref(),
    )?;

    let mut connector = harn_vm::HarnConnector::load(&module_path)
        .await
        .map_err(|error| {
            format!(
                "failed to instantiate connector module '{}' for provider '{}': {error}",
                module_path.display(),
                provider_id.as_str()
            )
        })?;
    let ctx = connector_ctx().await?;
    connector.init(ctx).await.map_err(|error| {
        format!(
            "provider '{}' init(ctx) failed: {error}",
            provider_id.as_str()
        )
    })?;

    let activation_bindings = contract
        .kinds
        .iter()
        .filter(|kind| run_poll_tick || kind.as_str() != "poll")
        .map(|kind| {
            let mut binding = harn_vm::TriggerBinding::new(
                provider_id.clone(),
                kind.clone(),
                format!("contract-{}-{}", provider_id.as_str(), kind.as_str()),
            );
            binding.dedupe_key = Some("event.dedupe_key".to_string());
            if kind.as_str() == "poll" {
                binding.config = json!({
                    "poll": {
                        "interval_secs": 3600,
                        "state_key": "contract-check",
                        "lease_id": "contract-check",
                        "max_batch_size": 10,
                    }
                });
            }
            binding
        })
        .collect::<Vec<_>>();
    if !activation_bindings.is_empty() {
        connector
            .activate(&activation_bindings)
            .await
            .map_err(|error| {
                format!(
                    "provider '{}' activate(bindings) failed: {error}",
                    provider_id.as_str()
                )
            })?;
        if run_poll_tick {
            tokio::task::yield_now().await;
        }
    }

    match connector
        .client()
        .call("__harn_contract_check__", json!({}))
        .await
    {
        Ok(_) | Err(harn_vm::ClientError::MethodNotFound(_)) => {}
        Err(error) => {
            connector
                .shutdown(StdDuration::ZERO)
                .await
                .map_err(|shutdown_error| shutdown_error.to_string())?;
            return Err(format!(
                "provider '{}' call(method, args) validation failed: {error}",
                provider_id.as_str()
            ));
        }
    }

    let mut checked_fixtures = Vec::new();
    for fixture in fixtures
        .iter()
        .filter(|fixture| fixture.provider == provider_id)
    {
        let raw = raw_from_fixture(fixture)?;
        let result = match connector.normalize_inbound_result(raw).await {
            Ok(result) => {
                if let Some(expected) = fixture.expect_error_contains.as_deref() {
                    return Err(format!(
                        "provider '{}' normalize_inbound fixture '{}' expected error containing '{}' but succeeded",
                        provider_id.as_str(),
                        fixture_name(fixture),
                        expected
                    ));
                }
                result
            }
            Err(error) => {
                if let Some(expected) = fixture.expect_error_contains.as_deref() {
                    let message = error.to_string();
                    if message.contains(expected) {
                        checked_fixtures.push(CheckedFixture {
                            name: fixture_name(fixture),
                            result_type: "error".to_string(),
                            event_count: 0,
                        });
                        continue;
                    }
                    return Err(format!(
                        "provider '{}' normalize_inbound fixture '{}' expected error containing '{}' but got: {message}",
                        provider_id.as_str(),
                        fixture_name(fixture),
                        expected
                    ));
                }
                return Err(format!(
                    "provider '{}' normalize_inbound fixture '{}' failed: {error}",
                    provider_id.as_str(),
                    fixture_name(fixture)
                ));
            }
        };
        let checked = validate_normalize_result(fixture, &result)?;
        checked_fixtures.push(checked);
    }

    connector
        .shutdown(StdDuration::ZERO)
        .await
        .map_err(|error| {
            format!(
                "provider '{}' shutdown() failed: {error}",
                provider_id.as_str()
            )
        })?;

    Ok(CheckedConnector {
        provider: provider_id.as_str().to_string(),
        module: module_path.display().to_string(),
        kinds: contract
            .kinds
            .iter()
            .map(|kind| kind.as_str().to_string())
            .collect(),
        payload_schema: contract.payload_schema.harn_schema_name,
        has_poll_tick: contract.has_poll_tick,
        fixtures: checked_fixtures,
    })
}

fn validate_method_inventory(
    provider_id: &str,
    service: Option<&package::ConnectorServiceManifest>,
    contract_version: u32,
    exported_method_ids: Option<&[String]>,
) -> Result<(), String> {
    if contract_version < 2 {
        return Ok(());
    }
    let service = service.ok_or_else(|| {
        format!("provider '{provider_id}' connector contract v2 requires service metadata")
    })?;
    let exported_method_ids = exported_method_ids.ok_or_else(|| {
        format!(
            "provider '{provider_id}' connector contract v2 must export methods() so its runtime allowlist can be checked against service.operations"
        )
    })?;
    let declared = service
        .operations
        .iter()
        .map(|operation| operation.id.as_str())
        .collect::<BTreeSet<_>>();
    let exported = exported_method_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let missing_from_manifest = exported.difference(&declared).copied().collect::<Vec<_>>();
    let missing_from_runtime = declared.difference(&exported).copied().collect::<Vec<_>>();
    if missing_from_manifest.is_empty() && missing_from_runtime.is_empty() {
        return Ok(());
    }
    Err(format!(
        "provider '{provider_id}' methods() and service.operations differ (runtime-only: [{}]; manifest-only: [{}])",
        missing_from_manifest.join(", "),
        missing_from_runtime.join(", ")
    ))
}

async fn connector_ctx() -> Result<harn_vm::ConnectorCtx, String> {
    let event_log = Arc::new(harn_vm::event_log::AnyEventLog::Memory(
        harn_vm::event_log::MemoryEventLog::new(128),
    ));
    let metrics = Arc::new(harn_vm::MetricsRegistry::default());
    let inbox = harn_vm::InboxIndex::new(event_log.clone(), metrics.clone())
        .await
        .map_err(|error| error.to_string())?;
    Ok(harn_vm::ConnectorCtx {
        event_log,
        secrets: Arc::new(ContractSecretProvider::default()),
        inbox: Arc::new(inbox),
        metrics,
        rate_limiter: Arc::new(harn_vm::RateLimiterFactory::default()),
    })
}

fn connector_effect_policy_diagnostics(module_path: &Path) -> Result<Vec<String>, String> {
    let source = std::fs::read_to_string(module_path)
        .map_err(|error| format!("failed to read {}: {error}", module_path.display()))?;
    let program = harn_parser::parse_source(&source)
        .map_err(|error| format!("failed to parse {}: {error}", module_path.display()))?;
    Ok(harn_lint::lint_with_source(&program, &source)
        .into_iter()
        .filter(|diagnostic| diagnostic.rule == "connector-effect-policy")
        .map(|diagnostic| {
            format!(
                "{}:{} [{}]: {}",
                diagnostic.span.line, diagnostic.span.column, diagnostic.rule, diagnostic.message
            )
        })
        .collect())
}

#[derive(Default)]
struct ContractSecretProvider {
    values: BTreeMap<String, String>,
}

#[async_trait]
impl harn_vm::secrets::SecretProvider for ContractSecretProvider {
    async fn get(
        &self,
        id: &harn_vm::secrets::SecretId,
    ) -> Result<harn_vm::secrets::SecretBytes, harn_vm::secrets::SecretError> {
        let value = self
            .values
            .get(&id.to_string())
            .cloned()
            .unwrap_or_else(|| "contract-fixture-secret".to_string());
        Ok(harn_vm::secrets::SecretBytes::from(value))
    }

    async fn put(
        &self,
        _id: &harn_vm::secrets::SecretId,
        _value: harn_vm::secrets::SecretBytes,
    ) -> Result<(), harn_vm::secrets::SecretError> {
        Ok(())
    }

    async fn rotate(
        &self,
        id: &harn_vm::secrets::SecretId,
    ) -> Result<harn_vm::secrets::RotationHandle, harn_vm::secrets::SecretError> {
        Ok(harn_vm::secrets::RotationHandle {
            provider: self.namespace().to_string(),
            id: id.clone(),
            from_version: None,
            to_version: None,
        })
    }

    async fn list(
        &self,
        _prefix: &harn_vm::secrets::SecretId,
    ) -> Result<Vec<harn_vm::secrets::SecretMeta>, harn_vm::secrets::SecretError> {
        Ok(Vec::new())
    }

    fn namespace(&self) -> &'static str {
        "connector-contract"
    }

    fn supports_versions(&self) -> bool {
        false
    }
}

fn raw_from_fixture(fixture: &ConnectorContractFixture) -> Result<harn_vm::RawInbound, String> {
    if fixture.body.is_some() && fixture.body_json.is_some() {
        return Err(format!(
            "fixture '{}' sets both body and body_json",
            fixture_name(fixture)
        ));
    }
    let body = match (&fixture.body, &fixture.body_json) {
        (Some(body), None) => body.as_bytes().to_vec(),
        (None, Some(value)) => serde_json::to_vec(&toml_to_json(value)?)
            .map_err(|error| format!("failed to serialize fixture body_json: {error}"))?,
        (None, None) => b"{}".to_vec(),
        (Some(_), Some(_)) => unreachable!("checked above"),
    };
    let mut raw = harn_vm::RawInbound::new(
        fixture
            .kind
            .clone()
            .unwrap_or_else(|| "webhook".to_string()),
        fixture.headers.clone(),
        body,
    );
    raw.query = fixture.query.clone();
    raw.received_at = OffsetDateTime::parse("2026-04-22T12:00:00Z", &Rfc3339)
        .map_err(|error| error.to_string())?;
    raw.metadata = match &fixture.metadata {
        Some(value) => toml_to_json(value)?,
        None => json!({
            "binding_id": format!("contract-{}-fixture", fixture.provider.as_str()),
            "binding_version": 1,
            "path": "/harn/connector-contract",
        }),
    };
    Ok(raw)
}

fn toml_to_json(value: &toml::Value) -> Result<JsonValue, String> {
    serde_json::to_value(value).map_err(|error| format!("failed to convert TOML fixture: {error}"))
}

fn validate_normalize_result(
    fixture: &ConnectorContractFixture,
    result: &harn_vm::ConnectorNormalizeResult,
) -> Result<CheckedFixture, String> {
    let (result_type, event_count) = match result {
        harn_vm::ConnectorNormalizeResult::Event(event) => {
            if let Some(expected_kind) = fixture.expect_kind.as_deref() {
                if event.kind != expected_kind {
                    return Err(format!(
                        "fixture '{}' expected event kind '{}' but got '{}'",
                        fixture_name(fixture),
                        expected_kind,
                        event.kind
                    ));
                }
            }
            validate_event_expectations(fixture, event.as_ref())?;
            ("event", 1)
        }
        harn_vm::ConnectorNormalizeResult::Batch(events) => {
            if let Some(expected_kind) = fixture.expect_kind.as_deref() {
                if let Some(event) = events.iter().find(|event| event.kind != expected_kind) {
                    return Err(format!(
                        "fixture '{}' expected all event kinds '{}' but got '{}'",
                        fixture_name(fixture),
                        expected_kind,
                        event.kind
                    ));
                }
            }
            for event in events {
                validate_event_expectations(fixture, event)?;
            }
            ("batch", events.len())
        }
        harn_vm::ConnectorNormalizeResult::ImmediateResponse { response, events } => {
            validate_response_expectations(fixture, "immediate_response", response)?;
            if let Some(expected_kind) = fixture.expect_kind.as_deref() {
                if let Some(event) = events.iter().find(|event| event.kind != expected_kind) {
                    return Err(format!(
                        "fixture '{}' expected all event kinds '{}' but got '{}'",
                        fixture_name(fixture),
                        expected_kind,
                        event.kind
                    ));
                }
            }
            for event in events {
                validate_event_expectations(fixture, event)?;
            }
            ("immediate_response", events.len())
        }
        harn_vm::ConnectorNormalizeResult::Reject(response) => {
            validate_response_expectations(fixture, "reject", response)?;
            ("reject", 0)
        }
    };

    if let Some(expected_type) = fixture.expect_type.as_deref() {
        if result_type != expected_type {
            return Err(format!(
                "fixture '{}' expected NormalizeResult type '{}' but got '{}'",
                fixture_name(fixture),
                expected_type,
                result_type
            ));
        }
    }
    if let Some(expected_event_count) = fixture.expect_event_count {
        if event_count != expected_event_count {
            return Err(format!(
                "fixture '{}' expected {} normalized event(s) but got {}",
                fixture_name(fixture),
                expected_event_count,
                event_count
            ));
        }
    }

    Ok(CheckedFixture {
        name: fixture_name(fixture),
        result_type: result_type.to_string(),
        event_count,
    })
}

fn validate_event_expectations(
    fixture: &ConnectorContractFixture,
    event: &harn_vm::TriggerEvent,
) -> Result<(), String> {
    if let Some(expected_dedupe_key) = fixture.expect_dedupe_key.as_deref() {
        if event.dedupe_key != expected_dedupe_key {
            return Err(format!(
                "fixture '{}' expected dedupe_key '{}' but got '{}'",
                fixture_name(fixture),
                expected_dedupe_key,
                event.dedupe_key
            ));
        }
    }
    if let Some(expected_signature_state) = fixture.expect_signature_state.as_deref() {
        let signature_state = match &event.signature_status {
            harn_vm::SignatureStatus::Verified => "verified",
            harn_vm::SignatureStatus::Unsigned => "unsigned",
            harn_vm::SignatureStatus::Failed { .. } => "failed",
        };
        if signature_state != expected_signature_state {
            return Err(format!(
                "fixture '{}' expected signature state '{}' but got '{}'",
                fixture_name(fixture),
                expected_signature_state,
                signature_state
            ));
        }
    }
    if let Some(expected_payload) = &fixture.expect_payload_contains {
        let expected = toml_to_json(expected_payload)?;
        let actual = serde_json::to_value(&event.provider_payload).map_err(|error| {
            format!(
                "fixture '{}' failed to serialize provider payload: {error}",
                fixture_name(fixture)
            )
        })?;
        let envelope_result = assert_json_contains(fixture, "provider_payload", &actual, &expected);
        if let Err(envelope_error) = envelope_result {
            // Connector fixtures own and describe their package payload schema,
            // while TriggerEvent wraps that value with runtime schema metadata.
            // Accept assertions against either surface so package contracts do
            // not need to encode the host envelope.
            if let harn_vm::ProviderPayload::Extension(extension) = &event.provider_payload {
                if assert_json_contains(fixture, "provider_payload.raw", &extension.raw, &expected)
                    .is_ok()
                {
                    return Ok(());
                }
            }
            return Err(envelope_error);
        }
    }
    Ok(())
}

fn validate_response_expectations(
    fixture: &ConnectorContractFixture,
    result_type: &str,
    response: &harn_vm::ConnectorHttpResponse,
) -> Result<(), String> {
    if let Some(expected_status) = fixture.expect_response_status {
        if response.status != expected_status {
            return Err(format!(
                "fixture '{}' expected {result_type} HTTP status {} but got {}",
                fixture_name(fixture),
                expected_status,
                response.status
            ));
        }
    }
    if let Some(expected_body) = &fixture.expect_response_body {
        let expected = toml_to_json(expected_body)?;
        if response.body != expected {
            return Err(format!(
                "fixture '{}' expected {result_type} body {} but got {}",
                fixture_name(fixture),
                expected,
                response.body
            ));
        }
    }
    Ok(())
}

fn assert_json_contains(
    fixture: &ConnectorContractFixture,
    path: &str,
    actual: &JsonValue,
    expected: &JsonValue,
) -> Result<(), String> {
    match expected {
        JsonValue::Object(expected_map) => {
            let actual_map = actual.as_object().ok_or_else(|| {
                format!(
                    "fixture '{}' expected {path} to be an object containing {} but got {}",
                    fixture_name(fixture),
                    expected,
                    actual
                )
            })?;
            for (key, expected_value) in expected_map {
                let actual_value = actual_map.get(key).ok_or_else(|| {
                    format!(
                        "fixture '{}' expected {path}.{key} to exist in {}",
                        fixture_name(fixture),
                        actual
                    )
                })?;
                assert_json_contains(
                    fixture,
                    &format!("{path}.{key}"),
                    actual_value,
                    expected_value,
                )?;
            }
            Ok(())
        }
        _ if actual == expected => Ok(()),
        _ => Err(format!(
            "fixture '{}' expected {path} to contain {} but got {}",
            fixture_name(fixture),
            expected,
            actual
        )),
    }
}

fn fixture_name(fixture: &ConnectorContractFixture) -> String {
    fixture
        .name
        .clone()
        .unwrap_or_else(|| format!("{} fixture", fixture.provider.as_str()))
}
