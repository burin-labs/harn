pub(super) fn agent_tool_handler_result_text(value: &serde_json::Value) -> Option<&str> {
    let object = value.as_object()?;
    if object.get("schema")?.as_str()? != "harn.agent_tool_handler_result.v1" {
        return None;
    }
    object.get("text")?.as_str()
}

/// Whether a handler's return declares its own operation outcome.
///
/// A **boolean** `ok`, `success`, or `isError` is that declaration: a handler
/// writes one precisely to distinguish "the call returned" from "the operation
/// succeeded". This deliberately does **not** require the source to be a typed
/// struct.
///
/// The rule is boolean-only on purpose. A failure `status` string is also read
/// by `ok_result_failure_category`, but keying the structured path off any
/// `status` key would hold open every dict that reports progress, so a dict
/// whose only failure signal is `status: "error"` still takes the display path
/// and is still misclassified. That residual is tracked on harn#7884 rather
/// than fixed by widening this predicate. Requiring one meant a plain dict carrying
/// `{ok: false}` fell through to the display rendering below, arriving at
/// `ok_result_failure_category` as an unparseable string like
/// `{ok: false, error: boom}` — so every dict-shaped refusal was classified a
/// success and the loop could not see it (harn#7884).
pub(super) fn carries_typed_outcome(value: &serde_json::Value) -> bool {
    value.as_object().is_some_and(|object| {
        ["ok", "success", "isError"]
            .iter()
            .any(|key| object.get(*key).is_some_and(serde_json::Value::is_boolean))
    })
}

#[cfg(test)]
mod tests {
    use super::super::{harn_handler_result_value, render_tool_result};

    #[test]
    fn explicit_handler_result_preserves_data_and_renders_only_text() {
        let envelope = serde_json::json!({
            "schema": "harn.agent_tool_handler_result.v1",
            "text": "human feedback",
            "data": {"diagnostics_error_count": 2}
        });
        let value = crate::stdlib::json_to_vm_value(&envelope);

        assert_eq!(harn_handler_result_value(&value), envelope);
        assert_eq!(render_tool_result(&envelope), "human feedback");
    }

    #[test]
    fn ordinary_handler_dict_keeps_legacy_display_rendering() {
        let ordinary = serde_json::json!({"text": "human feedback", "data": {"count": 2}});
        let value = crate::stdlib::json_to_vm_value(&ordinary);
        let result = harn_handler_result_value(&value);

        assert!(
            result.is_string(),
            "unmarked dict returns must keep their historical display-string payload"
        );
    }

    #[test]
    fn typed_domain_outcomes_remain_structured() {
        let fields =
            crate::value::DictMap::new().update("ok".into(), crate::value::VmValue::Bool(false));
        let typed = crate::value::VmValue::struct_instance("ServiceError", fields);
        assert_eq!(
            harn_handler_result_value(&typed),
            serde_json::json!({"ok": false})
        );

        // Reversed by harn#7884. #6508 limited the structured path to typed
        // structs, which meant a plain dict declaring `{ok: false}` was
        // display-rendered into `{ok: false}` as *text* and reached the
        // failure classifier as an unparseable string, so every dict-shaped
        // refusal was reported a success. A boolean outcome now keeps the
        // return structured whatever built it.
        let ordinary = crate::stdlib::json_to_vm_value(&serde_json::json!({"ok": false}));
        assert_eq!(
            harn_handler_result_value(&ordinary),
            serde_json::json!({"ok": false}),
            "a dict declaring a boolean outcome must stay structured"
        );

        // A dict with no boolean outcome is untouched by that reversal and
        // keeps #6508's display rendering.
        let unmarked = crate::stdlib::json_to_vm_value(&serde_json::json!({"stdout": "done"}));
        assert!(
            harn_handler_result_value(&unmarked).is_string(),
            "a dict declaring no outcome retains legacy display rendering"
        );
    }
}
