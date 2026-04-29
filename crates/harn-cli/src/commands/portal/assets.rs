use axum::http::{header, HeaderMap};
use axum::response::{Html, IntoResponse, Response};
use include_dir::{include_dir, Dir};

use super::errors::{internal_error, not_found_error};

pub(super) static PORTAL_DIST: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/portal-dist");

pub(super) async fn index() -> Response {
    match PORTAL_DIST.get_file("index.html") {
        Some(file) => Html(String::from_utf8_lossy(file.contents()).into_owned()).into_response(),
        None => internal_error("portal frontend is not built; run ./scripts/dev_setup.sh or npm --prefix crates/harn-cli/portal run build")
            .into_response(),
    }
}

pub(super) async fn asset(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    if !is_safe_asset_path(&path) {
        return not_found_error(format!("asset not found: {path}")).into_response();
    }
    let asset_path = format!("assets/{path}");
    match PORTAL_DIST.get_file(&asset_path) {
        Some(file) => asset_response(file.contents(), content_type_for_path(&asset_path)),
        None => not_found_error(format!("asset not found: {path}")).into_response(),
    }
}

fn is_safe_asset_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

pub(super) fn asset_response(body: &'static [u8], content_type: &'static str) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        content_type.parse().expect("content type"),
    );
    (headers, body).into_response()
}

pub(super) fn content_type_for_path(path: &str) -> &'static str {
    if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if path.ends_with(".js") {
        "application/javascript; charset=utf-8"
    } else if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".json") {
        "application/json; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}
