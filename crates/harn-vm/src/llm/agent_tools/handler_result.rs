/// The `schema` value that marks a handler's return as the typed result
/// envelope rather than a freeform dict.
///
/// The runtime reader below and the `untyped-tool-handler-result` lint must
/// agree on this string exactly. `harn-lint` reads it from this owner.
pub const AGENT_TOOL_HANDLER_RESULT_SCHEMA: &str = "harn.agent_tool_handler_result.v2";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandlerOutcome {
    Ok,
    Error,
    Rejected,
}

impl HandlerOutcome {
    pub(in crate::llm) fn failure_category(self) -> Option<&'static str> {
        match self {
            Self::Ok => None,
            Self::Error => Some("tool_error"),
            Self::Rejected => Some("tool_rejected"),
        }
    }
}

/// A well-formed `harn.agent_tool_handler_result.v2` envelope, borrowed from
/// the handler's portable JSON return.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HandlerResultEnvelope<'a> {
    pub outcome: HandlerOutcome,
    pub data: &'a serde_json::Value,
}

/// The one reader of the typed handler-result envelope, shared by agent
/// dispatch and every tool-registry adapter.
///
/// `None` means the value does not claim the envelope schema. `Some(Err(()))`
/// means it claims the schema but is malformed, which every caller must
/// refuse rather than treat as freeform data.
pub(crate) fn parse_handler_result_envelope(
    value: &serde_json::Value,
) -> Option<Result<HandlerResultEnvelope<'_>, ()>> {
    let object = value.as_object()?;
    if object.get("schema").and_then(serde_json::Value::as_str)
        != Some(AGENT_TOOL_HANDLER_RESULT_SCHEMA)
    {
        return None;
    }
    let parsed = (|| {
        object.get("text")?.as_str()?;
        let data = object.get("data")?;
        let outcome = match object.get("outcome")?.as_str()? {
            "ok" => HandlerOutcome::Ok,
            "error" => HandlerOutcome::Error,
            "rejected" => HandlerOutcome::Rejected,
            _ => return None,
        };
        Some(HandlerResultEnvelope { outcome, data })
    })();
    Some(parsed.ok_or(()))
}

pub(super) fn agent_tool_handler_result_text(value: &serde_json::Value) -> Option<&str> {
    let object = value.as_object()?;
    if object.get("schema")?.as_str()? != AGENT_TOOL_HANDLER_RESULT_SCHEMA {
        return None;
    }
    object.get("text")?.as_str()
}

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
pub(super) fn coerce_and_classify_handler_result(
    val: &crate::value::VmValue,
) -> Result<(serde_json::Value, HandlerOutcome), crate::value::VmError> {
    use crate::value::{ErrorCategory, VmError, VmValue};
    let json = crate::llm::vm_value_to_json(val);
    let invalid = || VmError::CategorizedError {
        message: concat!(
            "tool handler must return a typed outcome: use ",
            "agent_tool_handler_result(text, data, outcome), or a nominal struct ",
            "with exactly one boolean ok or success field"
        )
        .into(),
        category: ErrorCategory::SchemaValidation,
    };
    if let Some(parsed) = parse_handler_result_envelope(&json) {
        let outcome = parsed.map_err(|()| invalid())?.outcome;
        return Ok((json, outcome));
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
            return Ok((json, outcome));
        }
    }
    if matches!(val, VmValue::Dict(_)) {
        return Err(invalid());
    }
    let payload = if json_carries_screenshot(&json) {
        json
    } else {
        serde_json::Value::String(val.display())
    };
    Ok((payload, HandlerOutcome::Ok))
}

#[cfg(test)]
mod tests {
    use super::super::render_tool_result;
    use super::coerce_and_classify_handler_result;

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
