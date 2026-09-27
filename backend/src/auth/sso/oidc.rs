//! OpenID Connect relying party: authorization code flow with PKCE (S256),
//! for web sign-in only.
//!
//! - The provider is found through discovery (`{issuer}/.well-known/openid-configuration`);
//!   its `issuer` must be the configured one, and its endpoints https (or
//!   loopback http when the issuer is).
//! - The ID token comes straight from the token endpoint over TLS, and is still
//!   checked in full: signature against the provider's key set (refreshed once
//!   when a key is unknown, at most once a minute), `iss`, `aud` (and `azp`
//!   when there are several audiences), `exp`, `iat`, `nbf` and the `nonce` of
//!   this sign-in. The access token is not used.
//! - No redirects are followed and every response is capped at 1 MiB, so a
//!   misbehaving endpoint cannot make the server fetch elsewhere or buffer
//!   without bound. `HTTPS_PROXY`/`NO_PROXY` are honoured for the outbound calls.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::jose::{self, JwkSet, JwsError};
use super::tls;

/// Timeout for each call to the provider.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Largest response read from the provider.
const MAX_RESPONSE: usize = 1024 * 1024;
/// Discovery and keys are fetched again after this long.
const CACHE_TTL: Duration = Duration::from_secs(60 * 60);
/// Unknown key ids refresh the key set at most this often.
const JWKS_REFRESH_MIN: Duration = Duration::from_secs(60);
/// Clock skew tolerated between us and the provider.
const LEEWAY_SECS: i64 = 120;

/// What the provider row says about an OIDC provider.
#[derive(Debug, Clone)]
pub struct Settings {
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scopes: String,
    pub username_claim: String,
    pub groups_claim: String,
    pub ca_certificate: Option<String>,
}

/// A failure talking to the provider or checking its answer. The text is for
/// the server log and the administrator's connection test, never the browser.
#[derive(Debug, Clone)]
pub struct OidcError(pub String);

impl std::fmt::Display for OidcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn err(msg: impl Into<String>) -> OidcError {
    OidcError(msg.into())
}

#[derive(Debug, Clone, Deserialize)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub token_endpoint_auth_methods_supported: Option<Vec<String>>,
    #[serde(default)]
    pub id_token_signing_alg_values_supported: Option<Vec<String>>,
}

/// A provider ready for sign-ins: its discovery document and keys.
pub struct Provider {
    http: reqwest::Client,
    pub discovery: Discovery,
    keys: Mutex<(JwkSet, Instant)>,
    fetched: Instant,
}

/// Discovered providers, per provider id and settings.
#[derive(Default)]
pub struct Cache {
    entries: Mutex<HashMap<Uuid, (String, Arc<Provider>)>>,
}

fn fingerprint(s: &Settings, version: &str) -> String {
    format!("{version}\n{}\n{}", s.issuer_url, s.ca_certificate.as_deref().unwrap_or_default())
}

impl Cache {
    /// The provider, discovered on first use and again once the cache entry is
    /// an hour old or the provider row changed (`version`: its updated_at).
    pub async fn provider(&self, id: Uuid, version: &str, s: &Settings) -> Result<Arc<Provider>, OidcError> {
        let key = fingerprint(s, version);
        if let Some((k, p)) = self.entries.lock().expect("oidc cache").get(&id)
            && *k == key
            && p.fetched.elapsed() < CACHE_TTL
        {
            return Ok(p.clone());
        }
        let provider = Arc::new(discover(s).await?);
        self.entries.lock().expect("oidc cache").insert(id, (key, provider.clone()));
        Ok(provider)
    }

    pub fn forget(&self, id: Uuid) {
        self.entries.lock().expect("oidc cache").remove(&id);
    }
}

fn http_client(ca_pem: Option<&str>) -> Result<reqwest::Client, OidcError> {
    let tls = tls::client_config(ca_pem).map_err(|e| err(format!("CA certificate: {e}")))?;
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(HTTP_TIMEOUT)
        .user_agent(concat!("ShadouCMDB/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| err(format!("HTTP client: {e}")))
}

/// Reads at most [`MAX_RESPONSE`] bytes of a response body.
async fn body(mut res: reqwest::Response, what: &str) -> Result<Vec<u8>, OidcError> {
    let mut out = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| err(format!("{what}: reading the response failed: {e}")))? {
        if out.len() + chunk.len() > MAX_RESPONSE {
            return Err(err(format!("{what}: the response is larger than 1 MiB")));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

fn describe(e: &reqwest::Error) -> String {
    use std::error::Error;
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    text
}

async fn get_json<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
    what: &str,
) -> Result<T, OidcError> {
    let res = http
        .get(url)
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|e| err(format!("{what}: request to {url} failed: {}", describe(&e))))?;
    let status = res.status();
    if !status.is_success() {
        return Err(err(format!("{what}: {url} answered HTTP {}", status.as_u16())));
    }
    serde_json::from_slice(&body(res, what).await?)
        .map_err(|e| err(format!("{what}: {url} did not return the expected JSON: {e}")))
}

/// https, or http when the issuer itself is a loopback http URL (a test issuer).
fn endpoint_ok(issuer: &str, url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else { return false };
    match u.scheme() {
        "https" => u.host().is_some(),
        "http" => issuer.starts_with("http://") && is_loopback(&u),
        _ => false,
    }
}

pub fn is_loopback(u: &url::Url) -> bool {
    match u.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

async fn discover(s: &Settings) -> Result<Provider, OidcError> {
    let http = http_client(s.ca_certificate.as_deref())?;
    let base = s.issuer_url.trim_end_matches('/');
    let url = format!("{base}/.well-known/openid-configuration");
    let discovery: Discovery = get_json(&http, &url, "discovery").await?;
    if discovery.issuer.trim_end_matches('/') != base {
        return Err(err(format!(
            "discovery: the provider calls itself {:?}, but the configured issuer is {:?}",
            discovery.issuer, s.issuer_url
        )));
    }
    for (name, value) in [
        ("authorization_endpoint", &discovery.authorization_endpoint),
        ("token_endpoint", &discovery.token_endpoint),
        ("jwks_uri", &discovery.jwks_uri),
    ] {
        if !endpoint_ok(&discovery.issuer, value) {
            return Err(err(format!("discovery: {name} {value:?} is not an https URL")));
        }
    }
    let keys: JwkSet = get_json(&http, &discovery.jwks_uri, "key set").await?;
    let now = Instant::now();
    Ok(Provider { http, discovery, keys: Mutex::new((keys, now)), fetched: now })
}

// ---------------------------------------------------------------------------
// The authorization request
// ---------------------------------------------------------------------------

/// A random value for state, nonce and the PKCE verifier: 256 bits, base64url.
pub fn random_value() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS random number generator");
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Space-separated scopes with `openid` first and no duplicates.
pub fn scope_list(scopes: &str) -> String {
    let mut out = vec!["openid"];
    for s in scopes.split_whitespace() {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out.join(" ")
}

pub struct AuthorizationRequest<'a> {
    pub redirect_uri: &'a str,
    pub state: &'a str,
    pub nonce: &'a str,
    pub code_verifier: &'a str,
}

/// Where to send the browser.
pub fn authorization_url(p: &Provider, s: &Settings, r: &AuthorizationRequest<'_>) -> Result<String, OidcError> {
    let mut url = url::Url::parse(&p.discovery.authorization_endpoint)
        .map_err(|_| err("discovery: authorization_endpoint is not a URL"))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &s.client_id)
        .append_pair("redirect_uri", r.redirect_uri)
        .append_pair("scope", &scope_list(&s.scopes))
        .append_pair("state", r.state)
        .append_pair("nonce", r.nonce)
        .append_pair("code_challenge", &pkce_challenge(r.code_verifier))
        .append_pair("code_challenge_method", "S256");
    Ok(url.into())
}

// ---------------------------------------------------------------------------
// The token request and the ID token
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    id_token: Option<String>,
}

#[derive(Deserialize)]
struct TokenError {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// RFC 6749 §2.3.1: client_secret_basic encodes id and secret form-style first.
fn form_encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Exchanges the authorization code; returns the raw ID token.
pub async fn exchange_code(
    p: &Provider,
    s: &Settings,
    redirect_uri: &str,
    code: &str,
    code_verifier: &str,
) -> Result<String, OidcError> {
    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", code_verifier),
    ];
    let mut req = p.http.post(&p.discovery.token_endpoint);
    match s.client_secret.as_deref() {
        // Basic unless the provider says it only takes the secret in the body.
        Some(secret) => {
            let methods = p.discovery.token_endpoint_auth_methods_supported.as_deref().unwrap_or_default();
            let post_only = !methods.is_empty()
                && !methods.iter().any(|m| m == "client_secret_basic")
                && methods.iter().any(|m| m == "client_secret_post");
            if post_only {
                form.push(("client_id", &s.client_id));
                form.push(("client_secret", secret));
            } else {
                req = req.basic_auth(form_encode(&s.client_id), Some(form_encode(secret)));
            }
        }
        None => form.push(("client_id", &s.client_id)),
    }
    let res = req
        .header("accept", "application/json")
        .form(&form)
        .send()
        .await
        .map_err(|e| err(format!("token request failed: {}", describe(&e))))?;
    let status = res.status();
    let bytes = body(res, "token request").await?;
    if !status.is_success() {
        let detail = serde_json::from_slice::<TokenError>(&bytes)
            .ok()
            .map(|e| {
                let text = format!("{} {}", e.error.unwrap_or_default(), e.error_description.unwrap_or_default());
                text.trim().chars().take(300).collect::<String>()
            })
            .unwrap_or_default();
        return Err(err(format!("token request: HTTP {} {detail}", status.as_u16()).trim().to_owned()));
    }
    let parsed: TokenResponse =
        serde_json::from_slice(&bytes).map_err(|e| err(format!("token response is not JSON: {e}")))?;
    parsed.id_token.filter(|t| !t.is_empty()).ok_or_else(|| err("token response has no id_token"))
}

impl Provider {
    fn keys(&self) -> JwkSet {
        self.keys.lock().expect("jwks").0.clone()
    }

    /// Fetches the key set again unless that happened within the last minute.
    async fn refresh_keys(&self) -> Result<bool, OidcError> {
        if self.keys.lock().expect("jwks").1.elapsed() < JWKS_REFRESH_MIN {
            return Ok(false);
        }
        let keys: JwkSet = get_json(&self.http, &self.discovery.jwks_uri, "key set").await?;
        *self.keys.lock().expect("jwks") = (keys, Instant::now());
        Ok(true)
    }

    /// Verifies the ID token's signature and claims; returns the claims.
    pub async fn validate_id_token(
        &self,
        s: &Settings,
        token: &str,
        nonce: &str,
        now: i64,
    ) -> Result<Map<String, Value>, OidcError> {
        let claims = match jose::verify(token, &self.keys()) {
            Err(JwsError::UnknownKey) if self.refresh_keys().await? => jose::verify(token, &self.keys()),
            other => other,
        }
        .map_err(|e| err(format!("ID token: {e}")))?;
        check_claims(&claims, &self.discovery.issuer, &s.client_id, nonce, now)?;
        Ok(claims)
    }
}

fn number(claims: &Map<String, Value>, name: &str) -> Option<i64> {
    claims.get(name).and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
}

/// The ID token rules of OIDC Core §3.1.3.7 that apply once the signature is good.
pub fn check_claims(
    claims: &Map<String, Value>,
    issuer: &str,
    client_id: &str,
    nonce: &str,
    now: i64,
) -> Result<(), OidcError> {
    if claims.get("iss").and_then(Value::as_str) != Some(issuer) {
        return Err(err(format!("ID token: issuer {:?} is not {issuer:?}", claims.get("iss"))));
    }
    let audiences: Vec<&str> = match claims.get("aud") {
        Some(Value::String(a)) => vec![a.as_str()],
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !audiences.contains(&client_id) {
        return Err(err("ID token: it was not issued for this client (aud)"));
    }
    let azp = claims.get("azp").and_then(Value::as_str);
    if (audiences.len() > 1 || azp.is_some()) && azp != Some(client_id) {
        return Err(err("ID token: authorized party (azp) is not this client"));
    }
    let exp = number(claims, "exp").ok_or_else(|| err("ID token: no expiry (exp)"))?;
    if now > exp + LEEWAY_SECS {
        return Err(err("ID token: expired"));
    }
    let iat = number(claims, "iat").ok_or_else(|| err("ID token: no issue time (iat)"))?;
    if iat > now + LEEWAY_SECS {
        return Err(err("ID token: issued in the future (check the clocks)"));
    }
    if number(claims, "nbf").is_some_and(|nbf| nbf > now + LEEWAY_SECS) {
        return Err(err("ID token: not valid yet (nbf)"));
    }
    let sent = claims.get("nonce").and_then(Value::as_str).unwrap_or_default();
    if !crate::auth::session::constant_time_eq(sent.as_bytes(), nonce.as_bytes()) {
        return Err(err("ID token: nonce does not match this sign-in"));
    }
    match claims.get("sub").and_then(Value::as_str) {
        Some(sub) if !sub.is_empty() && sub.len() <= 255 => Ok(()),
        _ => Err(err("ID token: no usable subject (sub)")),
    }
}

/// A claim by name; `a.b.c` descends into nested objects (Keycloak's
/// `realm_access.roles`) when no claim has the dotted name itself.
pub fn claim<'c>(claims: &'c Map<String, Value>, path: &str) -> Option<&'c Value> {
    if let Some(v) = claims.get(path) {
        return Some(v);
    }
    let mut parts = path.split('.');
    let mut cur = claims.get(parts.next()?)?;
    for part in parts {
        cur = cur.as_object()?.get(part)?;
    }
    Some(cur)
}

/// The groups claim as a list: an array of strings, or one string.
pub fn groups(claims: &Map<String, Value>, path: &str) -> Vec<String> {
    match claim(claims, path) {
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn claims(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    const NOW: i64 = 1_800_000_000;

    fn good() -> Value {
        json!({ "iss": "https://idp.example.test", "aud": "cmdb", "sub": "u1", "exp": NOW + 300, "iat": NOW, "nonce": "n1" })
    }

    fn check(v: Value) -> Result<(), String> {
        check_claims(&claims(v), "https://idp.example.test", "cmdb", "n1", NOW).map_err(|e| e.0)
    }

    #[test]
    fn claims_are_checked() {
        assert_eq!(check(good()), Ok(()));
        let with = |k: &str, v: Value| {
            let mut c = good();
            c[k] = v;
            c
        };
        let without = |k: &str| {
            let mut c = good();
            c.as_object_mut().unwrap().remove(k);
            c
        };
        assert!(check(with("iss", json!("https://evil.example.test"))).unwrap_err().contains("issuer"));
        assert!(check(with("aud", json!("other"))).unwrap_err().contains("aud"));
        assert!(check(with("aud", json!(["other", "cmdb"]))).unwrap_err().contains("azp"));
        let mut two = with("aud", json!(["other", "cmdb"]));
        two["azp"] = json!("cmdb");
        assert_eq!(check(two), Ok(()));
        assert!(check(with("azp", json!("other"))).unwrap_err().contains("azp"));
        assert!(check(with("exp", json!(NOW - LEEWAY_SECS - 1))).unwrap_err().contains("expired"));
        assert_eq!(check(with("exp", json!(NOW - 10))), Ok(()), "within the clock leeway");
        assert!(check(with("iat", json!(NOW + 3600))).unwrap_err().contains("future"));
        assert!(check(with("nbf", json!(NOW + 3600))).unwrap_err().contains("nbf"));
        assert!(check(with("nonce", json!("n2"))).unwrap_err().contains("nonce"));
        assert!(check(without("nonce")).unwrap_err().contains("nonce"));
        assert!(check(without("exp")).unwrap_err().contains("exp"));
        assert!(check(without("iat")).unwrap_err().contains("iat"));
        assert!(check(with("sub", json!(""))).unwrap_err().contains("sub"));
        assert!(check(without("sub")).unwrap_err().contains("sub"));
    }

    #[test]
    fn pkce_matches_rfc_7636_appendix_b() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let v = random_value();
        assert_eq!(v.len(), 43);
        assert_ne!(v, random_value());
    }

    #[test]
    fn scopes_start_with_openid_once() {
        assert_eq!(scope_list("profile email"), "openid profile email");
        assert_eq!(scope_list(" openid  groups openid "), "openid groups");
        assert_eq!(scope_list(""), "openid");
    }

    #[test]
    fn claims_by_path_and_groups() {
        let c = claims(json!({
            "groups": ["a", "b", 3],
            "realm_access": { "roles": ["admin"] },
            "role": "single",
            "dotted.name": ["x"],
        }));
        assert_eq!(groups(&c, "groups"), ["a", "b"]);
        assert_eq!(groups(&c, "realm_access.roles"), ["admin"]);
        assert_eq!(groups(&c, "role"), ["single"]);
        assert_eq!(groups(&c, "dotted.name"), ["x"]);
        assert!(groups(&c, "missing").is_empty());
        assert!(groups(&c, "realm_access.missing").is_empty());
    }

    #[test]
    fn endpoints_must_be_https_unless_the_issuer_is_loopback_http() {
        assert!(endpoint_ok("https://idp", "https://idp.example.test/token"));
        assert!(!endpoint_ok("https://idp", "http://idp.example.test/token"));
        assert!(!endpoint_ok("https://idp", "http://127.0.0.1/token"));
        assert!(endpoint_ok("http://127.0.0.1:8080", "http://127.0.0.1:8080/token"));
        assert!(!endpoint_ok("http://127.0.0.1:8080", "http://idp.example.test/token"));
        assert!(!endpoint_ok("https://idp", "javascript:alert(1)"));
    }

    #[test]
    fn authorization_url_carries_pkce_state_and_nonce() {
        let p = Provider {
            http: http_client(None).unwrap(),
            discovery: Discovery {
                issuer: "https://idp.example.test".into(),
                authorization_endpoint: "https://idp.example.test/authorize?tenant=x".into(),
                token_endpoint: "https://idp.example.test/token".into(),
                jwks_uri: "https://idp.example.test/jwks".into(),
                token_endpoint_auth_methods_supported: None,
                id_token_signing_alg_values_supported: None,
            },
            keys: Mutex::new((JwkSet::default(), Instant::now())),
            fetched: Instant::now(),
        };
        let s = Settings {
            issuer_url: "https://idp.example.test".into(),
            client_id: "cmdb app".into(),
            client_secret: None,
            scopes: "profile".into(),
            username_claim: "preferred_username".into(),
            groups_claim: "groups".into(),
            ca_certificate: None,
        };
        let req = AuthorizationRequest {
            redirect_uri: "https://cmdb.example.com/api/v1/auth/oidc/callback",
            state: "st",
            nonce: "no",
            code_verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        };
        let url = url::Url::parse(&authorization_url(&p, &s, &req).unwrap()).unwrap();
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(q["tenant"], "x", "the endpoint's own query is kept");
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], "cmdb app");
        assert_eq!(q["scope"], "openid profile");
        assert_eq!((q["state"].as_str(), q["nonce"].as_str()), ("st", "no"));
        assert_eq!(q["code_challenge"], "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["redirect_uri"], req.redirect_uri);
    }
}
