//! Backoff for password guessing, per key and process-wide.
//!
//! Per key (the username for login, the user id for changing one's own
//! password): the first [`FREE_FAILURES`] - 1 wrong passwords cost nothing.
//! From then on every failure locks the key for 1 s, 2 s, 4 s, ... up to
//! [`MAX_LOCK`]; while locked, the endpoint answers 429 without checking the
//! password. A success clears the counter. Unknown usernames are throttled
//! exactly like real ones, so the lock does not reveal which names exist.
//!
//! Process-wide (login only): once [`GLOBAL_BUDGET`] failures for any
//! usernames fall within [`GLOBAL_WINDOW`], sign-in is slowed, not refused:
//! every attempt waits its turn in a single slow lane that lets one attempt
//! through per [`GLOBAL_PENALTY`], so guessing across all usernames stays at
//! about the budget's rate while a correct password still signs in. That caps
//! password spraying (one or two guesses each for many usernames), which the
//! per-username lock does not see, and it caps how fast anyone can churn the
//! per-username table. Only when [`SLOW_LANE_WAITERS`] attempts are already
//! queued is the next one refused (429).
//!
//! The per-key table is bounded by [`MAX_ENTRIES`]: when it is full, the key
//! with the oldest failure is evicted. Evicting never disables throttling:
//! filling the table takes more failures than the global budget allows.
//!
//! The state is in memory: it resets on restart and is per process. That is
//! enough to make online guessing impractical (a few attempts per hour per
//! account once its lock is at the maximum, and about [`GLOBAL_BUDGET`] per
//! [`GLOBAL_WINDOW`] across all accounts).

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;

pub const FREE_FAILURES: u32 = 5;
pub const MAX_LOCK: Duration = Duration::from_secs(15 * 60);
/// Failures, for all keys together, that fit in [`GLOBAL_WINDOW`].
pub const GLOBAL_BUDGET: usize = 300;
pub const GLOBAL_WINDOW: Duration = Duration::from_secs(10 * 60);
/// Over the budget, one attempt per this long: the budget's own rate (2 s).
pub const GLOBAL_PENALTY: Duration = Duration::from_millis(GLOBAL_WINDOW.as_millis() as u64 / GLOBAL_BUDGET as u64);
/// Attempts that may queue in the slow lane; the next one is refused.
pub const SLOW_LANE_WAITERS: usize = 64;
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

/// What to do with an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Open,
    /// Too many failures overall: go through [`LoginThrottle::slow_lane`] first.
    Slow,
    /// Too many failures for this key: refuse for this long.
    Locked(Duration),
}

#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>,
    /// Times of the most recent failures (all keys), at most the global budget.
    recent: VecDeque<Instant>,
    /// When the global budget was last reported as exhausted.
    warned_at: Option<Instant>,
}

pub struct LoginThrottle {
    state: Mutex<State>,
    global_budget: Option<usize>,
    slow_lane: Semaphore,
    slow_lane_waiters: AtomicUsize,
}

impl Default for LoginThrottle {
    /// Per-key backoff plus the process-wide budget: for login.
    fn default() -> Self {
        LoginThrottle::new(Some(GLOBAL_BUDGET))
    }
}

/// A place in the slow lane's queue, given back when the attempt leaves it
/// (also when the client disconnects while waiting).
struct Place<'a>(&'a AtomicUsize);

impl<'a> Place<'a> {
    fn take(waiters: &'a AtomicUsize) -> Option<Self> {
        waiters.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < SLOW_LANE_WAITERS).then_some(n + 1)).ok()?;
        Some(Place(waiters))
    }
}

impl Drop for Place<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
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
        LoginThrottle::new(None)
    }

    fn new(global_budget: Option<usize>) -> Self {
        LoginThrottle {
            state: Mutex::default(),
            global_budget,
            slow_lane: Semaphore::new(1),
            slow_lane_waiters: AtomicUsize::new(0),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether an attempt for `key` may go ahead right now.
    pub fn check(&self, key: &str) -> Gate {
        self.check_at(key, Instant::now())
    }

    fn check_at(&self, k: &str, now: Instant) -> Gate {
        let mut s = self.state();
        if let Some(until) = s.entries.get(&key(k)).and_then(|e| e.locked_until)
            && until > now
        {
            return Gate::Locked(until - now);
        }
        let Some(budget) = self.global_budget else { return Gate::Open };
        while s.recent.front().is_some_and(|t| now.duration_since(*t) >= GLOBAL_WINDOW) {
            s.recent.pop_front();
        }
        if s.recent.len() < budget {
            return Gate::Open;
        }
        if s.warned_at.is_none_or(|t| now.duration_since(t) >= GLOBAL_WINDOW) {
            s.warned_at = Some(now);
            tracing::warn!(
                failures = s.recent.len(),
                window_secs = GLOBAL_WINDOW.as_secs(),
                "failed sign-ins exceed the server-wide budget (password spraying?): sign-in is slowed to one attempt per {} s",
                GLOBAL_PENALTY.as_secs_f32()
            );
        }
        Gate::Slow
    }

    /// Waits for a turn in the slow lane: one attempt per [`GLOBAL_PENALTY`],
    /// in arrival order. `false` when [`SLOW_LANE_WAITERS`] are already queued.
    pub async fn slow_lane(&self) -> bool {
        let Some(_place) = Place::take(&self.slow_lane_waiters) else { return false };
        let Ok(_turn) = self.slow_lane.acquire().await else { return false };
        tokio::time::sleep(GLOBAL_PENALTY).await;
        true
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
        assert_eq!(t.check_at("alice", now), Gate::Open);
        assert_eq!(t.failure_at("ALICE", now), Some(Duration::from_secs(1)));
        assert_eq!(t.check_at(" alice ", now), Gate::Locked(Duration::from_secs(1)));
        assert_eq!(t.check_at("alice", now + Duration::from_secs(2)), Gate::Open, "lock expires");
        assert_eq!(t.check_at("bob", now), Gate::Open, "other users are unaffected");
        assert_eq!(t.failure_at("alice", now + Duration::from_secs(2)), Some(Duration::from_secs(2)));
        t.success("alice");
        assert_eq!(t.check_at("alice", now + Duration::from_secs(2)), Gate::Open);
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
        assert!(matches!(t.check_at("victim", now), Gate::Locked(_)), "overflow must not disable the lock");
        assert!(t.state().entries.len() <= MAX_ENTRIES, "the table stays bounded");
    }

    #[test]
    fn spraying_many_usernames_slows_sign_in_instead_of_refusing_it() {
        let t = LoginThrottle::default();
        let now = Instant::now();
        for i in 0..GLOBAL_BUDGET {
            assert_eq!(t.check_at(&format!("user-{i}"), now), Gate::Open, "one guess each is not locked per username");
            t.failure_at(&format!("user-{i}"), now);
        }
        // Not a refusal: anyone else (the administrator with the right password) gets through, slowly.
        assert_eq!(t.check_at("someone-else", now), Gate::Slow);
        assert_eq!(t.check_at("someone-else", now + GLOBAL_WINDOW / 2), Gate::Slow);
        assert_eq!(t.check_at("someone-else", now + GLOBAL_WINDOW), Gate::Open, "the window slides");
    }

    #[test]
    fn a_locked_username_is_refused_even_while_everyone_is_slowed() {
        let t = LoginThrottle::default();
        let now = Instant::now();
        for i in 0..GLOBAL_BUDGET - FREE_FAILURES as usize {
            t.failure_at(&format!("user-{i}"), now);
        }
        for _ in 0..FREE_FAILURES {
            t.failure_at("victim", now);
        }
        assert_eq!(t.check_at("victim", now), Gate::Locked(Duration::from_secs(1)));
        assert_eq!(t.check_at("someone-else", now), Gate::Slow);
    }

    #[test]
    fn the_global_window_slides_and_stays_bounded() {
        let t = LoginThrottle::default();
        let start = Instant::now();
        // Spread the budget over the window, then one more failure per step:
        // the slowdown ends when the oldest failure in the window expires.
        let step = GLOBAL_WINDOW / GLOBAL_BUDGET as u32;
        for i in 0..GLOBAL_BUDGET as u32 * 3 {
            t.failure_at(&format!("u{i}"), start + step * i);
        }
        assert!(t.state().recent.len() <= GLOBAL_BUDGET);
        let last = start + step * (GLOBAL_BUDGET as u32 * 3 - 1);
        assert_eq!(t.check_at("x", last), Gate::Slow);
        assert_eq!(t.check_at("x", last + step), Gate::Open);
    }

    #[test]
    fn the_penalty_is_the_budget_rate() {
        assert_eq!(GLOBAL_PENALTY, GLOBAL_WINDOW / GLOBAL_BUDGET as u32);
        assert_eq!(GLOBAL_PENALTY, Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn the_slow_lane_lets_one_attempt_through_per_penalty_and_bounds_its_queue() {
        let t = std::sync::Arc::new(LoginThrottle::default());
        let start = tokio::time::Instant::now();
        let mut queued = Vec::new();
        for _ in 0..SLOW_LANE_WAITERS {
            let t = t.clone();
            queued.push(tokio::spawn(async move { t.slow_lane().await.then(tokio::time::Instant::now) }));
        }
        while t.slow_lane_waiters.load(Ordering::Acquire) < SLOW_LANE_WAITERS {
            tokio::task::yield_now().await;
        }
        assert!(!t.slow_lane().await, "a full queue refuses the next attempt");
        let mut done = vec![start];
        for q in queued {
            done.push(q.await.unwrap().expect("queued attempts get their turn"));
        }
        done.sort();
        for w in done.windows(2) {
            assert!(w[1] - w[0] >= GLOBAL_PENALTY, "one attempt per penalty");
        }
        let all = GLOBAL_PENALTY * SLOW_LANE_WAITERS as u32;
        assert!(done[SLOW_LANE_WAITERS] - start < all + Duration::from_secs(1), "and no slower");
        assert!(t.slow_lane().await, "the queue has room again");
        // A client that gives up while queued gives its place back.
        let gave_up = tokio::time::timeout(Duration::from_millis(1), t.slow_lane()).await;
        assert!(gave_up.is_err());
        assert_eq!(t.slow_lane_waiters.load(Ordering::Acquire), 0);
    }

    #[test]
    fn per_key_throttle_has_no_global_budget() {
        let t = LoginThrottle::per_key();
        let now = Instant::now();
        for i in 0..GLOBAL_BUDGET * 2 {
            t.failure_at(&format!("user-{i}"), now);
        }
        assert_eq!(t.check_at("someone-else", now), Gate::Open);
    }
}
