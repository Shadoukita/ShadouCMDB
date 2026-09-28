//! A pending OIDC sign-in, sealed into the `shadoucmdb_oidc` cookie (GH#122).
//!
//! Starting a sign-in is anonymous, so it stores nothing on the server: the
//! provider, `state`, `nonce`, PKCE verifier, return path and expiry travel in
//! the cookie, encrypted and authenticated with AES-256-GCM under a key from
//! `server_keys` (purpose `oidc_state`). The browser can neither read nor
//! change them; the callback opens the cookie and checks it.
//!
//! Wire value: `base64url_nopad(key_id || nonce(12) || ciphertext || tag)`,
//! with `shadoucmdb_oidc/v1` and the key id as additional authenticated data.
//! Plaintext (big-endian):
//!
//! ```text
//! version(1) = 1 | provider_id(16) | exp(8, unix seconds)
//! | state | nonce | code_verifier | return_to     each: length(2) || bytes
//! ```
//!
//! An empty `return_to` means none (a real one always starts with `/`).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use sqlx::PgPool;
use uuid::Uuid;

use crate::data::server_keys;

/// The `server_keys` row the sealing key lives in.
pub const KEY_PURPOSE: &str = "oidc_state";
/// Id of the key a fresh install generates.
const FIRST_KEY_ID: u8 = 1;
const AAD_PREFIX: &[u8] = b"shadoucmdb_oidc/v1";
const VERSION: u8 = 1;
const TAG_LEN: usize = 16;

/// What the start route hands to the callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginState {
    pub provider_id: Uuid,
    pub state: String,
    pub nonce: String,
    pub code_verifier: String,
    pub return_to: Option<String>,
    /// Unix seconds after which the callback refuses it.
    pub exp: i64,
}

/// The key sealing [`LoginState`]s.
pub struct SealingKey {
    id: u8,
    key: LessSafeKey,
}

impl std::fmt::Debug for SealingKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealingKey").field("id", &self.id).finish_non_exhaustive()
    }
}

impl SealingKey {
    /// `None` unless `secret` is 32 bytes.
    pub fn new(id: u8, secret: &[u8]) -> Option<Self> {
        Some(SealingKey { id, key: LessSafeKey::new(UnboundKey::new(&AES_256_GCM, secret).ok()?) })
    }

    fn aad(id: u8) -> [u8; AAD_PREFIX.len() + 1] {
        let mut aad = [0u8; AAD_PREFIX.len() + 1];
        aad[..AAD_PREFIX.len()].copy_from_slice(AAD_PREFIX);
        aad[AAD_PREFIX.len()] = id;
        aad
    }

    /// The cookie value carrying `s`. A fresh random nonce every time.
    pub fn seal(&self, s: &LoginState) -> String {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).expect("OS random number generator");
        let mut plain = Vec::with_capacity(64 + s.state.len() + s.nonce.len() + s.code_verifier.len() + 2048);
        plain.push(VERSION);
        plain.extend_from_slice(s.provider_id.as_bytes());
        plain.extend_from_slice(&s.exp.to_be_bytes());
        for field in [&s.state, &s.nonce, &s.code_verifier, s.return_to.as_deref().unwrap_or_default()] {
            let len = u16::try_from(field.len()).expect("sign-in state fields are short");
            plain.extend_from_slice(&len.to_be_bytes());
            plain.extend_from_slice(field.as_bytes());
        }
        self.key
            .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(Self::aad(self.id)), &mut plain)
            .expect("AES-GCM sealing a short message");
        let mut wire = Vec::with_capacity(1 + NONCE_LEN + plain.len());
        wire.push(self.id);
        wire.extend_from_slice(&nonce);
        wire.extend_from_slice(&plain);
        URL_SAFE_NO_PAD.encode(wire)
    }

    /// The state in a cookie value, if this key sealed it and it has not
    /// expired at `now` (unix seconds). Every failure looks the same.
    pub fn open(&self, value: &str, now: i64) -> Option<LoginState> {
        let wire = URL_SAFE_NO_PAD.decode(value).ok()?;
        let (&id, rest) = wire.split_first()?;
        if id != self.id || rest.len() < NONCE_LEN + TAG_LEN {
            return None;
        }
        let (nonce, sealed) = rest.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
        let mut sealed = sealed.to_vec();
        let plain = self.key.open_in_place(nonce, Aad::from(Self::aad(id)), &mut sealed).ok()?;
        let s = decode(plain)?;
        (s.exp >= now).then_some(s)
    }
}

fn decode(plain: &[u8]) -> Option<LoginState> {
    let (&version, rest) = plain.split_first()?;
    if version != VERSION || rest.len() < 24 {
        return None;
    }
    let (provider_id, rest) = rest.split_at(16);
    let (exp, mut rest) = rest.split_at(8);
    let mut field = || -> Option<String> {
        let len = usize::from(u16::from_be_bytes(rest.get(..2)?.try_into().ok()?));
        let bytes = rest.get(2..2 + len)?;
        rest = &rest[2 + len..];
        String::from_utf8(bytes.to_vec()).ok()
    };
    let (state, nonce, code_verifier, return_to) = (field()?, field()?, field()?, field()?);
    if !rest.is_empty() {
        return None;
    }
    Some(LoginState {
        provider_id: Uuid::from_slice(provider_id).ok()?,
        state,
        nonce,
        code_verifier,
        return_to: Some(return_to).filter(|r| !r.is_empty()),
        exp: i64::from_be_bytes(exp.try_into().ok()?),
    })
}

/// The sealing key every API process shares: read from `server_keys`, and
/// generated there by whichever process needs it first.
pub async fn load_or_create_key(pool: &PgPool) -> sqlx::Result<SealingKey> {
    if server_keys::get(pool, KEY_PURPOSE).await?.is_none() {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).expect("OS random number generator");
        server_keys::insert_if_absent(pool, KEY_PURPOSE, i16::from(FIRST_KEY_ID), &secret).await?;
    }
    let (id, secret) = server_keys::get(pool, KEY_PURPOSE)
        .await?
        .ok_or_else(|| sqlx::Error::Protocol("server key oidc_state vanished after it was stored".into()))?;
    let unusable = || sqlx::Error::Protocol("server key oidc_state is not a one-byte id and 32 bytes".into());
    SealingKey::new(u8::try_from(id).map_err(|_| unusable())?, &secret).ok_or_else(unusable)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn key(id: u8, fill: u8) -> SealingKey {
        SealingKey::new(id, &[fill; 32]).unwrap()
    }

    fn sample(return_to: Option<&str>) -> LoginState {
        LoginState {
            provider_id: Uuid::from_u128(0x1234),
            state: crate::auth::sso::oidc::random_value(),
            nonce: crate::auth::sso::oidc::random_value(),
            code_verifier: crate::auth::sso::oidc::random_value(),
            return_to: return_to.map(str::to_owned),
            exp: NOW + 600,
        }
    }

    #[test]
    fn round_trip() {
        let k = key(1, 7);
        for return_to in [None, Some("/"), Some("/items?q=a&b=\"c\"")] {
            let s = sample(return_to);
            assert_eq!(k.open(&k.seal(&s), NOW), Some(s));
        }
    }

    #[test]
    fn every_seal_differs() {
        let (k, s) = (key(1, 7), sample(None));
        assert_ne!(k.seal(&s), k.seal(&s));
    }

    #[test]
    fn longest_return_to_fits_a_cookie() {
        let k = key(1, 7);
        // Quotes would double in JSON; the binary layout keeps them one byte.
        let s = sample(Some(&format!("/{}", "\"".repeat(2047))));
        let value = k.seal(&s);
        let header = crate::auth::session::oidc_cookie(true, &value, std::time::Duration::from_secs(600));
        assert!(header.len() < 4096, "{} bytes", header.len());
        assert_eq!(k.open(&value, NOW), Some(s));
    }

    #[test]
    fn refuses_anything_it_did_not_seal_unchanged() {
        let k = key(1, 7);
        let good = k.seal(&sample(Some("/items")));
        let wire = URL_SAFE_NO_PAD.decode(&good).unwrap();
        for i in 0..wire.len() {
            let mut w = wire.clone();
            w[i] ^= 0x01;
            assert_eq!(k.open(&URL_SAFE_NO_PAD.encode(&w), NOW), None, "byte {i} flipped");
        }
        for len in [0, 1, 13, 29, wire.len() - 1] {
            assert_eq!(k.open(&URL_SAFE_NO_PAD.encode(&wire[..len]), NOW), None, "truncated to {len}");
        }
        assert_eq!(k.open("not base64!", NOW), None);
        assert_eq!(k.open(&format!("{good}="), NOW), None);
        // Another key, with the same or another id.
        assert_eq!(key(1, 8).open(&good, NOW), None);
        assert_eq!(key(2, 7).open(&good, NOW), None);
    }

    #[test]
    fn refuses_expired_state() {
        let k = key(1, 7);
        let s = sample(None);
        let value = k.seal(&s);
        assert!(k.open(&value, s.exp).is_some());
        assert_eq!(k.open(&value, s.exp + 1), None);
    }

    #[test]
    fn refuses_other_versions_and_trailing_bytes() {
        let k = key(1, 7);
        let reseal = |edit: &dyn Fn(&mut Vec<u8>)| {
            let wire = URL_SAFE_NO_PAD.decode(k.seal(&sample(None))).unwrap();
            let (nonce, sealed) = (&wire[1..1 + NONCE_LEN], &wire[1 + NONCE_LEN..]);
            let mut plain = sealed.to_vec();
            let n = Nonce::try_assume_unique_for_key(nonce).unwrap();
            let len = k.key.open_in_place(n, Aad::from(SealingKey::aad(1)), &mut plain).unwrap().len();
            plain.truncate(len);
            edit(&mut plain);
            let mut fresh = [0u8; NONCE_LEN];
            getrandom::fill(&mut fresh).unwrap();
            k.key
                .seal_in_place_append_tag(
                    Nonce::assume_unique_for_key(fresh),
                    Aad::from(SealingKey::aad(1)),
                    &mut plain,
                )
                .unwrap();
            URL_SAFE_NO_PAD.encode([&[1u8][..], &fresh, &plain].concat())
        };
        assert!(k.open(&reseal(&|_| {}), NOW).is_some());
        assert_eq!(k.open(&reseal(&|p| p[0] = 2), NOW), None);
        assert_eq!(k.open(&reseal(&|p| p.push(0)), NOW), None);
    }
}
