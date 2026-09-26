//! The web UI (`frontend/dist`), embedded into release builds.
//!
//! When `frontend/dist` does not exist at build time the binary is simply
//! built without a UI and serves the API only. Unknown paths that look like
//! client-side routes get `index.html` (SPA fallback); unknown paths that look
//! like files (`/missing.js`) are a 404.

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../frontend/dist"]
#[allow_missing = true]
struct Assets;

const INDEX: &str = "index.html";

/// Whether this build contains a UI.
pub fn available() -> bool {
    Assets::get(INDEX).is_some()
}

fn file_response(path: &str, file: rust_embed::EmbeddedFile) -> Response {
    let mime = file.metadata.mimetype();
    // Vite puts content-hashed files under assets/; everything else (index.html
    // in particular) must be revalidated so a new release is picked up.
    let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    let mut res = Response::new(Body::from(file.data.into_owned()));
    let headers = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(mime) {
        headers.insert(header::CONTENT_TYPE, v);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    *res.status_mut() = StatusCode::OK;
    res
}

/// The embedded file for `path`, `index.html` for client-side routes, or
/// `None` when there is no UI or the path names a missing file.
pub fn serve(path: &str) -> Option<Response> {
    let rel = path.trim_start_matches('/');
    if !rel.is_empty() {
        if let Some(file) = Assets::get(rel) {
            return Some(file_response(rel, file));
        }
        let last = rel.rsplit('/').next().unwrap_or_default();
        if last.contains('.') {
            return None;
        }
    }
    Assets::get(INDEX).map(|file| file_response(INDEX, file))
}
