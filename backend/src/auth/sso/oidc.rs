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
//! - With `OIDC_ALLOWED_HOSTS` set ([`AllowedHosts`]), the server contacts no
//!   other host: the discovery URL, and the `token_endpoint` and `jwks_uri` the
//!   discovery document names, are all checked before any request. Those are
//!   the only URLs the server fetches (no userinfo call; the browser, not the
//!   server, goes to `authorization_endpoint`).

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
    pub mfa: MfaPolicy,
}

/// Whether a sign-in through the provider must prove a second factor in the
/// ID token before it counts as MFA for a `requireMfa` profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MfaPolicy {
    /// The token must prove it ([`provider_mfa`]); with `required_acr` set,
    /// its `acr` must be one of them, otherwise `amr` decides.
    Verify { required_acr: Vec<String> },
    /// The administrator states the provider enforces MFA; nothing is checked.
    TrustProvider,
}

impl MfaPolicy {
    pub const VERIFY: &'static str = "verify";
    pub const TRUST_PROVIDER: &'static str = "trust_provider";

    /// From the provider row's `mfa_assurance` and `required_acr`. Anything
    /// but `trust_provider` verifies (fail closed).
    pub fn from_row(mfa_assurance: Option<&str>, required_acr: Option<&[String]>) -> MfaPolicy {
        match mfa_assurance {
            Some(MfaPolicy::TRUST_PROVIDER) => MfaPolicy::TrustProvider,
            _ => MfaPolicy::Verify { required_acr: required_acr.unwrap_or_default().to_vec() },
        }
    }

    pub fn required_acr(&self) -> &[String] {
        match self {
            MfaPolicy::Verify { required_acr } => required_acr,
            MfaPolicy::TrustProvider => &[],
        }
    }
}

/// RFC 8176 `amr` values by authentication factor category; any other value
/// counts for nothing.
const KNOWLEDGE: &[&str] = &["pwd", "pin", "kba"];
const POSSESSION: &[&str] = &["hwk", "swk", "otp", "sc", "sms", "tel", "pop"];
const INHERENCE: &[&str] = &["fpt", "face", "iris", "retina", "vbm"];

/// Whether the (signature-verified) ID token claims prove a second factor
/// under `Verify`. With `required_acr`, `acr` must be exactly one of them;
/// otherwise `amr` must contain `mfa` or cover two factor categories.
/// Anything missing or malformed is false. `TrustProvider` is never proof.
pub fn provider_mfa(claims: &Map<String, Value>, policy: &MfaPolicy) -> bool {
    let MfaPolicy::Verify { required_acr } = policy else { return false };
    if !required_acr.is_empty() {
        return claims.get("acr").and_then(Value::as_str).is_some_and(|acr| required_acr.iter().any(|r| r == acr));
    }
    let Some(amr) = claims.get("amr").and_then(Value::as_array) else { return false };
    let Some(values) = amr.iter().map(Value::as_str).collect::<Option<Vec<&str>>>() else { return false };
    if values.contains(&"mfa") {
        return true;
    }
    let categories = [KNOWLEDGE, POSSESSION, INHERENCE].iter().filter(|c| values.iter().any(|v| c.contains(v))).count();
    categories >= 2
}

/// `OIDC_ALLOWED_HOSTS`: the hosts the server may contact for OIDC. An entry
/// is a host name or IP address (`[...]` for IPv6), optionally with `:port`;
/// names compare exactly after lower-casing (no suffix or wildcard matching).
/// An entry without a port allows any port on that host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedHosts(Vec<(url::Host<String>, Option<u16>)>);

impl AllowedHosts {
    /// A comma-separated list; an empty list is refused (unset the variable
    /// to allow any host).
    pub fn parse(raw: &str) -> Result<AllowedHosts, String> {
        let mut hosts = Vec::new();
        for entry in raw.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            hosts.push(Self::entry(entry).ok_or_else(|| {
                format!(
                    "{entry:?} is not a host name or IP address with an optional :port (no scheme, path or wildcard)"
                )
            })?);
        }
        if hosts.is_empty() {
            return Err("lists no host; leave it unset to allow any OIDC provider host".into());
        }
        Ok(AllowedHosts(hosts))
    }

    fn entry(entry: &str) -> Option<(url::Host<String>, Option<u16>)> {
        let lower = entry.to_ascii_lowercase();
        // Parsed as the authority of a URL, so names compare as reqwest will see them.
        let url = url::Url::parse(&format!("https://{lower}/")).ok()?;
        if !url.username().is_empty() || url.password().is_some() || url.path() != "/" || url.query().is_some() {
            return None;
        }
        if lower.contains('*') || lower.contains('/') || lower.ends_with(':') {
            return None;
        }
        // `Url` drops the https default port; keep an explicit `:443`.
        let port = url.port().or_else(|| lower.ends_with(":443").then_some(443));
        Some((url.host()?.to_owned(), port))
    }

    pub fn allows(&self, url: &url::Url) -> bool {
        let Some(host) = url.host() else { return false };
        let port = url.port_or_known_default();
        self.0.iter().any(|(h, p)| h.to_string() == host.to_string() && p.is_none_or(|p| Some(p) == port))
    }

    /// Refuses `url` unless it is on the list; the error names the URL for
    /// the server log only ([`OidcError::summary`] does not).
    fn check(&self, what: &str, url: &str) -> Result<(), OidcError> {
        match url::Url::parse(url) {
            Ok(u) if self.allows(&u) => Ok(()),
            _ => Err(OidcError {
                detail: format!("{what}: {url} is not on a host in OIDC_ALLOWED_HOSTS"),
                kind: ErrorKind::HostNotAllowed,
            }),
        }
    }
}

/// A failure talking to the provider or checking its answer. `Display` gives
/// the full text, for the server log only; [`OidcError::summary`] is what the
/// administrator's connection test may show. Never sent to the browser.
#[derive(Debug, Clone)]
pub struct OidcError {
    pub detail: String,
    kind: ErrorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorKind {
    /// The provider answered over verified TLS, or its answer is at fault.
    Answer,
    /// No verified TLS answer came back (connect, TLS or transport failure).
    Unreachable,
    /// `OIDC_ALLOWED_HOSTS` does not list the host; nothing was sent.
    HostNotAllowed,
}

/// What the connection test shows instead of the transport error: the error
/// texts would tell a closed, a filtered and a non-TLS port apart (GH#125).
pub const UNREACHABLE: &str = "Could not reach the provider over verified TLS (no connection, TLS handshake failed or \
                               no answer); the server log has the details";

/// What the connection test shows for a URL outside `OIDC_ALLOWED_HOSTS`,
/// without naming the URL or host.
pub const HOST_NOT_ALLOWED: &str = "The provider uses a host this server may not contact (OIDC_ALLOWED_HOSTS); \
                                    the server log has the details";

impl OidcError {
    pub fn new(msg: impl Into<String>) -> Self {
        OidcError { detail: msg.into(), kind: ErrorKind::Answer }
    }

    fn unreachable(msg: String) -> Self {
        OidcError { detail: msg, kind: ErrorKind::Unreachable }
    }

    /// The text for the administrator: the detail once the provider answered
    /// over verified TLS, else [`UNREACHABLE`] or [`HOST_NOT_ALLOWED`].
    pub fn summary(&self) -> &str {
        match self.kind {
            ErrorKind::Answer => &self.detail,
            ErrorKind::Unreachable => UNREACHABLE,
            ErrorKind::HostNotAllowed => HOST_NOT_ALLOWED,
        }
    }
}

impl std::fmt::Display for OidcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

fn err(msg: impl Into<String>) -> OidcError {
    OidcError::new(msg)
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
    #[serde(default)]
    pub claims_supported: Option<Vec<String>>,
    #[serde(default)]
    pub acr_values_supported: Option<Vec<String>>,
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
    /// `OIDC_ALLOWED_HOSTS`; None allows any host.
    allowed_hosts: Option<AllowedHosts>,
}

fn fingerprint(s: &Settings, version: &str) -> String {
    format!("{version}\n{}\n{}", s.issuer_url, s.ca_certificate.as_deref().unwrap_or_default())
}

impl Cache {
    pub fn new(allowed_hosts: Option<AllowedHosts>) -> Self {
        Cache { entries: Mutex::default(), allowed_hosts }
    }

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
        let provider = Arc::new(discover(s, self.allowed_hosts.as_ref()).await?);
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
    while let Some(chunk) = res
        .chunk()
        .await
        .map_err(|e| OidcError::unreachable(format!("{what}: reading the response failed: {}", describe(&e))))?
    {
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
        .map_err(|e| OidcError::unreachable(format!("{what}: request to {url} failed: {}", describe(&e))))?;
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

async fn discover(s: &Settings, allowed: Option<&AllowedHosts>) -> Result<Provider, OidcError> {
    let http = http_client(s.ca_certificate.as_deref())?;
    let base = s.issuer_url.trim_end_matches('/');
    let url = format!("{base}/.well-known/openid-configuration");
    if let Some(allowed) = allowed {
        allowed.check("discovery", &url)?;
    }
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
    // The document may name any host: every URL the server will fetch is checked.
    if let Some(allowed) = allowed {
        allowed.check("discovery: token_endpoint", &discovery.token_endpoint)?;
        allowed.check("discovery: jwks_uri", &discovery.jwks_uri)?;
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
    // A request only, so the provider can step up; provider_mfa() enforces it.
    let acr = s.mfa.required_acr();
    if !acr.is_empty() {
        url.query_pairs_mut().append_pair("acr_values", &acr.join(" "));
    }
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
        .map_err(|e| OidcError::unreachable(format!("token request failed: {}", describe(&e))))?;
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

    /// Random per test run, so no hard-coded nonce reaches the check.
    static NONCE: std::sync::LazyLock<String> = std::sync::LazyLock::new(random_value);

    fn good() -> Value {
        json!({ "iss": "https://idp.example.test", "aud": "cmdb", "sub": "u1", "exp": NOW + 300, "iat": NOW, "nonce": *NONCE })
    }

    fn check(v: Value) -> Result<(), String> {
        check_claims(&claims(v), "https://idp.example.test", "cmdb", &NONCE, NOW).map_err(|e| e.detail)
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
        assert!(check(with("nonce", json!(random_value()))).unwrap_err().contains("nonce"));
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
                claims_supported: None,
                acr_values_supported: None,
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
            mfa: MfaPolicy::Verify { required_acr: Vec::new() },
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
        assert!(!q.contains_key("acr_values"), "nothing asked for without requiredAcr");

        // GH#131 (c): requiredAcr is asked for (the check is provider_mfa's).
        let gold = Settings { mfa: MfaPolicy::Verify { required_acr: vec!["urn:x:gold".into()] }, ..s.clone() };
        let raw = authorization_url(&p, &gold, &req).unwrap();
        assert!(raw.contains("acr_values=urn%3Ax%3Agold"), "{raw}");
        let two = Settings { mfa: MfaPolicy::Verify { required_acr: vec!["a".into(), "b".into()] }, ..s.clone() };
        let url = url::Url::parse(&authorization_url(&p, &two, &req).unwrap()).unwrap();
        assert_eq!(url.query_pairs().find(|(k, _)| k == "acr_values").unwrap().1, "a b");
        let trust = Settings { mfa: MfaPolicy::TrustProvider, ..s };
        assert!(!authorization_url(&p, &trust, &req).unwrap().contains("acr_values"));
    }

    /// GH#131 (g): what counts as proof of a second factor in the ID token.
    #[test]
    fn provider_mfa_needs_proof_in_the_token() {
        let amr = MfaPolicy::Verify { required_acr: Vec::new() };
        let cases = [
            (json!({ "amr": ["pwd"] }), false),
            (json!({ "amr": ["otp"] }), false),
            (json!({ "amr": ["pwd", "otp"] }), true),
            (json!({ "amr": ["hwk", "fpt"] }), true),
            (json!({ "amr": ["pwd", "kba"] }), false),
            (json!({ "amr": ["pwd", "mfa"] }), true),
            (json!({ "amr": ["mfa"] }), true),
            (json!({ "amr": ["MFA"] }), false),
            (json!({ "amr": ["PWD", "OTP"] }), false),
            (json!({ "amr": ["pwd", "unknown", "wia"] }), false),
            (json!({ "amr": ["face", "sms"] }), true),
            (json!({ "amr": [] }), false),
            (json!({ "amr": "mfa" }), false),
            (json!({ "amr": ["pwd", 1] }), false),
            (json!({ "amr": ["mfa", null] }), false),
            (json!({ "amr": null }), false),
            (json!({ "acr": "mfa" }), false),
            (json!({}), false),
        ];
        for (c, expected) in &cases {
            assert_eq!(provider_mfa(&claims(c.clone()), &amr), *expected, "{c}");
        }

        let gold = MfaPolicy::Verify { required_acr: vec!["urn:x:gold".into(), "loa3".into()] };
        let acr_cases = [
            (json!({ "acr": "urn:x:gold" }), true),
            (json!({ "acr": "loa3", "amr": ["pwd"] }), true),
            (json!({ "acr": "urn:x:silver", "amr": ["mfa"] }), false),
            (json!({ "acr": "URN:X:GOLD" }), false),
            (json!({ "acr": ["urn:x:gold"] }), false),
            (json!({ "amr": ["pwd", "otp"] }), false),
            (json!({}), false),
        ];
        for (c, expected) in &acr_cases {
            assert_eq!(provider_mfa(&claims(c.clone()), &gold), *expected, "acr: {c}");
        }
        assert!(!provider_mfa(&claims(json!({ "amr": ["mfa"] })), &MfaPolicy::TrustProvider));

        assert_eq!(MfaPolicy::from_row(Some("trust_provider"), Some(&[])), MfaPolicy::TrustProvider);
        assert_eq!(MfaPolicy::from_row(Some("verify"), None), amr);
        assert_eq!(MfaPolicy::from_row(None, None), amr, "fails closed");
        assert_eq!(MfaPolicy::from_row(Some("bogus"), None), amr, "fails closed");
    }

    /// GH#125: a closed port, a port that does not speak TLS and one that
    /// never answers read the same; only the log has the cause.
    #[tokio::test]
    async fn unreachable_providers_all_read_the_same() {
        use tokio::io::AsyncWriteExt;
        let closed = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap().port()
        };
        let plain = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let plain_port = plain.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = plain.accept().await {
                let _ = socket.write_all(b"SSH-2.0-OpenSSH_9.6\r\n").await;
            }
        });
        let mut texts = Vec::new();
        for port in [closed, plain_port] {
            let s = Settings {
                issuer_url: format!("https://127.0.0.1:{port}"),
                client_id: "cmdb".into(),
                client_secret: None,
                scopes: "openid".into(),
                username_claim: "sub".into(),
                groups_claim: "groups".into(),
                ca_certificate: None,
                mfa: MfaPolicy::TrustProvider,
            };
            let e = discover(&s, None).await.err().expect("nothing to discover");
            assert_eq!(e.summary(), UNREACHABLE, "{e}");
            assert!(e.to_string().starts_with("discovery: request to "), "{e}");
            texts.push(e.to_string());
        }
        assert_ne!(texts[0], texts[1], "the log keeps the cause");
        assert_eq!(OidcError::new("discovery: x answered HTTP 404").summary(), "discovery: x answered HTTP 404");
    }
    #[test]
    fn allowed_hosts_match_exact_names_and_ports() {
        let allowed =
            AllowedHosts::parse(" IdP.Example.test , login.example.test:8443,[2001:DB8::1]:443, 192.0.2.7").unwrap();
        let ok = |u: &str| allowed.allows(&url::Url::parse(u).unwrap());
        assert!(ok("https://idp.example.test/.well-known/openid-configuration"));
        assert!(ok("https://IDP.EXAMPLE.TEST:9000/x"), "no port in the entry: any port");
        assert!(ok("https://login.example.test:8443/token"));
        assert!(!ok("https://login.example.test/token"), "the entry's port only");
        assert!(ok("https://[2001:db8::1]/jwks"), ":443 is the https default");
        assert!(!ok("https://[2001:db8::1]:8443/jwks"));
        assert!(ok("https://192.0.2.7/x"));
        // No suffix, prefix or look-alike matching.
        assert!(!ok("https://evil.idp.example.test/x"));
        assert!(!ok("https://idp.example.test.evil.test/x"));
        assert!(!ok("https://example.test/x"));
        assert!(!ok("https://idp.example.test@evil.test/x"));
        assert!(!ok("https://192.0.2.70/x"));

        for bad in
            ["", " , ", "https://idp.example.test", "*.example.test", "idp.example.test/path", "user@idp", "idp:x"]
        {
            assert!(AllowedHosts::parse(bad).is_err(), "{bad:?}");
        }
    }

    /// A stand-in provider on 127.0.0.1 whose discovery document names
    /// endpoints on `localhost` (another host name, same port); counts the
    /// requests it gets per path.
    async fn provider_naming(other: &str) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let issuer = format!("http://127.0.0.1:{port}");
        let other = format!("http://{other}:{port}");
        let doc = json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/authorize"),
            "token_endpoint": format!("{other}/token"),
            "jwks_uri": format!("{other}/jwks"),
        });
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let app = axum::Router::new()
            .route("/.well-known/openid-configuration", axum::routing::get(move || async move { axum::Json(doc) }))
            .route("/jwks", axum::routing::get(|| async { axum::Json(json!({ "keys": [] })) }))
            .layer(axum::middleware::from_fn(move |req: axum::extract::Request, next: axum::middleware::Next| {
                log.lock().unwrap().push(req.uri().path().to_owned());
                next.run(req)
            }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (issuer, seen)
    }

    fn settings(issuer: &str) -> Settings {
        Settings {
            issuer_url: issuer.into(),
            client_id: "cmdb".into(),
            client_secret: None,
            scopes: "openid".into(),
            username_claim: "sub".into(),
            groups_claim: "groups".into(),
            ca_certificate: None,
            mfa: MfaPolicy::TrustProvider,
        }
    }

    /// GH-192: with OIDC_ALLOWED_HOSTS set, no request goes to a host off the
    /// list, whether it is the issuer or an endpoint the discovery document names.
    #[tokio::test]
    async fn allowed_hosts_cover_every_url_the_server_fetches() {
        let (issuer, seen) = provider_naming("localhost").await;
        let port = url::Url::parse(&issuer).unwrap().port().unwrap();
        let only = |hosts: &str| AllowedHosts::parse(hosts).unwrap();

        // The issuer's host is not listed: nothing is sent at all.
        let e = discover(&settings(&issuer), Some(&only(&format!("127.0.0.1:{}", port + 1)))).await.err().unwrap();
        assert_eq!(e.summary(), HOST_NOT_ALLOWED);
        assert!(e.to_string().contains(&issuer), "the log names the URL: {e}");
        assert!(seen.lock().unwrap().is_empty());

        // The issuer is listed, but the document points the key set and token
        // endpoint at another host: refused before the key set is fetched.
        let e = discover(&settings(&issuer), Some(&only("127.0.0.1"))).await.err().unwrap();
        assert_eq!(e.summary(), HOST_NOT_ALLOWED);
        assert!(e.to_string().contains("token_endpoint"), "{e}");
        assert_eq!(*seen.lock().unwrap(), ["/.well-known/openid-configuration"]);
        assert!(!HOST_NOT_ALLOWED.contains("localhost") && !HOST_NOT_ALLOWED.contains("127.0.0.1"));

        // Both hosts listed: discovery completes. Unset: any host, as before.
        discover(&settings(&issuer), Some(&only(&format!("127.0.0.1, localhost:{port}")))).await.unwrap();
        discover(&settings(&issuer), None).await.unwrap();
        assert_eq!(seen.lock().unwrap().iter().filter(|p| *p == "/jwks").count(), 2);
    }

    /// Redirects are not followed, so a listed host cannot send the server on
    /// to one that is not.
    #[tokio::test]
    async fn redirects_are_not_followed() {
        let (elsewhere, seen) = provider_naming("127.0.0.1").await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let target = format!("{elsewhere}/.well-known/openid-configuration");
        let app = axum::Router::new().route(
            "/.well-known/openid-configuration",
            axum::routing::get(move || async move { axum::response::Redirect::temporary(&target) }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let e = discover(&settings(&issuer), Some(&AllowedHosts::parse("127.0.0.1").unwrap())).await.err().unwrap();
        assert!(e.to_string().contains("answered HTTP 307"), "{e}");
        assert!(seen.lock().unwrap().is_empty(), "the redirect target was not contacted");
    }
}
