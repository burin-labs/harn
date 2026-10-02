//! One execution-result boundary shared by every Harn tool adapter.

use serde_json::Value as JsonValue;

use super::handler_result::parse_handler_result;
use super::{
    PreparedToolCatalog, ToolApplicationError, ToolContractPhase, ToolContractViolation,
    ToolContractViolationDetail, ToolThrownClassification, HARN_MCP_TOOL_CONTRACT_META_KEY,
};
use crate::value::{VmError, VmValue};

/// Convert a handler result to portable JSON without stringifying unsupported
/// runtime-only values such as closures or capability handles.
pub fn result_to_json(value: &VmValue) -> Result<JsonValue, String> {
    crate::llm::helpers::vm_value_to_export_json_strict(value, "result")
}

/// Prepare a direct agent dispatch without compiling unrelated tool entries.
/// Lifecycle-owned registries retain their full prepared catalog instead.
pub(crate) fn tool_registry_catalog_for_tool(
    registry: &VmValue,
    name: &str,
) -> Result<super::ToolCatalog, VmError> {
    // Agent primitives also accept legacy `{tools: [...]}` wrappers.
    let registry = registry
        .as_dict()
        .ok_or_else(|| VmError::Runtime("expected a tool registry".into()))?;
    let entry = super::registry_entries(registry)?
        .iter()
        .find(|entry| {
            entry.as_dict().is_some_and(|entry| {
                matches!(entry.get("name"), Some(VmValue::String(actual)) if actual.as_str() == name)
            })
        })
        .ok_or_else(|| VmError::Runtime(format!("tool {name:?} is not registered")))?;
    Ok(super::ToolCatalog {
        schema_version: super::ToolCatalogSchemaVersion::V2,
        info: None,
        cli: None,
        tools: vec![super::catalog_entry(entry)?],
        components: super::registry_components(registry)?,
    })
}

/// A portable handler result after its declared contract has accepted it.
#[derive(Debug)]
pub enum ToolInvocationOutcome {
    Success {
        value: VmValue,
        json: JsonValue,
        text: Option<String>,
    },
    ApplicationError(ToolApplicationError),
}

/// A handler failure that is not declared application data.
#[derive(Debug)]
pub enum ToolInvocationError {
    Runtime(VmError),
    Contract(ToolContractViolation),
}

/// Closed VM failure classification shared by adapters with custom success
/// projection, such as printed-output pipelines.
#[derive(Debug)]
pub enum ToolFailureClassification {
    Application(ToolApplicationError),
    Runtime(VmError),
    Contract(ToolContractViolation),
}

/// Stable generated-CLI JSON failure envelope.
pub fn application_error_cli_envelope(error: &ToolApplicationError) -> JsonValue {
    let mut payload = error.to_json();
    payload
        .as_object_mut()
        .expect("application error payload is an object")
        .insert(
            "kind".to_string(),
            JsonValue::String("application".to_string()),
        );
    serde_json::json!({
        "ok": false,
        "error": payload,
    })
}

/// Stable MCP `CallToolResult` for a declared application failure.
pub fn application_error_mcp_result(error: &ToolApplicationError) -> JsonValue {
    let mut result = serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "tool {:?} failed: {}", error.tool, error.summary()
        )}],
        "isError": true,
        "_meta": {},
    });
    result["_meta"][HARN_MCP_TOOL_CONTRACT_META_KEY] = serde_json::json!({
        "applicationError": error.to_json(),
    });
    result
}

impl std::fmt::Display for ToolInvocationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Runtime(error) => formatter.write_str(&tool_runtime_error_summary(error)),
            Self::Contract(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ToolInvocationError {}

/// Value-free human summary for a runtime failure crossing a tool adapter.
///
/// A raw `throw` has no declared portable contract, so its value must remain
/// inside the owned [`VmError`] instead of entering logs, stderr, status text,
/// or MCP content. Typed application data takes the separate
/// [`ToolApplicationError`] path.
pub fn tool_runtime_error_summary(error: &VmError) -> String {
    match crate::value::error_to_category(error) {
        // Keep the stable classifier token so A2A can retain its resumable
        // auth-required state without recovering the original thrown value.
        crate::value::ErrorCategory::Auth => "tool authentication_error".to_string(),
        crate::value::ErrorCategory::BudgetExceeded => "tool execution budget exceeded".to_string(),
        crate::value::ErrorCategory::Cancelled => "tool execution cancelled".to_string(),
        crate::value::ErrorCategory::RateLimit => "tool execution was rate limited".to_string(),
        _ if matches!(error, VmError::Thrown(_)) => "tool threw an undeclared value".to_string(),
        _ => error.to_string(),
    }
}

/// Classify and validate a raw VM handler result exactly once.
///
/// Explicit typed failures carry portable data and their declared disposition.
/// Declared error schemas constrain that data when present. Raw throws still
/// require a matching error schema; control and host failures remain runtime.
pub fn classify_tool_result(
    prepared: &PreparedToolCatalog,
    tool: &str,
    result: Result<VmValue, VmError>,
) -> Result<ToolInvocationOutcome, ToolInvocationError> {
    match result {
        Ok(value) => {
            let (value, text) = if let Some(result) =
                parse_handler_result(&value).map_err(ToolInvocationError::Runtime)?
            {
                if let Some(outcome) = result.outcome.application_outcome() {
                    let json =
                        portable_value(tool, ToolContractPhase::ApplicationError, result.data)?;
                    return prepared
                        .declared_failure(tool, &json, outcome)
                        .map(ToolInvocationOutcome::ApplicationError)
                        .map_err(ToolInvocationError::Contract);
                }
                (result.data.clone(), Some(result.text.to_owned()))
            } else {
                (value, None)
            };
            let json = portable_value(tool, ToolContractPhase::Output, &value)?;
            prepared
                .validate_output(tool, &json)
                .map_err(ToolInvocationError::Contract)?;
            Ok(ToolInvocationOutcome::Success { value, json, text })
        }
        Err(error) => match classify_tool_failure(prepared, tool, error) {
            ToolFailureClassification::Application(error) => {
                Ok(ToolInvocationOutcome::ApplicationError(error))
            }
            ToolFailureClassification::Runtime(error) => Err(ToolInvocationError::Runtime(error)),
            ToolFailureClassification::Contract(error) => Err(ToolInvocationError::Contract(error)),
        },
    }
}

pub fn classify_tool_failure(
    prepared: &PreparedToolCatalog,
    tool: &str,
    error: VmError,
) -> ToolFailureClassification {
    if is_reserved_control_error(&error) {
        return ToolFailureClassification::Runtime(error);
    }
    let VmError::Thrown(value) = error else {
        return ToolFailureClassification::Runtime(error);
    };
    if prepared
        .entry(tool)
        .is_none_or(|entry| entry.error_schema.is_none())
    {
        return ToolFailureClassification::Runtime(VmError::Thrown(value));
    }
    let json = match portable_value(tool, ToolContractPhase::ApplicationError, &value) {
        Ok(json) => json,
        Err(ToolInvocationError::Contract(error)) => {
            return ToolFailureClassification::Contract(error)
        }
        Err(_) => unreachable!("portable_value only returns contract failures"),
    };
    match prepared.classify_thrown_json(tool, &json) {
        ToolThrownClassification::Application(error) => {
            ToolFailureClassification::Application(error)
        }
        ToolThrownClassification::Undeclared => {
            ToolFailureClassification::Runtime(VmError::Thrown(value))
        }
        ToolThrownClassification::ContractViolation(error) => {
            ToolFailureClassification::Contract(error)
        }
    }
}

fn is_reserved_control_error(error: &VmError) -> bool {
    match error {
        VmError::AbandonedExecution => true,
        VmError::CategorizedError { category, .. } => matches!(
            category,
            crate::value::ErrorCategory::BudgetExceeded | crate::value::ErrorCategory::Cancelled
        ),
        VmError::Thrown(VmValue::String(message)) => message.starts_with("kind:cancelled:"),
        VmError::Thrown(VmValue::Dict(fields)) => {
            let string = |key: &str| match fields.get(key) {
                Some(VmValue::String(value)) => Some(value.as_str()),
                _ => None,
            };
            match string("category") {
                Some("budget_exceeded") => matches!(
                    (string("kind"), string("reason")),
                    (Some("terminal"), Some("budget_exceeded"))
                        | (Some("budget_exhausted"), Some("step_budget_exhausted"))
                        | (
                            Some("budget_exhausted"),
                            Some("nested_execution_budget_exhausted")
                        )
                ),
                Some("cancelled") => matches!(
                    string("name"),
                    Some("WaitpointCancelledError") | Some("HumanCancelledError")
                ),
                _ => false,
            }
        }
        _ => false,
    }
}

fn portable_value(
    tool: &str,
    phase: ToolContractPhase,
    value: &VmValue,
) -> Result<JsonValue, ToolInvocationError> {
    result_to_json(value).map_err(|_| {
        ToolInvocationError::Contract(ToolContractViolation {
            tool: tool.to_string(),
            phase,
            violations: vec![ToolContractViolationDetail {
                structural_path: String::new(),
                schema_path: String::new(),
                keyword: "portableJson".to_string(),
                missing_property: None,
            }],
        })
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::tool_registry::{
        ToolCatalog, ToolCatalogEntry, ToolCatalogSchemaVersion, ToolCliSpec, ToolGovernance,
    };
    use crate::value::ErrorCategory;

    fn prepared(error_schema: Option<JsonValue>) -> PreparedToolCatalog {
        PreparedToolCatalog::prepare(ToolCatalog {
            schema_version: ToolCatalogSchemaVersion::V2,
            info: None,
            cli: None,
            tools: vec![ToolCatalogEntry {
                name: "widgets.create".to_string(),
                title: None,
                description: None,
                input_schema: json!({"type": "object"}),
                output_schema: Some(json!({"type": "integer"})),
                error_schema,
                annotations: None,
                icons: None,
                execution: None,
                governance: ToolGovernance::default(),
                cli: ToolCliSpec {
                    command: vec!["widgets".to_string(), "create".to_string()],
                    aliases: Vec::new(),
                    hidden: false,
                    arguments: BTreeMap::new(),
                },
                namespace: None,
                defer_loading: false,
                source: None,
                policy: None,
                meta: None,
            }],
            components: None,
        })
        .expect("prepare catalog")
    }

    #[test]
    fn declared_throw_is_application_data_and_wrong_shape_is_a_contract_failure() {
        let prepared = prepared(Some(json!({
            "type": "object",
            "properties": {"code": {"const": "conflict"}},
            "required": ["code"],
            "additionalProperties": false
        })));
        let value = crate::schema::json_to_vm_value(&json!({"code": "conflict"}));
        let outcome =
            classify_tool_result(&prepared, "widgets.create", Err(VmError::Thrown(value)))
                .expect("declared application error");
        assert!(matches!(
            outcome,
            ToolInvocationOutcome::ApplicationError(ToolApplicationError { data, .. })
                if data == json!({"code": "conflict"})
        ));

        let invalid = crate::schema::json_to_vm_value(&json!({"code": "missing"}));
        let error =
            classify_tool_result(&prepared, "widgets.create", Err(VmError::Thrown(invalid)))
                .expect_err("wrong thrown shape must fail closed");
        assert!(matches!(
            error,
            ToolInvocationError::Contract(ToolContractViolation {
                phase: ToolContractPhase::ApplicationError,
                ..
            })
        ));
    }

    #[test]
    fn undeclared_throw_remains_a_runtime_failure() {
        let error = classify_tool_result(
            &prepared(None),
            "widgets.create",
            Err(VmError::Thrown(VmValue::String("conflict".into()))),
        )
        .expect_err("undeclared throw is not typed application data");
        assert!(matches!(
            error,
            ToolInvocationError::Runtime(VmError::Thrown(_))
        ));
    }

    #[test]
    fn explicit_success_projects_only_declared_data_and_preserves_feedback() {
        let result = crate::schema::json_to_vm_value(&json!({
            "schema": super::super::handler_result::AGENT_TOOL_HANDLER_RESULT_SCHEMA,
            "text": "Created widget",
            "data": 7,
            "outcome": "ok"
        }));
        let outcome = classify_tool_result(&prepared(None), "widgets.create", Ok(result))
            .expect("the integer payload satisfies the output contract");
        assert!(
            matches!(outcome, ToolInvocationOutcome::Success { value, json, text }
            if matches!(value, VmValue::Int(7)) && json == json!(7)
                && text.as_deref() == Some("Created widget"))
        );

        let invalid = crate::schema::json_to_vm_value(&json!({
            "schema": super::super::handler_result::AGENT_TOOL_HANDLER_RESULT_SCHEMA,
            "text": "Reached handler",
            "data": {"wrong": true},
            "outcome": "ok"
        }));
        assert!(matches!(
            classify_tool_result(&prepared(None), "widgets.create", Ok(invalid)),
            Err(ToolInvocationError::Contract(ToolContractViolation {
                phase: ToolContractPhase::Output,
                ..
            }))
        ));
    }

    #[test]
    fn explicit_failures_and_malformed_outcomes_cannot_be_successful_data() {
        for outcome in ["error", "rejected", "maybe"] {
            let result = crate::schema::json_to_vm_value(&json!({
                "schema": super::super::handler_result::AGENT_TOOL_HANDLER_RESULT_SCHEMA,
                "text": "Canonical feedback",
                "data": 7,
                "outcome": outcome
            }));
            let classified = classify_tool_result(&prepared(None), "widgets.create", Ok(result));
            if outcome == "maybe" {
                assert!(matches!(
                    classified,
                    Err(ToolInvocationError::Runtime(VmError::CategorizedError {
                        category: ErrorCategory::SchemaValidation,
                        ..
                    }))
                ));
            } else {
                let ToolInvocationOutcome::ApplicationError(error) = classified.unwrap() else {
                    panic!("explicit failure must not succeed");
                };
                assert_eq!(error.data, json!(7));
                assert_eq!(error.to_json()["outcome"], outcome);
            }
        }
        let invalid = crate::schema::json_to_vm_value(&json!({
            "schema": super::super::handler_result::AGENT_TOOL_HANDLER_RESULT_SCHEMA,
            "text": "Declared failure", "data": "wrong", "outcome": "error"
        }));
        assert!(matches!(
            classify_tool_result(
                &prepared(Some(json!({"type": "integer"}))),
                "widgets.create",
                Ok(invalid)
            ),
            Err(ToolInvocationError::Contract(ToolContractViolation {
                phase: ToolContractPhase::ApplicationError,
                ..
            }))
        ));
    }

    #[test]
    fn ordinary_domain_values_do_not_inherit_agent_outcome_policy() {
        let mut catalog = prepared(None).catalog().clone();
        catalog.tools[0].output_schema = Some(json!({
            "type": "object", "properties": {
                "ok": {"const": false}, "success": {"const": false},
                "status": {"const": "error"}
            }, "required": ["ok", "success", "status"], "additionalProperties": false
        }));
        let prepared = PreparedToolCatalog::prepare(catalog).expect("prepare domain contract");
        let json = json!({"ok": false, "success": false, "status": "error"});
        let raw = crate::schema::json_to_vm_value(&json);
        let nominal =
            VmValue::struct_instance("DomainPayload", raw.as_dict().unwrap().as_ref().clone());
        for value in [raw, nominal] {
            let outcome = classify_tool_result(&prepared, "widgets.create", Ok(value))
                .expect("ordinary API payload is successful domain data");
            assert!(
                matches!(outcome, ToolInvocationOutcome::Success { json: actual, text: None, .. }
                if actual == json)
            );
        }
    }

    #[test]
    fn undeclared_throw_summary_never_reads_application_data() {
        let error = ToolInvocationError::Runtime(VmError::Thrown(crate::schema::json_to_vm_value(
            &json!({
                "variant": "LegacyFailure",
                "message": "PRIVATE-CUSTOMER-DIAGNOSTIC-123456",
            }),
        )));
        let summary = error.to_string();
        assert_eq!(summary, "tool threw an undeclared value");
        assert!(!summary.contains("PRIVATE-CUSTOMER-DIAGNOSTIC"));
    }

    #[test]
    fn sensitive_categorized_runtime_summaries_never_read_their_messages() {
        let cases = [
            (ErrorCategory::Auth, "tool authentication_error"),
            (
                ErrorCategory::BudgetExceeded,
                "tool execution budget exceeded",
            ),
            (ErrorCategory::Cancelled, "tool execution cancelled"),
            (ErrorCategory::RateLimit, "tool execution was rate limited"),
        ];
        for (category, expected) in cases {
            let error = VmError::CategorizedError {
                message: "PRIVATE-CUSTOMER-DIAGNOSTIC-123456".to_string(),
                category,
            };
            let summary = tool_runtime_error_summary(&error);
            assert_eq!(summary, expected);
            assert!(!summary.contains("PRIVATE-CUSTOMER-DIAGNOSTIC"));
        }
    }

    #[test]
    fn control_throw_cannot_be_blessed_by_a_broad_application_schema() {
        let control = crate::schema::json_to_vm_value(&json!({
            "category": "budget_exceeded",
            "kind": "terminal",
            "reason": "budget_exceeded",
            "limit": "mcp_calls",
            "limit_value": 1,
            "spent": 2,
            "message": "budget exhausted"
        }));
        let error = classify_tool_result(
            &prepared(Some(json!({"type": "object"}))),
            "widgets.create",
            Err(VmError::Thrown(control)),
        )
        .expect_err("control-plane budget stop remains runtime failure");
        assert!(matches!(
            error,
            ToolInvocationError::Runtime(VmError::Thrown(_))
        ));

        let cancellation =
            crate::cancellation::cancelled_error(crate::cancellation::HandlerDispatch::Dispatched);
        let error = classify_tool_result(
            &prepared(Some(json!({"type": "string"}))),
            "widgets.create",
            Err(cancellation),
        )
        .expect_err("host cancellation remains runtime control flow");
        assert!(matches!(
            &error,
            ToolInvocationError::Runtime(inner) if crate::cancellation::is_cancellation(inner)
        ));

        let business = VmError::Thrown(VmValue::String("customer cancelled order".into()));
        let outcome = classify_tool_result(
            &prepared(Some(json!({"type": "string"}))),
            "widgets.create",
            Err(business),
        )
        .expect("ordinary declared business errors are not control flow");
        assert!(matches!(
            outcome,
            ToolInvocationOutcome::ApplicationError(ToolApplicationError { data, .. })
                if data == json!("customer cancelled order")
        ));

        let business = crate::schema::json_to_vm_value(&json!({
            "category": "budget_exceeded",
            "variant": "CustomerLimit"
        }));
        let outcome = classify_tool_result(
            &prepared(Some(json!({"type": "object"}))),
            "widgets.create",
            Err(VmError::Thrown(business)),
        )
        .expect(
            "a category-shaped business error without the control sentinel is application data",
        );
        assert!(matches!(
            outcome,
            ToolInvocationOutcome::ApplicationError(_)
        ));

        let nested_control = crate::schema::json_to_vm_value(&json!({
            "category": "budget_exceeded",
            "kind": "budget_exhausted",
            "reason": "nested_execution_budget_exhausted",
            "message": "nested execution budget exhausted before sub_agent: customer label"
        }));
        let error = classify_tool_result(
            &prepared(Some(json!({"type": "object"}))),
            "widgets.create",
            Err(VmError::Thrown(nested_control)),
        )
        .expect_err("nested budget control sentinels remain runtime failures");
        assert!(matches!(error, ToolInvocationError::Runtime(_)));
    }
}
