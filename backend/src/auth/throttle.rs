//! Login backoff per username.
//!
//! The first [`FREE_FAILURES`] - 1 wrong passwords cost nothing. From then on
//! every failure locks the username for 1 s, 2 s, 4 s, ... up to
//! [`MAX_LOCK`]; while locked, login answers 429 without checking the
//! password. A success clears the counter. Unknown usernames are throttled
//! exactly like real ones, so the lock does not reveal which names exist.
//!
//! The state is in memory: it resets on restart and is per process. That is
//! enough to make online guessing impractical (a few attempts per hour once
//! the lock is at its maximum) without a table that attackers could fill.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const FREE_FAILURES: u32 = 5;
pub const MAX_LOCK: Duration = Duration::from_secs(15 * 60);
/// A username with no failure for this long starts over.
const FORGET_AFTER: Duration = Duration::from_secs(60 * 60);
/// Upper bound on tracked usernames.
const MAX_ENTRIES: usize = 10_000;

#[derive(Debug, Clone, Copy)]
struct Entry {
    failures: u32,
    last_failure: Instant,
    locked_until: Option<Instant>,
}

#[derive(Default)]
pub struct LoginThrottle {
    entries: Mutex<HashMap<String, Entry>>,
}

fn key(username: &str) -> String {
    username.trim().to_lowercase()
}

pub fn lock_for(failures: u32) -> Option<Duration> {
    if failures < FREE_FAILURES {
        return None;
    }
    let exp = (failures - FREE_FAILURES).min(20);
    Some(Duration::from_secs(1u64 << exp).min(MAX_LOCK))
}

impl LoginThrottle {
    /// `Some(wait)` when the username is locked right now.
    pub fn check(&self, username: &str) -> Option<Duration> {
        self.check_at(username, Instant::now())
    }

    fn check_at(&self, username: &str, now: Instant) -> Option<Duration> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let until = entries.get(&key(username))?.locked_until?;
        (until > now).then(|| until - now)
    }

    /// Records a failed attempt; returns the lock it triggered, if any.
    pub fn failure(&self, username: &str) -> Option<Duration> {
        self.failure_at(username, Instant::now())
    }

    fn failure_at(&self, username: &str, now: Instant) -> Option<Duration> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let k = key(username);
        if !entries.contains_key(&k) && entries.len() >= MAX_ENTRIES {
            entries.retain(|_, e| now.duration_since(e.last_failure) < FORGET_AFTER);
            if entries.len() >= MAX_ENTRIES {
                return None;
            }
        }
        let e = entries.entry(k).or_insert(Entry { failures: 0, last_failure: now, locked_until: None });
        if now.duration_since(e.last_failure) >= FORGET_AFTER {
            e.failures = 0;
        }
        e.failures = e.failures.saturating_add(1);
        e.last_failure = now;
        let lock = lock_for(e.failures);
        e.locked_until = lock.map(|d| now + d);
        lock
    }

    pub fn success(&self, username: &str) {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).remove(&key(username));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_is_capped() {
        assert_eq!(lock_for(FREE_FAILURES - 1), None);
        assert_eq!(lock_for(FREE_FAILURES), Some(Duration::from_secs(1)));
        assert_eq!(lock_for(FREE_FAILURES + 1), Some(Duration::from_secs(2)));
        assert_eq!(lock_for(FREE_FAILURES + 3), Some(Duration::from_secs(8)));
        assert_eq!(lock_for(FREE_FAILURES + 30), Some(MAX_LOCK));
        assert_eq!(lock_for(u32::MAX), Some(MAX_LOCK));
    }

    #[test]
    fn locks_after_the_free_failures_case_insensitively_and_resets_on_success() {
        let t = LoginThrottle::default();
        let now = Instant::now();
        for _ in 1..FREE_FAILURES {
            assert_eq!(t.failure_at("Alice", now), None);
        }
        assert_eq!(t.check_at("alice", now), None);
        assert_eq!(t.failure_at("ALICE", now), Some(Duration::from_secs(1)));
        assert!(t.check_at(" alice ", now).is_some());
        assert_eq!(t.check_at("alice", now + Duration::from_secs(2)), None, "lock expires");
        assert_eq!(t.check_at("bob", now), None, "other users are unaffected");
        assert_eq!(t.failure_at("alice", now + Duration::from_secs(2)), Some(Duration::from_secs(2)));
        t.success("alice");
        assert_eq!(t.check_at("alice", now + Duration::from_secs(2)), None);
        assert_eq!(t.failure_at("alice", now + Duration::from_secs(3)), None, "counter starts over");
    }

    #[test]
    fn old_failures_are_forgotten() {
        let t = LoginThrottle::default();
        let now = Instant::now();
        for _ in 0..FREE_FAILURES + 2 {
            t.failure_at("carol", now);
        }
        assert_eq!(t.failure_at("carol", now + FORGET_AFTER), None);
    }
}
