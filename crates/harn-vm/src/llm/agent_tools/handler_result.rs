pub use crate::tool_registry::handler_result::AGENT_TOOL_HANDLER_RESULT_SCHEMA;
pub(in crate::llm) use crate::tool_registry::handler_result::{
    agent_tool_handler_result_text, HandlerOutcome,
};

/// Whether a JSON value contains a screenshot dict (`{base64, scale_factor}`
/// with a non-empty base64) anywhere in its tree — the distinctive `ScreenImage`
/// signature the computer tool returns.
fn json_carries_screenshot(value: &serde_json::Value) -> bool {
    if crate::llm::content::is_screenshot_dict(value) {
        return true;
    }
    match value {
        serde_json::Value::Object(map) => map.values().any(json_carries_screenshot),
        serde_json::Value::Array(items) => items.iter().any(json_carries_screenshot),
        _ => false,
    }
}

/// Validate the actual return before rendering loses its type. Envelope data
/// never decides the disposition. Nominal records declare one boolean field.
#[cfg(test)]
fn coerce_and_classify_handler_result(
    val: &crate::value::VmValue,
) -> Result<(serde_json::Value, HandlerOutcome), crate::value::VmError> {
    coerce_and_validate_handler_result(val, None)
}

pub(super) fn coerce_and_validate_handler_result(
    val: &crate::value::VmValue,
    contract: Option<(&crate::tool_registry::PreparedToolCatalog, &str)>,
) -> Result<(serde_json::Value, HandlerOutcome), crate::value::VmError> {
    use crate::tool_registry::handler_result::{invalid_handler_result, parse_handler_result};
    use crate::value::VmValue;
    let json = crate::llm::vm_value_to_json(val);
    let invalid = invalid_handler_result;
    if let Some(result) = parse_handler_result(val)? {
        validate_handler_payload(result.data, contract, result.outcome)?;
        return Ok((json, result.outcome));
    }
    if val.struct_data().is_some() {
        let ok = json.get("ok");
        let success = json.get("success");
        let declared = match (ok, success) {
            (Some(declared), None) | (None, Some(declared)) => {
                declared.as_bool().ok_or_else(invalid)?
            }
            _ => return Err(invalid()),
        };
        if declared {
            validate_handler_payload(val, contract, HandlerOutcome::Ok)?;
        }
        return Ok((
            json,
            if declared {
                HandlerOutcome::Ok
            } else {
                HandlerOutcome::Error
            },
        ));
    }
    if let VmValue::EnumVariant(variant) = val {
        if variant.has_enum_name("Result") {
            if variant.fields.len() != 1 {
                return Err(invalid());
            }
            let outcome = if variant.is_variant("Result", "Ok") {
                HandlerOutcome::Ok
            } else if variant.is_variant("Result", "Err") {
                HandlerOutcome::Error
            } else {
                return Err(invalid());
            };
            if outcome == HandlerOutcome::Ok {
                validate_handler_payload(val, contract, HandlerOutcome::Ok)?;
            }
            return Ok((json, outcome));
        }
    }
    if matches!(val, VmValue::Dict(_)) {
        return Err(invalid());
    }
    // Legacy rendered results carry model-facing text; typed data is validated above.
    let payload = if json_carries_screenshot(&json) {
        json
    } else {
        serde_json::Value::String(val.display())
    };
    Ok((payload, HandlerOutcome::Ok))
}

fn validate_handler_payload(
    value: &crate::value::VmValue,
    contract: Option<(&crate::tool_registry::PreparedToolCatalog, &str)>,
    outcome: HandlerOutcome,
) -> Result<(), crate::value::VmError> {
    let Some((prepared, name)) = contract else {
        return Ok(());
    };
    if prepared.entry(name).is_none_or(|entry| {
        if outcome == HandlerOutcome::Ok {
            entry.output_schema.is_none()
        } else {
            entry.error_schema.is_none()
        }
    }) {
        return Ok(());
    }
    let invalid = |message| crate::value::VmError::CategorizedError {
        message,
        category: crate::value::ErrorCategory::SchemaValidation,
    };
    let json = crate::tool_registry::result_to_json(value).map_err(invalid)?;
    let accepted = match outcome.application_outcome() {
        Some(disposition) => prepared
            .declared_failure(name, &json, disposition)
            .map(|_| ()),
        None => prepared.validate_output(name, &json),
    };
    accepted.map_err(|error| invalid(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::super::render_tool_result;
    use super::coerce_and_classify_handler_result;

    #[test]
    fn legacy_text_preserves_presentation_while_explicit_data_obeys_the_schema() {
        let registry = crate::schema::json_to_vm_value(&serde_json::json!({
            "_type": "tool_registry", "tools": [{
                "name": "legacy", "parameters": {}, "outputSchema": {
                    "type": "object", "properties": {"label": {"type": "string"}},
                    "required": ["label"], "additionalProperties": false
                }
            }]
        }));
        let prepared = crate::tool_registry::PreparedToolCatalog::prepare(
            crate::tool_registry::tool_registry_catalog(&registry).unwrap(),
        )
        .unwrap();
        let contract = Some((&prepared, "legacy"));
        for text in ["Custom feedback", r#"{"label":"value"}"#, r#"{"ok":false}"#] {
            let (payload, outcome) = super::coerce_and_validate_handler_result(
                &crate::value::VmValue::string(text),
                contract,
            )
            .unwrap();
            assert_eq!(payload, serde_json::Value::String(text.into()));
            assert_eq!(outcome, super::HandlerOutcome::Ok);
            assert_eq!(render_tool_result(&payload), text);
        }
        for (data, valid) in [
            (serde_json::json!({"label": "value"}), true),
            (serde_json::json!({"wrong": true}), false),
        ] {
            let envelope = crate::schema::json_to_vm_value(&serde_json::json!({
                "schema": super::AGENT_TOOL_HANDLER_RESULT_SCHEMA,
                "outcome": "ok", "text": "Custom feedback", "data": data,
            }));
            assert_eq!(
                super::coerce_and_validate_handler_result(&envelope, contract).is_ok(),
                valid
            );
        }
    }

    #[test]
    fn freeform_dicts_are_contract_errors_regardless_of_conventional_keys() {
        let failure_shapes = [
            serde_json::json!({"ok": false, "status": "blocked", "message": "apply blocked"}),
            serde_json::json!({"ok": false, "error": "boom"}),
            serde_json::json!({"success": false, "message": "rejected"}),
            serde_json::json!({"isError": true, "message": "mcp shape"}),
            serde_json::json!({"status": "error", "message": "nope"}),
        ];
        for shape in failure_shapes {
            let returned = crate::stdlib::json_to_vm_value(&shape);
            assert!(
                super::coerce_and_classify_handler_result(&returned).is_err(),
                "{shape:?}"
            );
        }

        let ok_shape = serde_json::json!({"ok": true, "message": "fine"});
        assert!(
            super::coerce_and_classify_handler_result(&crate::stdlib::json_to_vm_value(&ok_shape))
                .is_err()
        );
    }

    #[test]
    fn text_that_happens_to_contain_json_is_successful_output() {
        let text = crate::value::VmValue::string(r#"{"ok":false,"status":"error"}"#);
        let (payload, outcome) = coerce_and_classify_handler_result(&text).unwrap();
        assert_eq!(payload, serde_json::Value::String(text.display()));
        assert_eq!(outcome, super::HandlerOutcome::Ok);
    }

    #[test]
    fn explicit_handler_result_preserves_data_and_renders_only_text() {
        let envelope = serde_json::json!({
            "schema": "harn.agent_tool_handler_result.v2",
            "outcome": "ok",
            "text": "human feedback",
            "data": {"diagnostics_error_count": 2}
        });
        let value = crate::stdlib::json_to_vm_value(&envelope);

        assert_eq!(
            coerce_and_classify_handler_result(&value).unwrap().0,
            envelope
        );
        assert_eq!(render_tool_result(&envelope), "human feedback");
    }

    #[test]
    fn explicit_outcomes_ignore_failure_like_data_and_metadata() {
        for (declared, expected) in [
            ("ok", super::HandlerOutcome::Ok),
            ("error", super::HandlerOutcome::Error),
            ("rejected", super::HandlerOutcome::Rejected),
        ] {
            let envelope = serde_json::json!({
                "schema": super::AGENT_TOOL_HANDLER_RESULT_SCHEMA,
                "outcome": declared,
                "text": "feedback",
                "data": {"ok": false, "status": "error"},
                "blocked": true,
                "error": "permission_denied"
            });
            let (payload, outcome) = super::coerce_and_classify_handler_result(
                &crate::stdlib::json_to_vm_value(&envelope),
            )
            .unwrap();
            assert_eq!(payload, envelope);
            assert_eq!(outcome, expected);
        }
    }

    #[test]
    fn malformed_envelopes_and_unconventional_dicts_cannot_default_to_success() {
        for shape in [
            serde_json::json!({"failed": true, "error_code": 7}),
            serde_json::json!({}),
            serde_json::json!({"schema": super::AGENT_TOOL_HANDLER_RESULT_SCHEMA, "text": "feedback", "data": {}}),
            serde_json::json!({"schema": super::AGENT_TOOL_HANDLER_RESULT_SCHEMA, "outcome": "maybe", "text": "feedback", "data": {}}),
        ] {
            assert!(
                super::coerce_and_classify_handler_result(&crate::stdlib::json_to_vm_value(&shape))
                    .is_err(),
                "{shape}"
            );
        }
    }

    #[test]
    fn typed_domain_outcomes_remain_structured() {
        let fields =
            crate::value::DictMap::new().update("ok".into(), crate::value::VmValue::Bool(false));
        let typed = crate::value::VmValue::struct_instance("ServiceError", fields);
        assert_eq!(
            coerce_and_classify_handler_result(&typed).unwrap().0,
            serde_json::json!({"ok": false})
        );

        let ordinary = crate::stdlib::json_to_vm_value(&serde_json::json!({"ok": false}));
        assert!(
            coerce_and_classify_handler_result(&ordinary).is_err(),
            "plain dictionaries cannot substitute for nominal outcomes"
        );
    }

    #[test]
    fn result_variants_declare_outcomes_without_payload_conventions() {
        for (variant, expected) in [
            ("Ok", super::HandlerOutcome::Ok),
            ("Err", super::HandlerOutcome::Error),
        ] {
            let value = crate::value::VmValue::enum_variant(
                "Result",
                variant,
                vec![crate::value::VmValue::dict([(
                    "failed",
                    crate::value::VmValue::Bool(true),
                )])],
            );
            assert_eq!(
                coerce_and_classify_handler_result(&value).unwrap().1,
                expected
            );
        }
    }
}
