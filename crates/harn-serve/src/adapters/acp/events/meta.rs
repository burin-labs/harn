//! Construction of ACP's namespaced Harn metadata envelope.

/// Merge `harn_meta` keys into `value._meta.harn`, creating intermediate
/// objects as needed. Existing `_meta.harn` keys are preserved (unless
/// overwritten by `harn_meta`). No-op when `harn_meta` is empty or
/// `value` is not a JSON object.
pub(in crate::adapters::acp) fn merge_harn_meta(
    value: &mut serde_json::Value,
    harn_meta: serde_json::Map<String, serde_json::Value>,
) {
    if harn_meta.is_empty() {
        return;
    }
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    let meta = obj
        .entry("_meta".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(meta_obj) = meta.as_object_mut() else {
        return;
    };
    let harn = meta_obj
        .entry("harn".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(harn_obj) = harn.as_object_mut() else {
        return;
    };
    for (key, value) in harn_meta {
        harn_obj.insert(key, value);
    }
}
