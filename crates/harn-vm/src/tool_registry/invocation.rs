//! One execution-result boundary shared by every Harn tool adapter.

use serde_json::Value as JsonValue;

use super::{
    result_to_json, PreparedToolCatalog, ToolApplicationError, ToolContractPhase,
    ToolContractViolation, ToolContractViolationDetail, ToolThrownClassification,
    HARN_MCP_TOOL_CONTRACT_META_KEY,
};
use crate::value::{VmError, VmValue};

/// A portable handler result after its declared contract has accepted it.
#[derive(Debug)]
pub enum ToolInvocationOutcome {
    Success { value: VmValue, json: JsonValue },
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
/// Only `VmError::Thrown` can enter a declared application-error channel.
/// Control flow, host failures, and other VM errors remain runtime failures.
pub fn classify_tool_result(
    prepared: &PreparedToolCatalog,
    tool: &str,
    result: Result<VmValue, VmError>,
) -> Result<ToolInvocationOutcome, ToolInvocationError> {
    match result {
        Ok(value) => {
            let json = portable_value(tool, ToolContractPhase::Output, &value)?;
            prepared
                .validate_output(tool, &json)
                .map_err(ToolInvocationError::Contract)?;
            Ok(ToolInvocationOutcome::Success { value, json })
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

        let cancellation = VmError::Thrown(VmValue::String(
            "kind:cancelled:VM cancelled by host".into(),
        ));
        let error = classify_tool_result(
            &prepared(Some(json!({"type": "string"}))),
            "widgets.create",
            Err(cancellation),
        )
        .expect_err("host cancellation remains runtime control flow");
        assert!(matches!(
            error,
            ToolInvocationError::Runtime(VmError::Thrown(VmValue::String(message)))
                if message.as_str() == "kind:cancelled:VM cancelled by host"
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
