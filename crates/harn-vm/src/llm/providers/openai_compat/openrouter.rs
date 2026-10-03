//! OpenRouter upstream-routing directives.
//!
//! OpenRouter fans a model out over competing upstreams, so a capability row
//! may need to require parameter support, deny a specific misbehaving
//! upstream, or pin a closed ordered allowlist. These are the wire
//! materializations of those rows — provider-agnostic data plumbing with no
//! model-specific logic of their own.

pub(crate) fn ensure_openrouter_require_parameters(body: &mut serde_json::Value) {
    match body.get_mut("provider") {
        Some(serde_json::Value::Object(provider)) => {
            provider
                .entry("require_parameters".to_string())
                .or_insert_with(|| serde_json::json!(true));
        }
        Some(_) => {}
        None => {
            body["provider"] = serde_json::json!({"require_parameters": true});
        }
    }
}

/// Merge `deny` into the OpenRouter request body's `provider.ignore` array,
/// preserving any entries already present and de-duplicating. Creates the
/// `provider` object and/or `ignore` array when absent. This is the wire
/// materialization of the capability-row `provider_route_denylist`; it is
/// provider-agnostic data plumbing with no model-specific logic — the caller
/// decides whether a denylist applies by consulting the capability matrix.
pub(crate) fn apply_openrouter_route_denylist(body: &mut serde_json::Value, deny: &[String]) {
    if deny.is_empty() {
        return;
    }
    if !body.is_object() {
        return;
    }
    let provider = body
        .as_object_mut()
        .expect("body is an object")
        .entry("provider".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let Some(provider_obj) = provider.as_object_mut() else {
        return;
    };
    let ignore = provider_obj
        .entry("ignore".to_string())
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    let Some(ignore_arr) = ignore.as_array_mut() else {
        return;
    };
    for name in deny {
        let already_present = ignore_arr
            .iter()
            .any(|existing| existing.as_str() == Some(name.as_str()));
        if !already_present {
            ignore_arr.push(serde_json::Value::String(name.clone()));
        }
    }
}

/// Pin the OpenRouter request body to a closed, ordered allowlist of upstream
/// providers: sets `provider.order` and `provider.only` to `order`, and
/// `provider.allow_fallbacks` to `false`, so OpenRouter routes only to those
/// upstreams in preference order and never falls back to one not on the list.
/// This is the wire materialization of the capability-row
/// `openrouter_provider_order` — provider-agnostic data plumbing with no
/// model-specific logic; the caller decides whether a pin applies by consulting
/// the capability matrix. A pre-existing `provider.order` (e.g. a caller
/// override) and `provider.only` are left untouched; `allow_fallbacks` is always
/// forced to `false` so the pin is closed. No-op when `order` is empty.
pub(crate) fn apply_openrouter_provider_order(body: &mut serde_json::Value, order: &[String]) {
    if order.is_empty() {
        return;
    }
    if !body.is_object() {
        return;
    }
    let provider = body
        .as_object_mut()
        .expect("body is an object")
        .entry("provider".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let Some(provider_obj) = provider.as_object_mut() else {
        return;
    };
    // Only set the order when the caller has not already pinned one; respect an
    // explicit caller override rather than clobbering it.
    if !provider_obj.contains_key("order") {
        provider_obj.insert(
            "order".to_string(),
            serde_json::Value::Array(
                order
                    .iter()
                    .map(|name| serde_json::Value::String(name.clone()))
                    .collect(),
            ),
        );
    }
    // Apply the closed set before parameter filtering. With order/fallbacks
    // alone, OpenRouter can discard the pinned endpoints for unsupported
    // parameters, then report an unrelated fallback failure for the rest.
    let pinned = provider_obj
        .get("order")
        .expect("provider order was set")
        .clone();
    provider_obj.entry("only".to_string()).or_insert(pinned);
    // A closed allowlist must not fall back off-list.
    provider_obj.insert(
        "allow_fallbacks".to_string(),
        serde_json::Value::Bool(false),
    );
}
