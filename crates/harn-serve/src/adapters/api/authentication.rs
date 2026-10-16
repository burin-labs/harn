use super::*;
use crate::auth::{AuthRequest, AuthorizationDecision};
use axum::body::Body;
use axum::extract::{FromRequest, OriginalUri, Request};
use axum::middleware::Next;

/// Native credentials authorize the whole shared workspace. Scoped and
/// tenant-bound policies belong to hosts that own resource authorization.
pub(super) async fn shared_workspace_authority(
    State(policy): State<AuthPolicy>,
    OriginalUri(uri): OriginalUri,
    request: Request,
    next: Next,
) -> Response {
    let (mut parts, body) = request.into_parts();
    let body = match Bytes::from_request(Request::from_parts(parts.clone(), body), &()).await {
        Ok(body) => body,
        Err(rejection) => return rejection.into_response(),
    };
    let auth = AuthRequest::from_http(&parts.method, uri.path(), body.to_vec(), &parts.headers);
    let principal = match policy.authorize(&auth).await {
        AuthorizationDecision::Authorized(principal) => principal,
        AuthorizationDecision::Rejected(message) => {
            return api_error(StatusCode::UNAUTHORIZED, "unauthenticated", &message)
        }
        AuthorizationDecision::MissingScope { required, granted } => {
            return forbidden_api_error(&required, &granted)
        }
        AuthorizationDecision::McpNotAllowlisted { reason, .. } => {
            return api_error(StatusCode::FORBIDDEN, "mcp_not_allowlisted", &reason)
        }
    };
    if principal.tenant_id.is_some() || !principal.granted_scopes.is_empty() {
        return api_error(StatusCode::FORBIDDEN, "unsupported_authorization_policy", "native Agents API requires server-wide credentials without tenant bindings or scoped grants");
    }
    parts.extensions.insert(principal);
    next.run(Request::from_parts(parts, Body::from(body))).await
}
