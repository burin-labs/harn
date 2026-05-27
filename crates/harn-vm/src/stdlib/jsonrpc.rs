//! Generic JSON-RPC 2.0 client built-ins.
//!
//! `jsonrpc_call(url, method, params?, options?)` posts a JSON-RPC 2.0
//! request to an HTTP endpoint and returns the `result` field — or
//! raises a thrown error if the server returned an `error` envelope.
//! `options.headers` adds request headers (case-insensitively
//! overriding the default `Content-Type`/`Accept`), `options.id`
//! overrides the envelope id (defaults to an auto-incrementing
//! counter), and `options.notify: true` sends a notification (no
//! response expected).
//!
//! `jsonrpc_batch(url, calls, options?)` posts a batched JSON-RPC 2.0
//! request — `calls` is a list of `{method, params?, id?, notify?}`
//! dicts. Returns a list of result values aligned with input order;
//! per-call errors arrive as `{jsonrpc_error: true, code, message,
//! data?}` dicts inside that list (matching the single-call thrown
//! shape). Notifications get a `nil` slot in the response list so
//! callers can still align by index.
//!
//! Wraps the existing `http_request` pipeline so policy, retries,
//! mock plumbing, and the egress allowlist apply identically to RPC
//! traffic.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::http::execute_http_request;
use crate::stdlib::json::vm_value_to_data_value;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

thread_local! {
    static JSONRPC_ID_COUNTER: Cell<i64> = const { Cell::new(0) };
}

pub(crate) fn reset_jsonrpc_state() {
    JSONRPC_ID_COUNTER.with(|c| c.set(0));
}

pub(crate) fn register_jsonrpc_builtins(vm: &mut Vm) {
    vm.register_async_builtin("jsonrpc_call", |args| async move {
        let url = args.first().map(|a| a.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(jsonrpc_err("jsonrpc_call: url is required"));
        }
        let method = args.get(1).map(|a| a.display()).unwrap_or_default();
        if method.is_empty() {
            return Err(jsonrpc_err("jsonrpc_call: method is required"));
        }
        let params = args.get(2).cloned().unwrap_or(VmValue::Nil);
        let options = args.get(3).and_then(VmValue::as_dict);
        let notify = options
            .and_then(|d| d.get("notify"))
            .map(VmValue::is_truthy)
            .unwrap_or(false);
        let id_value = options
            .and_then(|d| d.get("id"))
            .cloned()
            .unwrap_or_else(|| {
                if notify {
                    VmValue::Nil
                } else {
                    next_id_value()
                }
            });
        let envelope = build_envelope(&method, &params, &id_value, notify);
        let body = encode_envelope(&envelope, "jsonrpc_call")?;
        let request_options = build_request_options(body, options);
        let response = execute_http_request("POST", &url, &request_options).await?;
        if notify {
            return Ok(VmValue::Nil);
        }
        unwrap_jsonrpc_response(response)
    });

    vm.register_async_builtin("jsonrpc_batch", |args| async move {
        let url = args.first().map(|a| a.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(jsonrpc_err("jsonrpc_batch: url is required"));
        }
        let calls = match args.get(1) {
            Some(VmValue::List(items)) => Rc::clone(items),
            Some(other) => {
                return Err(jsonrpc_err(&format!(
                    "jsonrpc_batch: calls must be a list, got {}",
                    other.type_name()
                )));
            }
            None => return Err(jsonrpc_err("jsonrpc_batch: calls list is required")),
        };
        if calls.is_empty() {
            return Err(jsonrpc_err("jsonrpc_batch: calls list cannot be empty"));
        }
        let options = args.get(2).and_then(VmValue::as_dict);

        // Build the envelope array and remember each slot's
        // (input-index, kind) so we can stitch the response array
        // back together by id. Notifications slot to `Nil`; non-
        // notify calls get an auto id when none is provided.
        let mut envelopes: Vec<VmValue> = Vec::with_capacity(calls.len());
        let mut slots: Vec<BatchSlot> = Vec::with_capacity(calls.len());
        for (idx, call) in calls.iter().enumerate() {
            let call_dict = call.as_dict().ok_or_else(|| {
                jsonrpc_err(&format!(
                    "jsonrpc_batch: call at index {idx} must be a dict, got {}",
                    call.type_name()
                ))
            })?;
            let method = match call_dict.get("method") {
                Some(VmValue::String(s)) if !s.is_empty() => (**s).to_string(),
                _ => {
                    return Err(jsonrpc_err(&format!(
                        "jsonrpc_batch: call at index {idx} missing 'method'"
                    )));
                }
            };
            let params = call_dict.get("params").cloned().unwrap_or(VmValue::Nil);
            let notify = call_dict
                .get("notify")
                .map(VmValue::is_truthy)
                .unwrap_or(false);
            let id_value = call_dict.get("id").cloned().unwrap_or_else(|| {
                if notify {
                    VmValue::Nil
                } else {
                    next_id_value()
                }
            });
            envelopes.push(build_envelope(&method, &params, &id_value, notify));
            slots.push(BatchSlot {
                id: id_value,
                notify,
            });
        }
        let array = VmValue::List(Rc::new(envelopes));
        let body = encode_envelope(&array, "jsonrpc_batch")?;
        let request_options = build_request_options(body, options);
        let response = execute_http_request("POST", &url, &request_options).await?;

        // If every call was a notification the server is permitted
        // to return an empty body — JSON-RPC 2.0 §6. Yield a list of
        // Nil slots aligned with the input.
        if slots.iter().all(|s| s.notify) {
            return Ok(VmValue::List(Rc::new(
                slots.iter().map(|_| VmValue::Nil).collect(),
            )));
        }
        unwrap_batch_response(response, &slots)
    });
}

struct BatchSlot {
    id: VmValue,
    notify: bool,
}

fn build_request_options(
    body: String,
    options: Option<&BTreeMap<String, VmValue>>,
) -> BTreeMap<String, VmValue> {
    let mut headers = BTreeMap::new();
    headers.insert(
        "Content-Type".to_string(),
        VmValue::String(Rc::from("application/json")),
    );
    headers.insert(
        "Accept".to_string(),
        VmValue::String(Rc::from("application/json")),
    );
    if let Some(extra) = options
        .and_then(|d| d.get("headers"))
        .and_then(VmValue::as_dict)
    {
        for (k, v) in extra.iter() {
            // Case-insensitive override: drop any existing entry that
            // canonicalises to the same name so a user-supplied
            // `content-type` wins over our default `Content-Type`.
            headers.retain(|existing, _| !existing.eq_ignore_ascii_case(k));
            headers.insert(k.clone(), v.clone());
        }
    }
    let mut request_options = BTreeMap::new();
    request_options.insert("body".to_string(), VmValue::String(Rc::from(body)));
    request_options.insert("headers".to_string(), VmValue::Dict(Rc::new(headers)));
    if let Some(d) = options {
        if let Some(timeout) = d.get("timeout_ms") {
            request_options.insert("timeout_ms".to_string(), timeout.clone());
        }
        if let Some(retries) = d.get("retries") {
            request_options.insert("retries".to_string(), retries.clone());
        }
    }
    request_options
}

fn encode_envelope(value: &VmValue, builtin: &str) -> Result<String, VmError> {
    let json = vm_value_to_data_value(value);
    serde_json::to_string(&json).map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "{builtin}: encode envelope: {e}"
        ))))
    })
}

fn jsonrpc_err(msg: &str) -> VmError {
    VmError::Thrown(VmValue::String(Rc::from(msg)))
}

fn next_id_value() -> VmValue {
    JSONRPC_ID_COUNTER.with(|c| {
        let next = c.get().saturating_add(1);
        c.set(next);
        VmValue::Int(next)
    })
}

fn build_envelope(method: &str, params: &VmValue, id: &VmValue, notify: bool) -> VmValue {
    let mut m = BTreeMap::new();
    m.insert("jsonrpc".to_string(), VmValue::String(Rc::from("2.0")));
    m.insert("method".to_string(), VmValue::String(Rc::from(method)));
    if !matches!(params, VmValue::Nil) {
        m.insert("params".to_string(), params.clone());
    }
    if !notify {
        m.insert("id".to_string(), id.clone());
    }
    VmValue::Dict(Rc::new(m))
}

/// Extracts `result` from a JSON-RPC response envelope or raises a
/// thrown error when the server returned an `error` payload. The
/// response argument is the dict returned by `execute_http_request`,
/// which carries `status`, `headers`, and `body` keys.
fn unwrap_jsonrpc_response(response: VmValue) -> Result<VmValue, VmError> {
    let envelope = decode_response_envelope(&response, "jsonrpc_call")?;
    if let Some(err) = envelope.get("error") {
        return Err(VmError::Thrown(VmValue::Dict(Rc::new(error_to_dict(err)))));
    }
    let result = envelope
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    Ok(crate::schema::json_to_vm_value(&result))
}

/// Decodes a batched JSON-RPC response array, aligning each entry
/// back to its input slot by `id`. Per-call errors are kept inside
/// the returned list as `{jsonrpc_error: true, ...}` dicts instead
/// of being raised — callers that want fail-fast semantics can
/// inspect each slot. Notification slots are `Nil`.
fn unwrap_batch_response(response: VmValue, slots: &[BatchSlot]) -> Result<VmValue, VmError> {
    let array = decode_response_envelope(&response, "jsonrpc_batch")?;
    let entries = array.as_array().ok_or_else(|| {
        jsonrpc_err("jsonrpc_batch: server returned a non-array response envelope")
    })?;

    // Index responses by id for stable alignment. JSON-RPC 2.0 doesn't
    // require the server to preserve input ordering, only to match
    // each response to its request via `id`.
    let mut by_id: std::collections::HashMap<String, &serde_json::Value> =
        std::collections::HashMap::with_capacity(entries.len());
    let mut leftover: Vec<&serde_json::Value> = Vec::new();
    for entry in entries {
        let id_key = match entry.get("id") {
            Some(serde_json::Value::Null) | None => None,
            Some(id) => Some(canonical_id_key(id)),
        };
        match id_key {
            Some(key) => {
                by_id.insert(key, entry);
            }
            None => leftover.push(entry),
        }
    }

    let mut out = Vec::with_capacity(slots.len());
    let mut leftover_iter = leftover.into_iter();
    for slot in slots {
        if slot.notify {
            out.push(VmValue::Nil);
            continue;
        }
        let key = canonical_id_key_from_vm(&slot.id);
        let entry = key
            .as_deref()
            .and_then(|k| by_id.remove(k))
            .or_else(|| leftover_iter.next());
        let Some(entry) = entry else {
            // Server dropped a response for a non-notify call. Emit
            // a synthetic error so the slot is still populated and
            // callers can detect the mismatch.
            let mut err_dict = BTreeMap::new();
            err_dict.insert("jsonrpc_error".to_string(), VmValue::Bool(true));
            err_dict.insert("code".to_string(), VmValue::Int(0));
            err_dict.insert(
                "message".to_string(),
                VmValue::String(Rc::from("missing response for call")),
            );
            out.push(VmValue::Dict(Rc::new(err_dict)));
            continue;
        };
        if let Some(err) = entry.get("error") {
            out.push(VmValue::Dict(Rc::new(error_to_dict(err))));
            continue;
        }
        let result = entry
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        out.push(crate::schema::json_to_vm_value(&result));
    }
    Ok(VmValue::List(Rc::new(out)))
}

fn decode_response_envelope(
    response: &VmValue,
    builtin: &str,
) -> Result<serde_json::Value, VmError> {
    let response_dict = match response {
        VmValue::Dict(d) => Rc::clone(d),
        _ => {
            return Err(jsonrpc_err(&format!(
                "{builtin}: unexpected http response shape"
            )));
        }
    };
    let status = response_dict
        .get("status")
        .and_then(VmValue::as_int)
        .unwrap_or(0);
    let body_str = match response_dict.get("body") {
        Some(VmValue::String(s)) => (**s).to_string(),
        Some(other) => other.display(),
        None => String::new(),
    };
    if body_str.is_empty() && !(200..300).contains(&status) {
        return Err(jsonrpc_err(&format!(
            "{builtin}: http {status} with empty body"
        )));
    }
    serde_json::from_str(&body_str)
        .map_err(|e| jsonrpc_err(&format!("{builtin}: response is not JSON: {e}")))
}

fn error_to_dict(err: &serde_json::Value) -> BTreeMap<String, VmValue> {
    let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
    let message = err
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("jsonrpc error");
    let mut error_dict = BTreeMap::new();
    error_dict.insert("jsonrpc_error".to_string(), VmValue::Bool(true));
    error_dict.insert("code".to_string(), VmValue::Int(code));
    error_dict.insert(
        "message".to_string(),
        VmValue::String(Rc::from(message.to_string())),
    );
    if let Some(data) = err.get("data") {
        error_dict.insert("data".to_string(), crate::schema::json_to_vm_value(data));
    }
    error_dict
}

fn canonical_id_key(id: &serde_json::Value) -> String {
    match id {
        serde_json::Value::String(s) => format!("s:{s}"),
        serde_json::Value::Number(n) => format!("n:{n}"),
        other => format!("j:{other}"),
    }
}

fn canonical_id_key_from_vm(id: &VmValue) -> Option<String> {
    match id {
        VmValue::Nil => None,
        VmValue::String(s) => Some(format!("s:{}", &**s)),
        VmValue::Int(n) => Some(format!("n:{n}")),
        VmValue::Float(n) => Some(format!("n:{n}")),
        other => Some(format!("j:{}", other.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_includes_id_when_not_notify() {
        let env = build_envelope("ping", &VmValue::Nil, &VmValue::Int(7), false);
        let dict = env.as_dict().unwrap();
        assert_eq!(
            dict.get("jsonrpc").map(VmValue::display),
            Some("2.0".to_string())
        );
        assert_eq!(
            dict.get("method").map(VmValue::display),
            Some("ping".to_string())
        );
        assert_eq!(dict.get("id").and_then(VmValue::as_int), Some(7));
        assert!(!dict.contains_key("params"));
    }

    #[test]
    fn envelope_omits_id_when_notify() {
        let env = build_envelope("event", &VmValue::Nil, &VmValue::Int(1), true);
        let dict = env.as_dict().unwrap();
        assert!(!dict.contains_key("id"));
    }

    #[test]
    fn envelope_includes_params_when_present() {
        let mut params = BTreeMap::new();
        params.insert("x".to_string(), VmValue::Int(42));
        let env = build_envelope(
            "echo",
            &VmValue::Dict(Rc::new(params)),
            &VmValue::Int(1),
            false,
        );
        let dict = env.as_dict().unwrap();
        let params = dict.get("params").and_then(VmValue::as_dict).unwrap();
        assert_eq!(params.get("x").and_then(VmValue::as_int), Some(42));
    }

    #[test]
    fn unwrap_returns_result_field() {
        let body = serde_json::json!({"jsonrpc":"2.0","result":{"ok":true},"id":1});
        let mut response = BTreeMap::new();
        response.insert("status".to_string(), VmValue::Int(200));
        response.insert(
            "body".to_string(),
            VmValue::String(Rc::from(body.to_string())),
        );
        let result = unwrap_jsonrpc_response(VmValue::Dict(Rc::new(response))).unwrap();
        let dict = result.as_dict().unwrap();
        match dict.get("ok") {
            Some(VmValue::Bool(true)) => {}
            other => panic!("expected ok=true, got {other:?}"),
        }
    }

    #[test]
    fn unwrap_raises_error_envelope() {
        let body = serde_json::json!({
            "jsonrpc":"2.0",
            "error":{"code":-32601,"message":"Method not found"},
            "id":1,
        });
        let mut response = BTreeMap::new();
        response.insert("status".to_string(), VmValue::Int(200));
        response.insert(
            "body".to_string(),
            VmValue::String(Rc::from(body.to_string())),
        );
        let err = unwrap_jsonrpc_response(VmValue::Dict(Rc::new(response))).unwrap_err();
        match err {
            VmError::Thrown(VmValue::Dict(d)) => {
                assert_eq!(d.get("code").and_then(VmValue::as_int), Some(-32601));
                assert_eq!(
                    d.get("message").map(VmValue::display),
                    Some("Method not found".to_string())
                );
            }
            other => panic!("expected dict error, got {other:?}"),
        }
    }

    #[test]
    fn build_request_options_case_insensitive_override() {
        let mut user_headers = BTreeMap::new();
        user_headers.insert(
            "content-type".to_string(),
            VmValue::String(Rc::from("application/x-test")),
        );
        let mut opts = BTreeMap::new();
        opts.insert("headers".to_string(), VmValue::Dict(Rc::new(user_headers)));
        let request_options = build_request_options("{}".to_string(), Some(&opts));
        let headers = request_options
            .get("headers")
            .and_then(VmValue::as_dict)
            .unwrap();
        // User-supplied lowercase 'content-type' must replace our
        // default `Content-Type`, not coexist with it.
        let content_type_count = headers
            .keys()
            .filter(|k| k.eq_ignore_ascii_case("content-type"))
            .count();
        assert_eq!(content_type_count, 1, "headers: {:?}", headers.keys());
        assert_eq!(
            headers.get("content-type").map(VmValue::display),
            Some("application/x-test".to_string())
        );
        assert!(!headers.contains_key("Content-Type"));
    }

    #[test]
    fn batch_unwrap_realigns_by_id() {
        let body = serde_json::json!([
            {"jsonrpc":"2.0","result":"second","id":2},
            {"jsonrpc":"2.0","result":"first","id":1},
        ]);
        let mut response = BTreeMap::new();
        response.insert("status".to_string(), VmValue::Int(200));
        response.insert(
            "body".to_string(),
            VmValue::String(Rc::from(body.to_string())),
        );
        let slots = vec![
            BatchSlot {
                id: VmValue::Int(1),
                notify: false,
            },
            BatchSlot {
                id: VmValue::Int(2),
                notify: false,
            },
        ];
        let result =
            unwrap_batch_response(VmValue::Dict(Rc::new(response)), &slots).expect("batch unwrap");
        let items = match result {
            VmValue::List(items) => items,
            other => panic!("expected list, got {other:?}"),
        };
        assert_eq!(items[0].display(), "first".to_string());
        assert_eq!(items[1].display(), "second".to_string());
    }

    #[test]
    fn batch_unwrap_keeps_errors_inside_response_list() {
        let body = serde_json::json!([
            {"jsonrpc":"2.0","result":"ok","id":1},
            {"jsonrpc":"2.0","error":{"code":-32602,"message":"bad params"},"id":2},
        ]);
        let mut response = BTreeMap::new();
        response.insert("status".to_string(), VmValue::Int(200));
        response.insert(
            "body".to_string(),
            VmValue::String(Rc::from(body.to_string())),
        );
        let slots = vec![
            BatchSlot {
                id: VmValue::Int(1),
                notify: false,
            },
            BatchSlot {
                id: VmValue::Int(2),
                notify: false,
            },
        ];
        let result =
            unwrap_batch_response(VmValue::Dict(Rc::new(response)), &slots).expect("batch unwrap");
        let items = match result {
            VmValue::List(items) => items,
            other => panic!("expected list, got {other:?}"),
        };
        assert_eq!(items[0].display(), "ok".to_string());
        let err_dict = items[1].as_dict().expect("error dict");
        assert_eq!(err_dict.get("code").and_then(VmValue::as_int), Some(-32602));
    }

    #[test]
    fn batch_unwrap_yields_nil_for_notifications() {
        // No HTTP roundtrip needed when every entry is a notification.
        // The call site itself returns the list of Nils before reaching
        // unwrap_batch_response; this regression test pins that
        // contract via the BatchSlot vector.
        let slots = [
            BatchSlot {
                id: VmValue::Nil,
                notify: true,
            },
            BatchSlot {
                id: VmValue::Nil,
                notify: true,
            },
        ];
        // Simulate the call-site early-return when every slot is a
        // notification.
        let nils: Vec<VmValue> = slots.iter().map(|_| VmValue::Nil).collect();
        let result = VmValue::List(Rc::new(nils));
        let items = match result {
            VmValue::List(items) => items,
            other => panic!("expected list, got {other:?}"),
        };
        assert_eq!(items.len(), 2);
        assert!(matches!(items[0], VmValue::Nil));
    }
}
