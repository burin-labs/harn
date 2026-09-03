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

/// The boolean outcome fields a handler's return declares, if any.
///
/// These are exactly the boolean signals `ok_result_failure_category` reads. A
/// failure `status` string is deliberately not among them: keying off any
/// `status` key would capture every dict that merely reports progress, so a
/// dict whose only failure signal is `status: "error"` keeps the plain display
/// path and stays misclassified. That residual is tracked on harn#7884.
pub(super) fn declared_outcome_flags(
    value: &serde_json::Value,
) -> Option<Vec<(&'static str, bool)>> {
    let object = value.as_object()?;
    let flags: Vec<(&'static str, bool)> = ["ok", "success", "isError"]
        .into_iter()
        .filter_map(|key| {
            object
                .get(key)
                .and_then(serde_json::Value::as_bool)
                .map(|flag| (key, flag))
        })
        .collect();
    (!flags.is_empty()).then_some(flags)
}

/// Wrap a handler's display rendering in the existing text envelope, carrying
/// the declared boolean outcome alongside it.
///
/// `render_tool_result` returns an envelope's `text` verbatim, so the rendered
/// transcript string is byte-identical to the bare display string this
/// replaces. Only the classifier's view changes: it now sees a parseable
/// object carrying the boolean the handler declared.
pub(super) fn text_envelope_with_outcome(
    display: String,
    flags: Vec<(&'static str, bool)>,
) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "schema".to_string(),
        serde_json::Value::String("harn.agent_tool_handler_result.v1".to_string()),
    );
    object.insert("text".to_string(), serde_json::Value::String(display));
    for (key, flag) in flags {
        object.insert(key.to_string(), serde_json::Value::Bool(flag));
    }
    serde_json::Value::Object(object)
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

        // A plain dict keeps its legacy *rendering*, which is what #6508
        // protects. harn#7884 narrowed the claim from the payload's type to
        // that rendering: a dict declaring a boolean outcome now travels in
        // the text envelope so the classifier can still read the boolean, and
        // the envelope renders to the identical display string.
        let ordinary = crate::stdlib::json_to_vm_value(&serde_json::json!({"ok": false}));
        assert_eq!(
            render_tool_result(&harn_handler_result_value(&ordinary)),
            ordinary.display(),
            "plain dictionaries retain legacy display rendering"
        );

        // A dict declaring no outcome has nothing to carry and stays a bare
        // display string, exactly as #6508 left it.
        let unmarked = crate::stdlib::json_to_vm_value(&serde_json::json!({"stdout": "done"}));
        assert!(
            harn_handler_result_value(&unmarked).is_string(),
            "a dict declaring no outcome keeps the bare display payload"
        );
    }
}
