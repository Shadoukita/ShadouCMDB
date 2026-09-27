//! `/.well-known/security.txt` (RFC 9116): where to report a vulnerability in
//! ShadouCMDB, served by every installation.
//!
//! The fields live in `security.txt` next to this file; only `Expires` is
//! added here. RFC 9116 §2.5.5 wants it less than a year ahead, and a
//! compiled-in date would expire on installations that are still supported,
//! so it is computed per request: the start of today (UTC) plus
//! [`EXPIRES_AFTER_DAYS`]. The contacts are the project's for as long as a
//! release is supported (SECURITY.md), which is what `Expires` vouches for.
//!
//! GitHub private vulnerability reporting is the only contact (SHAA-77). The
//! GitHub links only reach reporters once the repository is public.
//!
//! `Canonical` is left out on purpose: it must be the URL this file is served
//! from, which only the operator knows.

use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use chrono::{Duration, Utc};

pub const PATH: &str = "/.well-known/security.txt";

const FIELDS: &str = include_str!("security.txt");

const EXPIRES_AFTER_DAYS: i64 = 180;

fn body() -> String {
    let expires = (Utc::now().date_naive() + Duration::days(EXPIRES_AFTER_DAYS)).format("%Y-%m-%dT00:00:00Z");
    format!("{FIELDS}Expires: {expires}\n")
}

pub async fn handler() -> Response {
    ([(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"))], body()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(body: &str) -> Vec<(&str, &str)> {
        body.lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.split_once(": ").unwrap_or_else(|| panic!("not a field: {l:?}")))
            .collect()
    }

    #[test]
    fn has_the_required_fields() {
        let body = body();
        let fields = fields(&body);
        assert!(fields.iter().any(|(k, _)| *k == "Contact"));
        assert_eq!(fields.iter().filter(|(k, _)| *k == "Expires").count(), 1, "{body}");
        for (k, v) in &fields {
            if matches!(*k, "Contact" | "Policy") {
                assert!(v.starts_with("https://") || v.starts_with("mailto:"), "{k}: {v}");
            }
        }
    }

    #[test]
    fn expires_is_in_the_future_but_under_a_year() {
        let body = body();
        let (_, expires) = fields(&body).into_iter().find(|(k, _)| *k == "Expires").unwrap();
        let expires = chrono::DateTime::parse_from_rfc3339(expires).unwrap().with_timezone(&Utc);
        let now = Utc::now();
        assert!(expires > now + Duration::days(EXPIRES_AFTER_DAYS - 1), "{expires}");
        assert!(expires < now + Duration::days(365), "{expires}");
    }
}
