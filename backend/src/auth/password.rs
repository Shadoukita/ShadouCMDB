//! Password hashing with argon2id and the password policy.
//!
//! Hashing is deliberately expensive (~19 MiB and tens of milliseconds), so it
//! runs on the blocking pool and at most [`MAX_CONCURRENT`] hashes run at once:
//! a burst of login attempts cannot exhaust memory or starve the runtime.

use std::sync::LazyLock;

use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use tokio::sync::Semaphore;

use crate::http::error::AppError;

pub const MIN_LENGTH: usize = 12;
/// Bounds the work an attacker can make the server do per attempt.
pub const MAX_LENGTH: usize = 256;

const MAX_CONCURRENT: usize = 4;
static HASHING: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(MAX_CONCURRENT));

/// Verified against when the username does not exist, so the response time
/// does not reveal which usernames are real.
static DUMMY_HASH: LazyLock<String> =
    LazyLock::new(|| hash_blocking("dummy password, never matches").unwrap_or_default());

#[cfg(test)]
thread_local! {
    /// Verifications against [`DUMMY_HASH`] on this thread (tests run on one).
    pub static DUMMY_VERIFIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Problems with a new password, as a message for the `password` field.
pub fn policy_error(password: &str) -> Option<String> {
    let len = password.chars().count();
    if len < MIN_LENGTH {
        return Some(format!("Must be at least {MIN_LENGTH} characters"));
    }
    if len > MAX_LENGTH {
        return Some(format!("Must be at most {MAX_LENGTH} characters"));
    }
    if password.trim().is_empty() {
        return Some("Must not be only whitespace".into());
    }
    None
}

fn hash_blocking(password: &str) -> Result<String, argon2::password_hash::Error> {
    // A fresh 16-byte salt from the OS generator.
    Ok(Argon2::default().hash_password(password.as_bytes())?.to_string())
}

/// False for a wrong password and for a hash that is not a valid PHC string.
fn verify_blocking(password: &str, hash: &str) -> bool {
    Argon2::default().verify_password(password.as_bytes(), hash).is_ok()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, AppError> {
    let _permit = HASHING.acquire().await.map_err(|_| AppError::internal())?;
    tokio::task::spawn_blocking(f).await.map_err(|_| AppError::internal())
}

/// A PHC string: `$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`.
pub async fn hash(password: &str) -> Result<String, AppError> {
    let password = password.to_owned();
    blocking(move || hash_blocking(&password)).await?.map_err(|err| {
        tracing::error!(error = %err, "password hashing failed");
        AppError::internal()
    })
}

/// Checks a password against a stored hash; `None` (unknown user) costs the same and fails.
pub async fn verify(password: &str, hash: Option<&str>) -> Result<bool, AppError> {
    let password = password.to_owned();
    let known = hash.is_some();
    #[cfg(test)]
    if !known {
        DUMMY_VERIFIES.with(|n| n.set(n.get() + 1));
    }
    let hash = hash.map(str::to_owned).unwrap_or_else(|| DUMMY_HASH.clone());
    let ok = blocking(move || verify_blocking(&password, &hash)).await?;
    Ok(ok && known)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy() {
        assert!(policy_error("short").is_some());
        assert!(policy_error(&" ".repeat(20)).is_some());
        assert!(policy_error(&"x".repeat(MAX_LENGTH + 1)).is_some());
        assert!(policy_error("correct horse battery").is_none());
    }

    #[tokio::test]
    async fn hashes_are_argon2id_salted_and_verify() {
        let a = hash("correct horse battery").await.unwrap();
        let b = hash("correct horse battery").await.unwrap();
        assert!(a.starts_with("$argon2id$v=19$"), "{a}");
        assert_ne!(a, b, "a fresh salt per hash");
        assert!(verify("correct horse battery", Some(&a)).await.unwrap());
        assert!(!verify("wrong horse battery", Some(&a)).await.unwrap());
        assert!(!verify("correct horse battery", None).await.unwrap());
        assert!(!verify("x", Some("not a phc string")).await.unwrap());
    }

    #[tokio::test]
    async fn verifies_hashes_stored_by_argon2_0_5() {
        // A password hashed by argon2 0.5.3: passwords stored before the 0.6 upgrade must
        // still sign in. Hash and password live in testdata/ so neither is a source literal.
        let testdata = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/");
        let stored = std::fs::read_to_string(format!("{testdata}argon2-0.5.3.phc")).unwrap();
        let password = std::fs::read_to_string(format!("{testdata}argon2-0.5.3.password")).unwrap();
        let (stored, password) = (stored.trim(), password.trim());
        assert!(verify(password, Some(stored)).await.unwrap());
        assert!(!verify(&format!("{password}!"), Some(stored)).await.unwrap());
    }
}
