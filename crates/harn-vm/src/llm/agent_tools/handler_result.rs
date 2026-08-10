pub(super) fn agent_tool_handler_result_text(value: &serde_json::Value) -> Option<&str> {
    let object = value.as_object()?;
    if object.get("schema")?.as_str()? != "harn.agent_tool_handler_result.v1" {
        return None;
    }
    object.get("text")?.as_str()
}

pub(super) fn carries_typed_outcome(
    source: &crate::value::VmValue,
    value: &serde_json::Value,
) -> bool {
    source.struct_data().is_some()
        && value.as_object().is_some_and(|object| {
            object.get("ok").is_some_and(serde_json::Value::is_boolean)
                || object
                    .get("success")
                    .is_some_and(serde_json::Value::is_boolean)
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

        let ordinary = crate::stdlib::json_to_vm_value(&serde_json::json!({"ok": false}));
        assert!(
            harn_handler_result_value(&ordinary).is_string(),
            "plain dictionaries retain legacy display rendering"
        );
    }
}
