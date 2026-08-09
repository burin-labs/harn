//! MCP 2026-07-28 multi-round-trip input requests.
//!
//! Stable MCP servers do not issue requests on a live client connection.
//! Instead, a handler returns `input_required`; the client resolves the
//! embedded requests and retries the original operation. This module gives
//! Harn handlers a small synchronous-looking boundary over that re-entry.

use std::cell::RefCell;
use std::future::Future;

use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::value::{VmError, VmValue};

/// Uncatchable VM control flow carrying the next stable MCP input round.
#[derive(Clone, Debug)]
pub struct McpInputRequired {
    pub key: String,
    pub request: JsonValue,
    pub request_state: String,
}

#[derive(Debug)]
struct InputContext {
    client_capabilities: JsonValue,
    responses: JsonMap<String, JsonValue>,
    next_request: usize,
}

tokio::task_local! {
    static INPUT_CONTEXT: RefCell<InputContext>;
}

pub(crate) async fn scope_input_context<F>(
    params: &JsonValue,
    client_capabilities: JsonValue,
    future: F,
) -> Result<F::Output, String>
where
    F: Future,
{
    let mut responses = match params.get("requestState") {
        Some(JsonValue::String(state)) => serde_json::from_str::<JsonMap<String, JsonValue>>(state)
            .map_err(|error| format!("invalid MCP requestState: {error}"))?,
        Some(JsonValue::Null) | None => JsonMap::new(),
        Some(_) => return Err("MCP requestState must be a string".to_string()),
    };
    if let Some(current) = params.get("inputResponses") {
        let current = current
            .as_object()
            .ok_or_else(|| "MCP inputResponses must be an object".to_string())?;
        responses.extend(current.clone());
    }
    Ok(INPUT_CONTEXT
        .scope(
            RefCell::new(InputContext {
                client_capabilities,
                responses,
                next_request: 0,
            }),
            future,
        )
        .await)
}

/// Resolve an embedded input response or suspend the handler with the request
/// the client must fulfill before retrying the original operation.
pub(crate) fn request_input(
    method: &str,
    params: JsonValue,
    error_prefix: &str,
) -> Result<JsonValue, VmError> {
    INPUT_CONTEXT
        .try_with(|cell| {
            let mut context = cell.borrow_mut();
            if !supports_input(&context.client_capabilities, method, &params) {
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    format!("{error_prefix}: the MCP client did not advertise support for {method}"),
                ))));
            }

            let key = format!("harn-input-{}", context.next_request);
            context.next_request += 1;
            if let Some(response) = context.responses.get(&key) {
                return Ok(response.clone());
            }

            let request_state = serde_json::to_string(&context.responses)
                .expect("MCP input responses are JSON serializable");
            Err(VmError::McpInputRequired(Box::new(McpInputRequired {
                key,
                request: serde_json::json!({"method": method, "params": params}),
                request_state,
            })))
        })
        .unwrap_or_else(|_| {
            Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!(
                    "{error_prefix}: no active MCP request; this builtin is only valid inside a served MCP handler"
                ),
            ))))
        })
}

fn supports_input(capabilities: &JsonValue, method: &str, params: &JsonValue) -> bool {
    match method {
        "elicitation/create" => {
            let Some(elicitation) = capabilities
                .get("elicitation")
                .and_then(JsonValue::as_object)
            else {
                return false;
            };
            match params
                .get("mode")
                .and_then(JsonValue::as_str)
                .unwrap_or("form")
            {
                "form" => elicitation.contains_key("form"),
                "url" => elicitation.contains_key("url"),
                _ => false,
            }
        }
        "roots/list" => capabilities.get("roots").is_some(),
        "sampling/createMessage" => capabilities.get("sampling").is_some(),
        _ => false,
    }
}

pub(crate) fn input_result(error: McpInputRequired) -> JsonValue {
    serde_json::json!({
        "resultType": "input_required",
        "inputRequests": {error.key: error.request},
        "requestState": error.request_state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn preserves_responses_across_reentered_rounds() {
        let params = serde_json::json!({
            "requestState": "{\"harn-input-0\":{\"roots\":[]}}",
            "inputResponses": {"harn-input-1": {"action": "decline"}},
        });
        let result = scope_input_context(
            &params,
            serde_json::json!({"roots": {}, "elicitation": {"form": {}}}),
            async {
                let roots = request_input("roots/list", serde_json::json!({}), "roots").unwrap();
                let elicitation = request_input(
                    "elicitation/create",
                    serde_json::json!({"mode": "form"}),
                    "elicit",
                )
                .unwrap();
                (roots, elicitation)
            },
        )
        .await
        .unwrap();
        assert_eq!(result.0, serde_json::json!({"roots": []}));
        assert_eq!(result.1, serde_json::json!({"action": "decline"}));
    }

    #[tokio::test]
    async fn suspends_with_stable_input_required_payload() {
        let error = scope_input_context(
            &serde_json::json!({}),
            serde_json::json!({"elicitation": {"form": {}}}),
            async {
                request_input(
                    "elicitation/create",
                    serde_json::json!({"mode": "form", "message": "Continue?"}),
                    "elicit",
                )
            },
        )
        .await
        .unwrap()
        .unwrap_err();
        let VmError::McpInputRequired(error) = error else {
            panic!("expected input-required control flow")
        };
        let result = input_result(*error);
        assert_eq!(result["resultType"], "input_required");
        assert_eq!(
            result["inputRequests"]["harn-input-0"]["method"],
            "elicitation/create"
        );
    }

    #[test]
    fn requires_the_declared_elicitation_mode() {
        let form = serde_json::json!({"mode": "form"});
        let url = serde_json::json!({"mode": "url"});

        assert!(!supports_input(
            &serde_json::json!({"elicitation": {}}),
            "elicitation/create",
            &form,
        ));
        assert!(supports_input(
            &serde_json::json!({"elicitation": {"form": {}}}),
            "elicitation/create",
            &form,
        ));
        assert!(!supports_input(
            &serde_json::json!({"elicitation": {"form": {}}}),
            "elicitation/create",
            &url,
        ));
        assert!(supports_input(
            &serde_json::json!({"elicitation": {"url": {}}}),
            "elicitation/create",
            &url,
        ));
    }
}
