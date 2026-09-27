//! `/api/v1`, the health probes and the OpenAPI document.
//!
//! Layers: routes (modules/*, declared with [`route::route`]) -> services
//! (modules/*) -> SQL (data/*). [`routes`] is the single list of everything
//! the API serves: the axum router and the OpenAPI document are both built
//! from it. Future modules (discovery, imports, reports) append their routes
//! here and plug middleware into http/mod.rs.

pub mod context;
pub mod openapi;
pub mod pg_error;
pub mod route;
pub mod schemas;
pub mod validate;

use axum::Router;
use axum::routing::MethodRouter;
use utoipa_swagger_ui::{Config, SwaggerUi};

use crate::http::AppState;
use crate::modules;
use route::Route;

/// Every route the API serves, in the order they appear in the OpenAPI document.
pub fn routes() -> Vec<Route> {
    [
        modules::health::routes(),
        modules::auth::routes(),
        modules::mfa::routes(),
        modules::items::routes(),
        modules::relationships::routes(),
        modules::areas::routes(),
        modules::classes::routes(),
        modules::schema_changes::routes(),
        modules::lookups::routes(),
        modules::templates::routes(),
        modules::ui_settings::routes(),
        modules::audit::routes(),
        modules::users::routes(),
        modules::profiles::routes(),
        modules::api_tokens::routes(),
        modules::config_transfer::routes(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The generated OpenAPI document as committed in backend/openapi.json.
pub fn openapi_json() -> String {
    let doc = openapi::document(&routes());
    let mut json = doc.to_pretty_json().expect("the OpenAPI document serialises");
    json.push('\n');
    json
}

/// Router for every route plus `/openapi.json` and the Swagger UI at `/docs`.
pub fn router() -> Router<AppState> {
    let routes = routes();
    let doc = openapi::document(&routes);

    let mut by_path: Vec<(String, MethodRouter<AppState>)> = Vec::new();
    for r in routes {
        match by_path.iter_mut().find(|(p, _)| *p == r.path) {
            Some((_, existing)) => {
                let merged = std::mem::take(existing).merge(r.handler);
                *existing = merged;
            }
            None => by_path.push((r.path, r.handler)),
        }
    }
    let mut router = Router::new();
    for (path, handler) in by_path {
        router = router.route(&path, handler);
    }
    // BaseLayout drops the top bar (logo and spec-URL box). Its logo SVG
    // injects an inline <style> that the CSP on /docs would block (see
    // `CSP` in http/mod.rs); the bar has nothing to offer with one document.
    router.merge(SwaggerUi::new("/docs").url("/openapi.json", doc).config(Config::default().use_base_layout()))
}
