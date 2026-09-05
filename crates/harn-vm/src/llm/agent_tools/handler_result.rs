/// The `schema` value that marks a handler's return as the typed result
/// envelope rather than a freeform dict.
///
/// The runtime reader below and the `untyped-tool-handler-result` lint must
/// agree on this string exactly: the lint stays quiet for an envelope
/// precisely because the runtime reads its `text` verbatim. Two spellings of
/// it would let the lint warn about the very shape it recommends, so this is
/// the one owner and `harn-lint` reads it from here.
pub const AGENT_TOOL_HANDLER_RESULT_SCHEMA: &str = "harn.agent_tool_handler_result.v1";

pub(super) fn agent_tool_handler_result_text(value: &serde_json::Value) -> Option<&str> {
    let object = value.as_object()?;
    if object.get("schema")?.as_str()? != AGENT_TOOL_HANDLER_RESULT_SCHEMA {
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

/// Coerce a Harn tool handler's return value into the tool-result payload.
/// Preserve explicit text envelopes, computer screenshots, and typed domain
/// outcomes. Boolean `ok` or `success` distinguishes return from operation
/// success; other values retain historical display rendering.
///
pub(super) fn harn_handler_result_value(val: &crate::value::VmValue) -> serde_json::Value {
    let json = crate::llm::vm_value_to_json(val);
    if agent_tool_handler_result_text(&json).is_some()
        || json_carries_screenshot(&json)
        || carries_typed_outcome(val, &json)
    {
        json
    } else {
        serde_json::Value::String(val.display())
    }
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

/// Coerce a handler's return value and classify it in one step, while the
/// structured form is still in hand.
///
/// The coercion above renders most dicts to a display string, and that string
/// is deliberate (#6508): it is what the model reads. But it is not JSON, so
/// classifying it afterwards is impossible — `ok_result_failure_category`
/// received text like `{error: boom, ok: false}`, failed to parse it, and
/// reported every dict-shaped refusal as a success (harn#7884). Read the
/// declaration off the structured value here, before it is rendered away, and
/// hand it to the caller alongside the unchanged payload.
pub(super) fn coerce_and_classify_handler_result(
    val: &crate::value::VmValue,
) -> (serde_json::Value, Option<&'static str>) {
    let declared = super::ok_result_failure_category(&crate::llm::vm_value_to_json(val));
    (harn_handler_result_value(val), declared)
}

#[cfg(test)]
mod tests {
    use super::super::render_tool_result;
    use super::harn_handler_result_value;

    /// Reach test for harn#7884. Both halves were green in isolation while the
    /// pair was broken: the classifier was only ever fed a pre-quoted JSON
    /// string in tests, and production fed it the display rendering of a plain
    /// dict, which does not parse. Compose them the way the dispatch path
    /// does, so a regression in either half fails here.
    #[test]
    fn a_plain_dict_handler_refusal_is_classified_before_it_is_rendered_away() {
        let failure_shapes = [
            serde_json::json!({"ok": false, "status": "blocked", "message": "apply blocked"}),
            serde_json::json!({"ok": false, "error": "boom"}),
            serde_json::json!({"success": false, "message": "rejected"}),
            serde_json::json!({"isError": true, "message": "mcp shape"}),
            // Classifying the structured value covers the failure `status`
            // string too, which no rule about what to keep structured could
            // have reached without holding open every dict that reports
            // progress.
            serde_json::json!({"status": "error", "message": "nope"}),
        ];
        for shape in failure_shapes {
            // A plain dict, not a typed struct — what a `tool_define` handler
            // returns unless it goes out of its way to build a struct.
            let returned = crate::stdlib::json_to_vm_value(&shape);
            let (payload, declared) = super::coerce_and_classify_handler_result(&returned);
            assert_eq!(
                declared,
                Some("tool_error"),
                "refusal must be classified before coercion: {shape:?}"
            );
            // The reason this had to be read early: the payload the caller is
            // left holding cannot be classified.
            assert_eq!(
                super::super::ok_result_failure_category(&payload),
                None,
                "the rendered payload is unparseable — that is the defect"
            );
        }

        // Negative control: the same path must not manufacture a failure.
        let ok_shape = serde_json::json!({"ok": true, "message": "fine"});
        let (_, declared) =
            super::coerce_and_classify_handler_result(&crate::stdlib::json_to_vm_value(&ok_shape));
        assert_eq!(declared, None);
    }

    /// #6508's rendering is the constraint this fix honors, so it is pinned
    /// directly: the coerced payload for a plain dict is still the bare display
    /// string, unchanged in type and in bytes. Classification now travels
    /// beside it rather than being read out of it.
    #[test]
    fn carrying_the_outcome_does_not_change_the_rendered_payload() {
        let shapes = [
            serde_json::json!({"ok": false, "status": "blocked", "message": "apply blocked"}),
            serde_json::json!({"ok": true, "message": "fine"}),
            serde_json::json!({"success": false, "message": "rejected"}),
            serde_json::json!({"isError": true, "message": "mcp shape"}),
            serde_json::json!({"stdout": "done", "exit_code": 0}),
        ];
        for shape in shapes {
            let returned = crate::stdlib::json_to_vm_value(&shape);
            let (payload, _) = super::coerce_and_classify_handler_result(&returned);
            // Taken from the same source value rather than restated, so the
            // two cannot drift apart.
            assert_eq!(
                payload,
                serde_json::Value::String(returned.display()),
                "coerced payload must be the unchanged display string for {shape:?}"
            );
        }
    }

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
