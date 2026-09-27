//! Signature checks for OIDC ID tokens (JWS compact serialisation, RFC 7515)
//! against the provider's JSON Web Key Set (RFC 7517), with ring.
//!
//! Only asymmetric algorithms are accepted: RS256/384/512, PS256/384/512,
//! ES256/384 and EdDSA (Ed25519). `none` and the HMAC algorithms are refused
//! outright, so neither an unsigned token nor one "signed" with a public key
//! as an HMAC secret gets through. The key must be of the algorithm's family
//! (and name the algorithm if it names one), RSA keys must have at least 2048
//! bits (ring refuses smaller ones), and a header with `crit` is refused
//! because no extension is understood.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature::{self, RsaPublicKeyComponents, UnparsedPublicKey};
use serde::Deserialize;
use serde_json::Value;

/// A JSON Web Key as the provider publishes it; unknown members are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct Jwk {
    pub kty: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default, rename = "use")]
    pub key_use: Option<String>,
    #[serde(default)]
    pub alg: Option<String>,
    #[serde(default)]
    pub n: Option<String>,
    #[serde(default)]
    pub e: Option<String>,
    #[serde(default)]
    pub crv: Option<String>,
    #[serde(default)]
    pub x: Option<String>,
    #[serde(default)]
    pub y: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct JwkSet {
    #[serde(default)]
    pub keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
struct Header {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    crit: Option<Value>,
}

/// Why a token was refused. The message is for the server log, never the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JwsError {
    Malformed(&'static str),
    Algorithm(String),
    /// No key in the set matches the header's `kid` (the set may be stale).
    UnknownKey,
    BadKey(&'static str),
    BadSignature,
}

impl std::fmt::Display for JwsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JwsError::Malformed(what) => write!(f, "malformed token: {what}"),
            JwsError::Algorithm(alg) => write!(f, "signature algorithm {alg:?} is not accepted"),
            JwsError::UnknownKey => write!(f, "no key in the provider's key set matches the token"),
            JwsError::BadKey(what) => write!(f, "unusable key in the provider's key set: {what}"),
            JwsError::BadSignature => write!(f, "the signature does not verify"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Rsa,
    Ec(&'static str),
    Okp,
}

fn family(alg: &str) -> Option<Family> {
    Some(match alg {
        "RS256" | "RS384" | "RS512" | "PS256" | "PS384" | "PS512" => Family::Rsa,
        "ES256" => Family::Ec("P-256"),
        "ES384" => Family::Ec("P-384"),
        "EdDSA" => Family::Okp,
        _ => return None,
    })
}

fn b64(s: &str, what: &'static str) -> Result<Vec<u8>, JwsError> {
    URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')).map_err(|_| JwsError::Malformed(what))
}

fn key_b64(s: Option<&String>, what: &'static str) -> Result<Vec<u8>, JwsError> {
    let s = s.ok_or(JwsError::BadKey(what))?;
    URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')).map_err(|_| JwsError::BadKey(what))
}

/// Whether `key` may check a signature made with `alg`.
fn usable(key: &Jwk, alg: &str, fam: Family) -> bool {
    let kty_ok = match fam {
        Family::Rsa => key.kty == "RSA",
        Family::Ec(crv) => key.kty == "EC" && key.crv.as_deref() == Some(crv),
        Family::Okp => key.kty == "OKP" && key.crv.as_deref() == Some("Ed25519"),
    };
    kty_ok && key.key_use.as_deref().is_none_or(|u| u == "sig") && key.alg.as_deref().is_none_or(|a| a == alg)
}

fn check(key: &Jwk, alg: &str, message: &[u8], sig: &[u8]) -> Result<(), JwsError> {
    let ok = match family(alg) {
        Some(Family::Rsa) => {
            let params: &'static signature::RsaParameters = match alg {
                "RS256" => &signature::RSA_PKCS1_2048_8192_SHA256,
                "RS384" => &signature::RSA_PKCS1_2048_8192_SHA384,
                "RS512" => &signature::RSA_PKCS1_2048_8192_SHA512,
                "PS256" => &signature::RSA_PSS_2048_8192_SHA256,
                "PS384" => &signature::RSA_PSS_2048_8192_SHA384,
                _ => &signature::RSA_PSS_2048_8192_SHA512,
            };
            let n = key_b64(key.n.as_ref(), "RSA modulus")?;
            let e = key_b64(key.e.as_ref(), "RSA exponent")?;
            RsaPublicKeyComponents { n: &n, e: &e }.verify(params, message, sig).is_ok()
        }
        Some(Family::Ec(crv)) => {
            let (params, size): (&'static signature::EcdsaVerificationAlgorithm, usize) = match crv {
                "P-256" => (&signature::ECDSA_P256_SHA256_FIXED, 32),
                _ => (&signature::ECDSA_P384_SHA384_FIXED, 48),
            };
            let x = key_b64(key.x.as_ref(), "EC x coordinate")?;
            let y = key_b64(key.y.as_ref(), "EC y coordinate")?;
            if x.len() != size || y.len() != size {
                return Err(JwsError::BadKey("EC coordinates of the wrong length"));
            }
            let mut point = Vec::with_capacity(1 + 2 * size);
            point.push(0x04);
            point.extend_from_slice(&x);
            point.extend_from_slice(&y);
            UnparsedPublicKey::new(params, &point).verify(message, sig).is_ok()
        }
        Some(Family::Okp) => {
            let x = key_b64(key.x.as_ref(), "Ed25519 public key")?;
            UnparsedPublicKey::new(&signature::ED25519, &x).verify(message, sig).is_ok()
        }
        None => return Err(JwsError::Algorithm(alg.to_owned())),
    };
    if ok { Ok(()) } else { Err(JwsError::BadSignature) }
}

/// Verifies `token` against `keys` and returns its payload (the claims).
/// Claims are not checked here: see [`super::oidc`].
pub fn verify(token: &str, keys: &JwkSet) -> Result<serde_json::Map<String, Value>, JwsError> {
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(JwsError::Malformed("expected three dot-separated parts"));
    };
    let header: Header =
        serde_json::from_slice(&b64(h, "header")?).map_err(|_| JwsError::Malformed("header is not a JOSE header"))?;
    if header.crit.is_some() {
        return Err(JwsError::Malformed("critical header extensions are not supported"));
    }
    let fam = family(&header.alg).ok_or_else(|| JwsError::Algorithm(header.alg.clone()))?;
    let sig = b64(s, "signature")?;
    let message = &token.as_bytes()[..h.len() + 1 + p.len()];

    let candidates: Vec<&Jwk> = keys
        .keys
        .iter()
        .filter(|k| usable(k, &header.alg, fam))
        .filter(|k| header.kid.is_none() || k.kid == header.kid)
        .collect();
    if candidates.is_empty() {
        return Err(JwsError::UnknownKey);
    }
    // Without a kid several keys may fit (during a rotation): any one that verifies will do.
    let mut last = JwsError::BadSignature;
    for key in candidates {
        match check(key, &header.alg, message, &sig) {
            Ok(()) => {
                let payload = b64(p, "payload")?;
                return match serde_json::from_slice(&payload) {
                    Ok(Value::Object(claims)) => Ok(claims),
                    _ => Err(JwsError::Malformed("payload is not a JSON object")),
                };
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

#[cfg(test)]
pub(crate) mod tests {
    use ring::rand::SystemRandom;
    use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, Ed25519KeyPair, KeyPair};
    use serde_json::json;

    use super::*;

    fn enc(v: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(v)
    }

    /// A fresh P-256 key: its JWK and a signer for `header.payload`.
    pub(crate) struct TestKey {
        pub jwk: Value,
        pair: EcdsaKeyPair,
    }

    impl TestKey {
        pub(crate) fn new(kid: &str) -> TestKey {
            let rng = SystemRandom::new();
            let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
            let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap();
            let point = pair.public_key().as_ref();
            let jwk = json!({
                "kty": "EC", "crv": "P-256", "kid": kid, "use": "sig", "alg": "ES256",
                "x": enc(&point[1..33]), "y": enc(&point[33..65]),
            });
            TestKey { jwk, pair }
        }

        pub(crate) fn sign(&self, header: Value, claims: &Value) -> String {
            let input = format!("{}.{}", enc(header.to_string().as_bytes()), enc(claims.to_string().as_bytes()));
            let sig = self.pair.sign(&SystemRandom::new(), input.as_bytes()).unwrap();
            format!("{input}.{}", enc(sig.as_ref()))
        }

        pub(crate) fn token(&self, claims: &Value) -> String {
            self.sign(json!({ "alg": "ES256", "kid": self.jwk["kid"], "typ": "JWT" }), claims)
        }
    }

    pub(crate) fn set(keys: &[&Value]) -> JwkSet {
        serde_json::from_value(json!({ "keys": keys })).unwrap()
    }

    #[test]
    fn a_valid_es256_token_verifies_and_returns_its_claims() {
        let key = TestKey::new("k1");
        let claims = json!({ "sub": "abc", "n": 1 });
        let got = verify(&key.token(&claims), &set(&[&key.jwk])).unwrap();
        assert_eq!(Value::Object(got), claims);
    }

    #[test]
    fn a_token_signed_by_another_key_is_refused() {
        let (good, evil) = (TestKey::new("k1"), TestKey::new("k1"));
        let token = evil.token(&json!({ "sub": "abc" }));
        assert_eq!(verify(&token, &set(&[&good.jwk])), Err(JwsError::BadSignature));
    }

    #[test]
    fn a_tampered_payload_is_refused() {
        let key = TestKey::new("k1");
        let token = key.token(&json!({ "sub": "abc" }));
        let parts: Vec<&str> = token.split('.').collect();
        let forged = format!("{}.{}.{}", parts[0], enc(br#"{"sub":"admin"}"#), parts[2]);
        assert_eq!(verify(&forged, &set(&[&key.jwk])), Err(JwsError::BadSignature));
    }

    #[test]
    fn none_and_hmac_are_refused() {
        let key = TestKey::new("k1");
        let claims = enc(br#"{"sub":"abc"}"#);
        for alg in ["none", "HS256", "HS512", "ES512"] {
            let header = enc(json!({ "alg": alg, "kid": "k1" }).to_string().as_bytes());
            let token = format!("{header}.{claims}.");
            assert_eq!(verify(&token, &set(&[&key.jwk])), Err(JwsError::Algorithm(alg.into())), "{alg}");
        }
    }

    #[test]
    fn the_key_must_match_the_algorithm_and_kid() {
        let key = TestKey::new("k1");
        let claims = json!({ "sub": "abc" });
        // A key published for another algorithm does not verify ES256.
        let mut other_alg = key.jwk.clone();
        other_alg["alg"] = json!("ES384");
        assert_eq!(verify(&key.token(&claims), &set(&[&other_alg])), Err(JwsError::UnknownKey));
        // An encryption key is not a signing key.
        let mut enc_key = key.jwk.clone();
        enc_key["use"] = json!("enc");
        assert_eq!(verify(&key.token(&claims), &set(&[&enc_key])), Err(JwsError::UnknownKey));
        // An unknown kid: the caller may refresh the key set.
        let token = key.sign(json!({ "alg": "ES256", "kid": "k2" }), &claims);
        assert_eq!(verify(&token, &set(&[&key.jwk])), Err(JwsError::UnknownKey));
        // No kid: every fitting key is tried.
        let token = key.sign(json!({ "alg": "ES256" }), &claims);
        let spare = TestKey::new("k0");
        assert!(verify(&token, &set(&[&spare.jwk, &key.jwk])).is_ok());
        // RSA header against an EC key.
        let token = key.sign(json!({ "alg": "RS256", "kid": "k1" }), &claims);
        assert_eq!(verify(&token, &set(&[&key.jwk])), Err(JwsError::UnknownKey));
    }

    #[test]
    fn crit_and_malformed_tokens_are_refused() {
        let key = TestKey::new("k1");
        let token = key.sign(json!({ "alg": "ES256", "kid": "k1", "crit": ["exp"] }), &json!({}));
        assert!(matches!(verify(&token, &set(&[&key.jwk])), Err(JwsError::Malformed(_))));
        for bad in ["", "a.b", "a.b.c.d", "!!.e30.", "e30.e30.e30"] {
            assert!(verify(bad, &set(&[&key.jwk])).is_err(), "{bad}");
        }
        let token = key.sign(json!({ "alg": "ES256", "kid": "k1" }), &json!(["not", "an", "object"]));
        assert_eq!(verify(&token, &set(&[&key.jwk])), Err(JwsError::Malformed("payload is not a JSON object")));
    }

    #[test]
    fn ed25519_verifies() {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let jwk = json!({ "kty": "OKP", "crv": "Ed25519", "kid": "ed", "x": enc(pair.public_key().as_ref()) });
        let input = format!(
            "{}.{}",
            enc(json!({ "alg": "EdDSA", "kid": "ed" }).to_string().as_bytes()),
            enc(br#"{"sub":"x"}"#)
        );
        let token = format!("{input}.{}", enc(pair.sign(input.as_bytes()).as_ref()));
        assert!(verify(&token, &set(&[&jwk])).is_ok());
    }

    /// RS256 against a published RSA key (ring cannot generate RSA keys): the
    /// fixture in testdata was signed once with a throwaway 2048-bit key whose
    /// private half was not kept.
    #[test]
    fn rs256_verifies_against_a_published_key() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../../testdata/oidc/rs256.json")).expect("rs256 fixture");
        let keys: JwkSet = serde_json::from_value(fixture["jwks"].clone()).unwrap();
        let segments: Vec<&str> =
            fixture["token_segments"].as_array().unwrap().iter().map(|s| s.as_str().unwrap()).collect();
        let token = segments.join(".");
        let claims = verify(&token, &keys).expect("RS256 token verifies");
        assert_eq!(claims["sub"], "rsa-subject");
        let mut forged = token.to_owned();
        forged.pop();
        forged.push(if token.ends_with('A') { 'B' } else { 'A' });
        assert!(verify(&forged, &keys).is_err());
    }
}
