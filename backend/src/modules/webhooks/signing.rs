//! Endpoint signing secrets and the signature header (design SHAA-2725 §5.4).
//!
//! A secret is 32 random bytes, shown once as `whsec_` and their unpadded
//! base64url. The receiver keys HMAC-SHA256 with that whole string, as shown:
//!
//! ```text
//! X-ShadouCMDB-Signature: t=1760000000,v1=<hex HMAC-SHA256(secret, t + "." + raw body)>
//! ```
//!
//! During the grace period after a rotation the header carries a second `v1=`
//! made with the previous secret, so a receiver can switch without downtime.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::secrets::Secret;

pub const PREFIX: &str = "whsec_";
const SECRET_BYTES: usize = 32;
/// How far `t` may be from the receiver's clock, in the documented recipe.
#[cfg(test)]
pub const TOLERANCE_SECS: i64 = 300;

/// 32 fresh random bytes.
pub fn generate() -> Secret {
    let mut bytes = vec![0u8; SECRET_BYTES];
    getrandom::fill(&mut bytes).expect("OS random number generator");
    Secret::new(bytes)
}

/// The form shown to the administrator and used as the HMAC key.
pub fn display(raw: &[u8]) -> String {
    format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(raw))
}

fn mac(secret: &str, t: i64, body: &[u8]) -> String {
    let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).expect("HMAC takes any key length");
    m.update(t.to_string().as_bytes());
    m.update(b".");
    m.update(body);
    hex(&m.finalize().into_bytes())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The `X-ShadouCMDB-Signature` value for `body` at `t`, with one `v1=` per
/// secret (raw bytes): the current one first, then the previous one.
pub fn header(t: i64, body: &[u8], secrets: &[&[u8]]) -> String {
    let mut out = format!("t={t}");
    for s in secrets {
        out.push_str(",v1=");
        out.push_str(&mac(&display(s), t, body));
    }
    out
}

/// The receiver's side, as documented: whether `header` holds a valid `v1`
/// for `body` under `secret` (the `whsec_...` string) with `t` within
/// [`TOLERANCE_SECS`] of `now`.
#[cfg(test)]
pub fn verify(secret: &str, header: &str, body: &[u8], now: i64) -> bool {
    let mut t = None;
    let mut sigs = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", v)) => t = v.parse::<i64>().ok(),
            Some(("v1", v)) => sigs.push(v),
            _ => {}
        }
    }
    let Some(t) = t else { return false };
    if (now - t).abs() > TOLERANCE_SECS {
        return false;
    }
    let expected = mac(secret, t, body);
    // Constant time per comparison.
    sigs.iter()
        .any(|s| s.len() == expected.len() && s.bytes().zip(expected.bytes()).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_verifies_with_the_documented_recipe() {
        let raw = [7u8; 32];
        let shown = display(&raw);
        assert!(shown.starts_with("whsec_") && shown.len() == 6 + 43, "{shown}");
        let body = br#"{"specVersion":"1"}"#;
        let h = header(1_760_000_000, body, &[&raw]);
        assert!(verify(&shown, &h, body, 1_760_000_100));
        assert!(!verify(&shown, &h, b"{}", 1_760_000_100), "another body");
        assert!(!verify(&shown, &h, body, 1_760_000_400), "t too old");
        assert!(!verify(&display(&[8u8; 32]), &h, body, 1_760_000_000), "another secret");
        // The recipe in plain terms: HMAC-SHA256 keyed with the whsec_ string over "t.body".
        let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(shown.as_bytes()).unwrap();
        m.update(b"1760000000.");
        m.update(body);
        assert_eq!(h, format!("t=1760000000,v1={}", hex(&m.finalize().into_bytes())));
    }

    #[test]
    fn both_secrets_verify_during_the_grace_period() {
        let (new, old) = ([1u8; 32], [2u8; 32]);
        let body = b"payload";
        let h = header(100, body, &[&new, &old]);
        assert_eq!(h.matches("v1=").count(), 2);
        assert!(verify(&display(&new), &h, body, 100));
        assert!(verify(&display(&old), &h, body, 100));
        assert!(!verify(&display(&[3u8; 32]), &h, body, 100));
        assert_ne!(display(&generate()), display(&generate()));
    }
}
