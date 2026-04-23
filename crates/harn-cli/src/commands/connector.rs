use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value as JsonValue};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::cli::{ConnectorArgs, ConnectorCheckArgs, ConnectorCommand};
use crate::package::{self, ConnectorContractFixture, ResolvedProviderConnectorKind};

pub(crate) async fn handle_connector_command(args: ConnectorArgs) -> Result<(), String> {
    match args.command {
        ConnectorCommand::Check(check) => {
            let report = check_connector_package(&check).await?;
            if check.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .map_err(|error| format!("failed to render connector report: {error}"))?
                );
            } else {
                print_human_report(&report);
            }
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ConnectorCheckReport {
    pub package: String,
    pub checked_connectors: Vec<CheckedConnector>,
    pub fixture_count: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CheckedConnector {
    pub provider: String,
    pub module: String,
    pub kinds: Vec<String>,
    pub payload_schema: String,
    pub has_poll_tick: bool,
    pub fixtures: Vec<CheckedFixture>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CheckedFixture {
    pub name: String,
    pub result_type: String,
    pub event_count: usize,
}

pub(crate) async fn check_connector_package(
    args: &ConnectorCheckArgs,
) -> Result<ConnectorCheckReport, String> {
    let _provider_schema_guard = package::lock_manifest_provider_schemas().await;
    let package = PathBuf::from(&args.package);
    let anchor = normalize_anchor(&package);
    let extensions = package::try_load_runtime_extensions(&anchor)?;
    package::install_manifest_provider_schemas(&extensions).await?;
    let manifest = extensions
        .root_manifest
        .as_ref()
        .ok_or_else(|| format!("no harn.toml found for {}", anchor.display()))?;
    let fixture_version = manifest.connector_contract.version.unwrap_or(1);
    if fixture_version != 1 {
        return Err(format!(
            "unsupported connector_contract.version {fixture_version}; expected 1"
        ));
    }

    let provider_filter = args.providers.iter().cloned().collect::<BTreeSet<_>>();
    let mut checked_connectors = Vec::new();
    let mut warnings = Vec::new();
    let mut failures = Vec::new();
    let mut fixture_count = 0usize;

    for provider in &extensions.provider_connectors {
        if !provider_filter.is_empty() && !provider_filter.contains(provider.id.as_str()) {
            continue;
        }

        let ResolvedProviderConnectorKind::Harn { module } = &provider.connector else {
            if matches!(
                provider.connector,
                ResolvedProviderConnectorKind::RustBuiltin
            ) {
                warnings.push(format!(
                    "skipped provider '{}' because it uses the Rust builtin connector",
                    provider.id.as_str()
                ));
            } else if let ResolvedProviderConnectorKind::Invalid(message) = &provider.connector {
                failures.push(message.clone());
            }
            continue;
        };

        match check_one_connector(
            provider.id.clone(),
            &provider.manifest_dir,
            module,
            &manifest.connector_contract.fixtures,
            args.run_poll_tick,
        )
        .await
        {
            Ok(checked) => {
                fixture_count += checked.fixtures.len();
                checked_connectors.push(checked);
            }
            Err(error) => failures.push(error),
        }
    }

    if !provider_filter.is_empty() {
        for provider in &provider_filter {
            if !extensions
                .provider_connectors
                .iter()
                .any(|config| config.id.as_str() == provider)
            {
                failures.push(format!(
                    "provider '{provider}' is not declared in harn.toml"
                ));
            }
        }
    }

    if checked_connectors.is_empty() && failures.is_empty() {
        failures.push(format!(
            "no pure-Harn connector providers found in {}",
            anchor.display()
        ));
    }
    if fixture_count == 0 {
        warnings.push("no connector_contract fixtures were declared; normalize_inbound shape was not exercised".to_string());
    }

    if failures.is_empty() {
        Ok(ConnectorCheckReport {
            package: anchor.display().to_string(),
            checked_connectors,
            fixture_count,
            warnings,
        })
    } else {
        Err(format!(
            "connector contract check failed:\n{}",
            failures
                .into_iter()
                .map(|failure| format!("- {failure}"))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }
}

async fn check_one_connector(
    provider_id: harn_vm::ProviderId,
    manifest_dir: &Path,
    module: &str,
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

    fn namespace(&self) -> &str {
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
            ("batch", events.len())
        }
        harn_vm::ConnectorNormalizeResult::ImmediateResponse { events, .. } => {
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
            ("immediate_response", events.len())
        }
        harn_vm::ConnectorNormalizeResult::Reject(_) => ("reject", 0),
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

fn fixture_name(fixture: &ConnectorContractFixture) -> String {
    fixture
        .name
        .clone()
        .unwrap_or_else(|| format!("{} fixture", fixture.provider.as_str()))
}

fn normalize_anchor(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join("harn.toml")
    } else {
        path.to_path_buf()
    }
}

fn print_human_report(report: &ConnectorCheckReport) {
    println!(
        "Connector contract check passed for {} connector(s), {} fixture(s).",
        report.checked_connectors.len(),
        report.fixture_count
    );
    for connector in &report.checked_connectors {
        println!(
            "- {}: kinds=[{}], schema={}, fixtures={}",
            connector.provider,
            connector.kinds.join(", "),
            connector.payload_schema,
            connector.fixtures.len()
        );
    }
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::sync::OnceLock;

    async fn connector_check_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await
    }

    fn write_package(manifest_tail: &str, lib: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("harn.toml"),
            format!(
                r#"
[package]
name = "contract-test"
version = "0.1.0"

[[providers]]
id = "echo"
connector = {{ harn = "./lib.harn" }}
{manifest_tail}
"#
            ),
        )
        .unwrap();
        fs::write(dir.path().join("lib.harn"), lib).unwrap();
        dir
    }

    fn check_args(path: &Path) -> ConnectorCheckArgs {
        ConnectorCheckArgs {
            package: path.display().to_string(),
            providers: Vec::new(),
            run_poll_tick: false,
            json: false,
        }
    }

    #[tokio::test]
    async fn connector_check_accepts_valid_fixture_package() {
        let _guard = connector_check_test_guard().await;
        let dir = write_package(
            r#"
[connector_contract]
version = 1

[[connector_contract.fixtures]]
provider = "echo"
name = "echo event"
kind = "webhook"
body_json = { id = "evt-1", message = "hello" }
expect_type = "event"
expect_kind = "echo.received"
expect_event_count = 1
"#,
            r#"
var active_bindings = []

pub fn provider_id() {
  return "echo"
}

pub fn kinds() {
  return ["webhook"]
}

pub fn payload_schema() {
  return {
    harn_schema_name: "EchoEventPayload",
    json_schema: {
      type: "object",
      additionalProperties: true,
    },
  }
}

pub fn init(ctx) {
  if ctx.capabilities.secret_get != true {
    throw "secret_get capability missing"
  }
}

pub fn activate(bindings) {
  active_bindings = bindings
  metrics_inc("echo_activate_bindings", len(bindings))
}

pub fn shutdown() {
  metrics_inc("echo_shutdown")
}

pub fn normalize_inbound(raw) {
  let body = raw.body_json ?? json_parse(raw.body_text)
  let token = secret_get("echo/api-token")
  event_log_emit("connectors.echo.contract", "normalize", {
    token: token,
  })
  return {
    type: "event",
    event: {
      kind: "echo.received",
      dedupe_key: "echo:" + body.id,
      payload: body,
    },
  }
}

pub fn call(method, _args) {
  throw "method_not_found:" + method
}
"#,
        );
        let report = check_connector_package(&check_args(dir.path()))
            .await
            .expect("valid package should pass");
        assert_eq!(report.checked_connectors.len(), 1);
        assert_eq!(report.fixture_count, 1);
        assert_eq!(
            report.checked_connectors[0].payload_schema,
            "EchoEventPayload"
        );
    }

    #[tokio::test]
    async fn connector_check_rejects_payload_schema_name_mismatch() {
        let _guard = connector_check_test_guard().await;
        let dir = write_package(
            "",
            r#"
pub fn provider_id() { return "echo" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() {
  return {
    name: "EchoEventPayload",
    json_schema: {type: "object"},
  }
}
pub fn normalize_inbound(_raw) {
  return {type: "reject", status: 400}
}
"#,
        );
        let error = check_connector_package(&check_args(dir.path()))
            .await
            .unwrap_err();
        assert!(error.contains("payload_schema() must return { harn_schema_name, json_schema? }"));
    }

    #[tokio::test]
    async fn connector_check_rejects_legacy_immediate_response_wrapper() {
        let _guard = connector_check_test_guard().await;
        let dir = write_package(
            r#"
[[connector_contract.fixtures]]
provider = "echo"
body_json = { id = "evt-1" }
"#,
            r#"
pub fn provider_id() { return "echo" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "EchoEventPayload" }
pub fn normalize_inbound(_raw) {
  return {
    immediate_response: {status: 200, body: "ok"},
    event: {
      kind: "echo.received",
      dedupe_key: "echo:evt-1",
      payload: {id: "evt-1"},
    },
  }
}
"#,
        );
        let error = check_connector_package(&check_args(dir.path()))
            .await
            .unwrap_err();
        assert!(error.contains("normalize_inbound fixture"));
    }

    #[tokio::test]
    async fn connector_check_reports_static_effect_policy_violations() {
        let _guard = connector_check_test_guard().await;
        let dir = write_package(
            "",
            r#"
pub fn provider_id() { return "echo" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "EchoEventPayload" }
pub fn normalize_inbound(_raw) {
  http_get("https://example.invalid")
  return {type: "reject", status: 400}
}
"#,
        );
        let error = check_connector_package(&check_args(dir.path()))
            .await
            .unwrap_err();
        assert!(error.contains("connector-effect-policy"), "{error}");
        assert!(error.contains("http_get"), "{error}");
    }

    #[tokio::test]
    async fn connector_check_can_assert_runtime_policy_denial_fixture() {
        let _guard = connector_check_test_guard().await;
        let dir = write_package(
            r#"
[connector_contract]
version = 1

[[connector_contract.fixtures]]
provider = "echo"
name = "indirect file read denied"
body_json = { id = "evt-1" }
expect_error_contains = "violated effect policy"
"#,
            r#"
pub fn provider_id() { return "echo" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "EchoEventPayload" }

fn read_indirect() {
  return read_file("ambient.txt")
}

pub fn normalize_inbound(raw) {
  let _body = raw.body_json
  read_indirect()
  return {type: "reject", status: 400}
}
"#,
        );
        let report = check_connector_package(&check_args(dir.path()))
            .await
            .expect("expected-error fixture should pass");
        assert_eq!(report.fixture_count, 1);
        assert_eq!(
            report.checked_connectors[0].fixtures[0].result_type,
            "error"
        );
    }
}
