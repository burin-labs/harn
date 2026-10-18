use super::*;

/// Private controls go straight to the runtime, never through task admission,
/// event publication, or a durable request record.
pub(super) async fn update(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    body: Bytes,
) -> Response {
    let Ok(command) = parse_json_body(&body) else {
        return invalid_json_response();
    };
    let _control = match state.restore_session(&session_id).await {
        Ok(control) => control,
        Err(error) => return error.response(),
    };
    if let Err(error) = state.require_open_session(&session_id) {
        return error.response();
    }
    match state
        .acp
        .call(
            crate::adapters::acp::ACP_METHOD_SESSION_INPUTS,
            json!({"sessionId":session_id, "command":command}),
        )
        .await
    {
        Ok(result) => Json(result).into_response(),
        Err(_) => api_error(
            StatusCode::BAD_REQUEST,
            "invalid_session_inputs",
            "Could not apply these inputs. Check the names, expiry, and usage limits.",
        ),
    }
}
