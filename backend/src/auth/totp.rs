//! Time-based one-time passwords (RFC 6238 over RFC 4226 HOTP) and recovery codes.
//!
//! The parameters are the ones every authenticator app assumes: HMAC-SHA1, a
//! 160-bit secret, 6 digits, 30-second steps. A code is accepted for the
//! current step and one step either side (clock drift), and only for a step
//! later than the last accepted one, so a code cannot be replayed.
//!
//! Recovery codes are 16 characters of base32 (80 random bits), shown as
//! `xxxx-xxxx-xxxx-xxxx`; case, dashes and spaces do not matter when typed.

use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit, Mac};
use password_hash::rand_core::{OsRng, RngCore};
use sha1::Sha1;
use sha2::{Digest, Sha256};

pub const SECRET_BYTES: usize = 20;
pub const DIGITS: usize = 6;
pub const STEP_SECONDS: u64 = 30;
/// Steps accepted either side of the current one.
const DRIFT_STEPS: i64 = 1;
pub const RECOVERY_CODES: usize = 10;
const RECOVERY_CHARS: usize = 16;
pub const ISSUER: &str = "ShadouCMDB";

const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

pub fn new_secret() -> Vec<u8> {
    let mut bytes = vec![0u8; SECRET_BYTES];
    OsRng.fill_bytes(&mut bytes);
    bytes
}

/// RFC 4648 base32 without padding, the form authenticator apps take.
pub fn base32(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &b in bytes {
        buffer = (buffer << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(BASE32[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(BASE32[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// `otpauth://totp/...`, the URI a QR code carries.
pub fn otpauth_uri(account: &str, secret: &[u8]) -> String {
    let label = format!("{ISSUER}:{account}");
    let mut uri = url::Url::parse("otpauth://totp/").expect("static URL");
    uri.set_path(&label);
    uri.query_pairs_mut()
        .append_pair("secret", &base32(secret))
        .append_pair("issuer", ISSUER)
        .append_pair("algorithm", "SHA1")
        .append_pair("digits", &DIGITS.to_string())
        .append_pair("period", &STEP_SECONDS.to_string());
    uri.to_string()
}

/// The HOTP value for one counter (RFC 4226 section 5.3).
fn hotp(secret: &[u8], counter: u64) -> u32 {
    let mut mac = Hmac::<Sha1>::new_from_slice(secret).expect("HMAC takes any key length");
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[digest.len() - 1] & 0x0f) as usize;
    let value = u32::from_be_bytes([digest[offset], digest[offset + 1], digest[offset + 2], digest[offset + 3]]);
    (value & 0x7fff_ffff) % 10u32.pow(DIGITS as u32)
}

pub fn current_step() -> i64 {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    (now / STEP_SECONDS) as i64
}

/// The code an authenticator shows for `step` (tests act as the app).
#[cfg(test)]
pub fn code_at(secret: &[u8], step: i64) -> String {
    format!("{:0width$}", hotp(secret, step as u64), width = DIGITS)
}

/// Whether `input` has the shape of a TOTP code (6 digits, spaces ignored).
pub fn looks_like_code(input: &str) -> bool {
    let digits: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    digits.len() == DIGITS && digits.bytes().all(|b| b.is_ascii_digit())
}

/// The step `input` is valid for, near `now_step` and after `last_used`; None if it matches none.
pub fn verify(secret: &[u8], input: &str, now_step: i64, last_used: Option<i64>) -> Option<i64> {
    if !looks_like_code(input) {
        return None;
    }
    let code: u32 = input.chars().filter(|c| !c.is_whitespace()).collect::<String>().parse().ok()?;
    // Every candidate is computed, so the time taken does not say which step matched.
    let mut matched = None;
    for step in now_step - DRIFT_STEPS..=now_step + DRIFT_STEPS {
        if step >= 0 && hotp(secret, step as u64) == code && last_used.is_none_or(|l| step > l) {
            matched = matched.or(Some(step));
        }
    }
    matched
}

/// Ten fresh recovery codes, formatted for display.
pub fn new_recovery_codes() -> Vec<String> {
    (0..RECOVERY_CODES)
        .map(|_| {
            let mut bytes = [0u8; RECOVERY_CHARS];
            OsRng.fill_bytes(&mut bytes);
            let chars: Vec<char> =
                bytes.iter().map(|b| BASE32[(b & 31) as usize].to_ascii_lowercase() as char).collect();
            chars.chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join("-")
        })
        .collect()
}

/// A typed recovery code in canonical form (lower case, no dashes or spaces),
/// or None if it cannot be one.
pub fn normalise_recovery_code(input: &str) -> Option<String> {
    let s: String = input.chars().filter(|c| *c != '-' && !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    let valid = s.len() == RECOVERY_CHARS && s.bytes().all(|b| BASE32.contains(&b.to_ascii_uppercase()));
    valid.then_some(s)
}

/// SHA-256 of a recovery code's canonical form.
pub fn recovery_code_hash(canonical: &str) -> Vec<u8> {
    Sha256::digest(canonical.as_bytes()).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238 appendix B, SHA-1 key, truncated to 6 digits.
    #[test]
    fn matches_the_rfc_6238_test_vectors() {
        let secret = b"12345678901234567890";
        for (time, expected) in
            [(59u64, 287082u32), (1111111109, 81804), (1111111111, 50471), (1234567890, 5924), (2000000000, 279037)]
        {
            assert_eq!(hotp(secret, time / STEP_SECONDS), expected, "t={time}");
        }
    }

    #[test]
    fn base32_encodes_like_rfc_4648() {
        assert_eq!(base32(b""), "");
        assert_eq!(base32(b"f"), "MY");
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32(&new_secret()).len(), 32);
    }

    #[test]
    fn accepts_drift_of_one_step_and_refuses_replays() {
        let secret = new_secret();
        let now = 1_000_000;
        let code = |step: i64| format!("{:06}", hotp(&secret, step as u64));
        assert_eq!(verify(&secret, &code(now), now, None), Some(now));
        assert_eq!(verify(&secret, &code(now - 1), now, None), Some(now - 1));
        assert_eq!(verify(&secret, &code(now + 1), now, None), Some(now + 1));
        assert_eq!(verify(&secret, &code(now - 2), now, None), None, "too old");
        assert_eq!(verify(&secret, &code(now), now, Some(now)), None, "already used");
        assert_eq!(verify(&secret, &code(now - 1), now, Some(now - 1)), None);
        assert_eq!(verify(&secret, &code(now + 1), now, Some(now)), Some(now + 1));
        let spaced = format!("{} {}", &code(now)[..3], &code(now)[3..]);
        assert_eq!(verify(&secret, &spaced, now, None), Some(now), "spaces are ignored");
        assert_eq!(verify(&secret, "12345", now, None), None);
        assert_eq!(verify(&secret, "abcdef", now, None), None);
    }

    #[test]
    fn recovery_codes_are_random_and_normalise() {
        let codes = new_recovery_codes();
        assert_eq!(codes.len(), RECOVERY_CODES);
        let unique: std::collections::HashSet<_> = codes.iter().collect();
        assert_eq!(unique.len(), RECOVERY_CODES);
        for c in &codes {
            assert_eq!(c.len(), 19);
            let canonical = normalise_recovery_code(c).expect("valid");
            assert_eq!(normalise_recovery_code(&c.to_uppercase().replace('-', " ")), Some(canonical.clone()));
            assert_eq!(recovery_code_hash(&canonical).len(), 32);
        }
        assert_eq!(normalise_recovery_code("123456"), None);
        assert_eq!(normalise_recovery_code("abcd-efgh-ijkl-mno1"), None, "1 is not base32");
    }

    #[test]
    fn otpauth_uri_carries_issuer_account_and_secret() {
        let uri = otpauth_uri("alice@example.com", b"12345678901234567890");
        assert!(
            uri.starts_with("otpauth://totp/ShadouCMDB:alice@example.com?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&"),
            "{uri}"
        );
        assert!(uri.contains("issuer=ShadouCMDB") && uri.contains("digits=6") && uri.contains("period=30"));
    }
}
