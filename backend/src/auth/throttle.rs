//! Backoff for password guessing, per key and process-wide.
//!
//! Per key (the username for login, the user id for changing one's own
//! password): the first [`FREE_FAILURES`] - 1 wrong passwords cost nothing.
//! From then on every failure locks the key for 1 s, 2 s, 4 s, ... up to
//! [`MAX_LOCK`]; while locked, the endpoint answers 429 without checking the
//! password. A success clears the counter. Unknown usernames are throttled
//! exactly like real ones, so the lock does not reveal which names exist.
//!
//! Process-wide (login only): more than [`GLOBAL_BUDGET`] failures for any
//! usernames within [`GLOBAL_WINDOW`] lock sign-in for everyone until the
//! window has room again. That caps password spraying (one or two guesses
//! each for many usernames), which the per-username lock does not see, and it
//! caps how fast anyone can churn the per-username table.
//!
//! The per-key table is bounded by [`MAX_ENTRIES`]: when it is full, the key
//! with the oldest failure is evicted. Evicting never disables throttling:
//! filling the table takes more failures than the global budget allows.
//!
//! The state is in memory: it resets on restart and is per process. That is
//! enough to make online guessing impractical (a few attempts per hour per
//! account once its lock is at the maximum, and at most [`GLOBAL_BUDGET`]
//! per [`GLOBAL_WINDOW`] across all accounts).

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const FREE_FAILURES: u32 = 5;
pub const MAX_LOCK: Duration = Duration::from_secs(15 * 60);
/// Failures, for all keys together, that fit in [`GLOBAL_WINDOW`].
pub const GLOBAL_BUDGET: usize = 300;
pub const GLOBAL_WINDOW: Duration = Duration::from_secs(10 * 60);
/// A key with no failure for this long starts over.
const FORGET_AFTER: Duration = Duration::from_secs(60 * 60);
/// Upper bound on tracked keys.
const MAX_ENTRIES: usize = 10_000;

#[derive(Debug, Clone, Copy)]
struct Entry {
    failures: u32,
    last_failure: Instant,
    locked_until: Option<Instant>,
}

/// Why an attempt is refused, and for how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locked {
    /// Too many failures for this key.
    Key(Duration),
    /// Too many failures overall.
    Everyone(Duration),
}

impl Locked {
    pub fn wait(self) -> Duration {
        match self {
            Locked::Key(d) | Locked::Everyone(d) => d,
        }
    }
}

#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>,
    /// Times of the most recent failures (all keys), at most the global budget.
    recent: VecDeque<Instant>,
}

pub struct LoginThrottle {
    state: Mutex<State>,
    global_budget: Option<usize>,
}

impl Default for LoginThrottle {
    /// Per-key backoff plus the process-wide budget: for login.
    fn default() -> Self {
        LoginThrottle { state: Mutex::default(), global_budget: Some(GLOBAL_BUDGET) }
    }
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
    /// Per-key backoff only. For keys an anonymous caller cannot choose (a
    /// signed-in user's id), where a shared budget would only let one user
    /// lock out the others.
    pub fn per_key() -> Self {
        LoginThrottle { state: Mutex::default(), global_budget: None }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `Some` when an attempt for `key` must be refused right now.
    pub fn check(&self, key: &str) -> Option<Locked> {
        self.check_at(key, Instant::now())
    }

    fn check_at(&self, k: &str, now: Instant) -> Option<Locked> {
        let mut s = self.state();
        if let Some(budget) = self.global_budget {
            while s.recent.front().is_some_and(|t| now.duration_since(*t) >= GLOBAL_WINDOW) {
                s.recent.pop_front();
            }
            if s.recent.len() >= budget {
                let until = s.recent[s.recent.len() - budget] + GLOBAL_WINDOW;
                return Some(Locked::Everyone(until - now));
            }
        }
        let until = s.entries.get(&key(k))?.locked_until?;
        (until > now).then(|| Locked::Key(until - now))
    }

    /// Records a failed attempt; returns the lock it triggered for the key, if any.
    pub fn failure(&self, key: &str) -> Option<Duration> {
        self.failure_at(key, Instant::now())
    }

    fn failure_at(&self, k: &str, now: Instant) -> Option<Duration> {
        let mut s = self.state();
        if let Some(budget) = self.global_budget {
            // Requests that passed `check` concurrently can overshoot; keep the newest.
            while s.recent.len() >= budget {
                s.recent.pop_front();
            }
            s.recent.push_back(now);
        }
        let k = key(k);
        let entries = &mut s.entries;
        if !entries.contains_key(&k) && entries.len() >= MAX_ENTRIES {
            entries.retain(|_, e| now.duration_since(e.last_failure) < FORGET_AFTER);
            // Still full: evict the stalest key rather than not recording this failure.
            while entries.len() >= MAX_ENTRIES {
                let Some(oldest) = entries.iter().min_by_key(|(_, e)| e.last_failure).map(|(k, _)| k.clone()) else {
                    break;
                };
                entries.remove(&oldest);
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

    pub fn success(&self, key: &str) {
        self.state().entries.remove(&self::key(key));
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

    #[test]
    fn a_full_table_still_throttles_a_new_username() {
        // Per-key only, so the global budget cannot be what locks the victim.
        let t = LoginThrottle::per_key();
        let now = Instant::now();
        for i in 0..MAX_ENTRIES {
            t.failure_at(&format!("filler-{i}"), now);
        }
        for _ in 0..FREE_FAILURES {
            t.failure_at("victim", now);
        }
        assert!(matches!(t.check_at("victim", now), Some(Locked::Key(_))), "overflow must not disable the lock");
        assert!(t.state().entries.len() <= MAX_ENTRIES, "the table stays bounded");
    }

    #[test]
    fn spraying_many_usernames_hits_the_global_budget() {
        let t = LoginThrottle::default();
        let now = Instant::now();
        for i in 0..GLOBAL_BUDGET {
            assert_eq!(t.check_at(&format!("user-{i}"), now), None, "one guess each is not locked per username");
            t.failure_at(&format!("user-{i}"), now);
        }
        assert_eq!(t.check_at("someone-else", now), Some(Locked::Everyone(GLOBAL_WINDOW)));
        let later = now + GLOBAL_WINDOW / 2;
        assert_eq!(t.check_at("someone-else", later), Some(Locked::Everyone(GLOBAL_WINDOW / 2)));
        assert_eq!(t.check_at("someone-else", now + GLOBAL_WINDOW), None, "the window slides");
    }

    #[test]
    fn the_global_window_slides_and_stays_bounded() {
        let t = LoginThrottle::default();
        let start = Instant::now();
        // Spread the budget over the window, then one more failure per step:
        // the lock ends when the oldest failure in the window expires.
        let step = GLOBAL_WINDOW / GLOBAL_BUDGET as u32;
        for i in 0..GLOBAL_BUDGET as u32 * 3 {
            t.failure_at(&format!("u{i}"), start + step * i);
        }
        assert!(t.state().recent.len() <= GLOBAL_BUDGET);
        let last = start + step * (GLOBAL_BUDGET as u32 * 3 - 1);
        assert_eq!(t.check_at("x", last), Some(Locked::Everyone(step)));
        assert_eq!(t.check_at("x", last + step), None);
    }

    #[test]
    fn per_key_throttle_has_no_global_budget() {
        let t = LoginThrottle::per_key();
        let now = Instant::now();
        for i in 0..GLOBAL_BUDGET * 2 {
            t.failure_at(&format!("user-{i}"), now);
        }
        assert_eq!(t.check_at("someone-else", now), None);
    }
}
