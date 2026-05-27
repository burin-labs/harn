//! Generic JSON-RPC 2.0 client built-in.
//!
//! `jsonrpc_call(url, method, params?, options?)` posts a JSON-RPC 2.0
//! request to an HTTP endpoint and returns the `result` field — or
//! raises a thrown error if the server returned an `error` envelope.
//! `options.headers` adds request headers, `options.id` overrides the
//! envelope id (defaults to an auto-incrementing counter), and
//! `options.notify: true` sends a notification (no response expected).
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
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "jsonrpc_call: url is required",
            ))));
        }
        let method = args.get(1).map(|a| a.display()).unwrap_or_default();
        if method.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "jsonrpc_call: method is required",
            ))));
        }
        let params = args.get(2).cloned().unwrap_or(VmValue::Nil);
        let options = args.get(3).and_then(VmValue::as_dict);
        let mut headers = BTreeMap::new();
        headers.insert(
            "Content-Type".to_string(),
            VmValue::String(Rc::from("application/json")),
        );
        headers.insert(
            "Accept".to_string(),
            VmValue::String(Rc::from("application/json")),
        );
        let notify = options
            .and_then(|d| d.get("notify"))
            .map(VmValue::is_truthy)
            .unwrap_or(false);
        if let Some(extra) = options
            .and_then(|d| d.get("headers"))
            .and_then(VmValue::as_dict)
        {
            for (k, v) in extra.iter() {
                headers.insert(k.clone(), v.clone());
            }
        }
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
        let json = vm_value_to_data_value(&envelope);
        let body = serde_json::to_string(&json).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "jsonrpc_call: encode envelope: {e}"
            ))))
        })?;
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
        let response = execute_http_request("POST", &url, &request_options).await?;
        if notify {
            return Ok(VmValue::Nil);
        }
        unwrap_jsonrpc_response(response)
    });
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
    let response_dict = match &response {
        VmValue::Dict(d) => Rc::clone(d),
        _ => {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "jsonrpc_call: unexpected http response shape",
            ))));
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
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "jsonrpc_call: http {status} with empty body"
        )))));
    }
    let envelope: serde_json::Value = serde_json::from_str(&body_str).map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "jsonrpc_call: response is not JSON: {e}"
        ))))
    })?;
    if let Some(err) = envelope.get("error") {
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
        return Err(VmError::Thrown(VmValue::Dict(Rc::new(error_dict))));
    }
    let result = envelope
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    Ok(crate::schema::json_to_vm_value(&result))
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
}
