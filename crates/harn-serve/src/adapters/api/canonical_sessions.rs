use std::path::Path;
use std::sync::Arc;

use axum::Router;

use crate::sessions::{SharedSessionStore, SqliteSessionStore};

pub(super) fn open(workspace_root: &Path) -> Option<SharedSessionStore> {
    let path = workspace_root.join(".harn").join("session-store.sqlite");
    match SqliteSessionStore::open(&path) {
        Ok(store) => Some(Arc::new(store) as SharedSessionStore),
        Err(error) => {
            eprintln!(
                "[harn] canonical session store unavailable at {}: {error}",
                path.display()
            );
            None
        }
    }
}

pub(super) fn workspace_root_for_pipeline(path: &str) -> std::path::PathBuf {
    if let Ok(root) = std::env::var("HARN_PROJECT_ROOT") {
        if !root.trim().is_empty() {
            return root.into();
        }
    }
    Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()))
}

pub(super) fn mount<S>(router: Router<S>, store: Option<SharedSessionStore>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    match store {
        Some(store) => router.nest(
            "/v1/session-store",
            crate::sessions::sessions_router(store).with_state(()),
        ),
        None => router,
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt as _;

    use super::*;

    #[tokio::test]
    async fn canonical_store_is_reachable_at_the_default_api_mount() {
        let root = tempfile::tempdir().expect("workspace");
        let router = mount(Router::new(), open(root.path()));
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/session-store/sessions")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        assert!(root.path().join(".harn/session-store.sqlite").is_file());
    }
}
