use std::collections::BTreeSet;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OriginAllowList {
    wildcard: bool,
    exact: BTreeSet<String>,
}

impl OriginAllowList {
    pub(crate) fn from_manifest(origins: &[String]) -> Self {
        if origins.is_empty() {
            return Self::wildcard();
        }

        let mut wildcard = false;
        let mut exact = BTreeSet::new();
        for origin in origins {
            let trimmed = origin.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed == "*" {
                wildcard = true;
                continue;
            }
            exact.insert(trimmed.to_string());
        }

        if wildcard || exact.is_empty() {
            Self::wildcard()
        } else {
            Self {
                wildcard: false,
                exact,
            }
        }
    }

    pub(crate) fn wildcard() -> Self {
        Self {
            wildcard: true,
            exact: BTreeSet::new(),
        }
    }

    pub(crate) fn allows(&self, origin: &str) -> bool {
        self.wildcard || self.exact.contains(origin)
    }
}

pub(crate) async fn enforce_allowed_origin(
    State(allow_list): State<Arc<OriginAllowList>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let origin = request
        .headers()
        .get(axum::http::header::ORIGIN)
        .and_then(|value| value.to_str().ok());

    match origin {
        Some(origin) if !allow_list.allows(origin) => (
            StatusCode::FORBIDDEN,
            format!("origin '{origin}' is not allowed"),
        )
            .into_response(),
        _ => next.run(request).await,
    }
}

#[cfg(test)]
mod tests {
    use super::OriginAllowList;

    #[test]
    fn empty_manifest_defaults_to_wildcard() {
        let allow_list = OriginAllowList::from_manifest(&[]);
        assert!(allow_list.allows("https://example.com"));
    }

    #[test]
    fn explicit_allow_list_restricts_origins() {
        let allow_list = OriginAllowList::from_manifest(&[
            "https://allowed.example".to_string(),
            "https://other.example".to_string(),
        ]);
        assert!(allow_list.allows("https://allowed.example"));
        assert!(!allow_list.allows("https://blocked.example"));
    }
}
