//! Request ids: taken from `X-Request-Id` when it is well-formed, otherwise a
//! fresh UUID. The id is echoed in the response header, attached to every log
//! line of the request, and available to handlers (for audit rows and the error
//! envelope) through [`current`].

use std::time::Instant;

use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument;

pub const HEADER: &str = "x-request-id";

tokio::task_local! {
    static REQUEST_ID: String;
}

/// The id of the request being handled, or "" outside a request.
pub fn current() -> String {
    REQUEST_ID.try_with(Clone::clone).unwrap_or_default()
}

fn acceptable(id: &str) -> bool {
    (1..=128).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}

pub async fn middleware(req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| acceptable(v))
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let span = tracing::info_span!("request", request_id = %id, method = %req.method(), path = %req.uri().path());
    let started = Instant::now();
    let mut res = REQUEST_ID.scope(id.clone(), next.run(req)).instrument(span.clone()).await;

    // `acceptable` / UUID output are always valid header values.
    if let Ok(value) = HeaderValue::from_str(&id) {
        res.headers_mut().insert(HEADER, value);
    }
    span.in_scope(|| {
        tracing::info!(
            status = res.status().as_u16(),
            latency_ms = started.elapsed().as_secs_f64() * 1000.0,
            "request completed"
        )
    });
    res
}
