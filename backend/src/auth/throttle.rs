//! Backoff for password guessing, per key and process-wide.
//!
//! Per key (the username for login, the user id for changing one's own
//! password) and client network ([`Net`]): the first [`FREE_FAILURES`] - 1
//! wrong passwords cost nothing. From then on every failure locks the key for
//! that network for 1 s, 2 s, 4 s, ... up to [`MAX_LOCK`]; while locked, the
//! endpoint answers 429 without checking the password. A success clears the
//! counter for that network. Unknown usernames are throttled exactly like real
//! ones, so the lock does not reveal which names exist.
//!
//! Per key, all networks together: each network counts with at most
//! [`FREE_FAILURES`] of its failures, and once they add up to
//! [`ACCOUNT_BUDGET`] (failures from at least three networks) the key is
//! locked for every network, with the same backoff. So guessing from one
//! network cannot lock the account holder out from another (GH#187), while
//! guesses spread over many networks (or forged forwarding headers) are still
//! capped per account.
//!
//! Process-wide (login only): once [`GLOBAL_BUDGET`] failures for any
//! usernames fall within [`GLOBAL_WINDOW`], sign-in is slowed, not refused:
//! every attempt waits its turn in a single slow lane that lets one attempt
//! through per [`GLOBAL_PENALTY`], so guessing across all usernames stays at
//! about the budget's rate while a correct password still signs in. That caps
//! password spraying (one or two guesses each for many usernames), which the
//! per-username lock does not see, and it caps how fast anyone can churn the
//! per-username table. Only when [`SLOW_LANE_WAITERS`] attempts are already
//! queued, or [`SLOW_LANE_PER_NET`] from the same network, is the next one
//! refused (429): one network cannot fill the queue for everyone.
//!
//! An attempt is reserved when it passes the gate and released when it is
//! done ([`Attempt`]), so the budgets hold against concurrent requests too:
//! a key admits only as many attempts at once as it has free failures left
//! (at least one), per network and for all networks together, and attempts in
//! flight count against the global budget as if they had failed. Otherwise
//! every request that arrived before the first failure was recorded would
//! have its password checked (GH#118).
//!
//! The per-key table is bounded by [`MAX_ENTRIES`] keys of at most
//! [`MAX_NETS`] networks each: when it is full, the key (or network) with the
//! oldest failure is evicted. Evicting never disables throttling: filling the
//! table takes more failures than the global budget allows, and a key with
//! failures from [`MAX_NETS`] networks is locked for all of them.
//!
//! The network comes from the client address the reverse proxy reports (see
//! [`crate::auth::session::client_ip`]). Without a proxy that overwrites the
//! forwarding headers, a client can claim any network: it then gets the
//! account budget instead of the per-network one, and can lock the account
//! for everyone as before (risk assessment, T1).
//!
//! The state is in memory: it resets on restart and is per process. That is
//! enough to make online guessing impractical (a few attempts per hour per
//! account and network once its lock is at the maximum, and about
//! [`GLOBAL_BUDGET`] per [`GLOBAL_WINDOW`] across all accounts).

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;

pub const FREE_FAILURES: u32 = 5;
/// Failures for one key from all networks, each counting with at most
/// [`FREE_FAILURES`], before the key is locked for every network.
pub const ACCOUNT_BUDGET: u32 = 3 * FREE_FAILURES;
pub const MAX_LOCK: Duration = Duration::from_secs(15 * 60);
/// Failures, for all keys together, that fit in [`GLOBAL_WINDOW`].
pub const GLOBAL_BUDGET: usize = 300;
pub const GLOBAL_WINDOW: Duration = Duration::from_secs(10 * 60);
/// Over the budget, one attempt per this long: the budget's own rate (2 s).
pub const GLOBAL_PENALTY: Duration = Duration::from_millis(GLOBAL_WINDOW.as_millis() as u64 / GLOBAL_BUDGET as u64);
/// Attempts that may queue in the slow lane; the next one is refused.
pub const SLOW_LANE_WAITERS: usize = 64;
/// Attempts from one network that may queue in the slow lane.
pub const SLOW_LANE_PER_NET: usize = 4;
/// A key with no failure for this long starts over.
const FORGET_AFTER: Duration = Duration::from_secs(60 * 60);
/// Upper bound on tracked keys.
const MAX_ENTRIES: usize = 10_000;
/// Upper bound on tracked networks per key.
const MAX_NETS: usize = 64;
/// Retry-After for a key whose remaining free attempts are all in flight.
pub const BUSY: Duration = Duration::from_secs(1);

/// The network a sign-in comes from: the IPv4 /24 or IPv6 /64 of the client
/// address, so one host cannot claim a fresh budget per address it holds.
/// Unknown for requests without one (all of them share that budget). Kept in
/// memory only, for throttling; never stored or logged.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Net(Option<IpAddr>);

impl Net {
    pub fn of(ip: Option<IpAddr>) -> Net {
        Net(ip.map(|ip| match ip.to_canonical() {
            IpAddr::V4(v4) => IpAddr::V4((u32::from(v4) & 0xffff_ff00).into()),
            IpAddr::V6(v6) => IpAddr::V6((u128::from(v6) & !0u128 << 64).into()),
        }))
    }
}

impl fmt::Debug for Net {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Not the address: request contexts are Debug, and it must stay out of the logs.
        f.write_str(if self.0.is_some() { "Net(..)" } else { "Net(unknown)" })
    }
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    failures: u32,
    last_failure: Instant,
    locked_until: Option<Instant>,
}

impl Entry {
    fn new(now: Instant) -> Self {
        Entry { failures: 0, last_failure: now, locked_until: None }
    }

    fn fresh(&self, now: Instant) -> bool {
        now.duration_since(self.last_failure) < FORGET_AFTER
    }

    /// Counts a failure; `free`: how many cost nothing. Returns the lock.
    fn fail(&mut self, now: Instant, free: u32) -> Option<Duration> {
        if !self.fresh(now) {
            *self = Entry::new(now);
        }
        self.failures = self.failures.saturating_add(1);
        self.last_failure = now;
        let lock = backoff(self.failures, free);
        self.locked_until = lock.map(|d| now + d);
        lock
    }

    fn locked(&self, now: Instant) -> Option<Duration> {
        self.locked_until.filter(|until| *until > now && self.fresh(now)).map(|until| until - now)
    }
}

/// One key's failures: per network, and past the account budget.
#[derive(Debug)]
struct Account {
    nets: HashMap<Net, Entry>,
    /// Failures while the networks' share was at [`ACCOUNT_BUDGET`]; locks every network.
    over: Entry,
    last_failure: Instant,
}

impl Account {
    fn new(now: Instant) -> Self {
        Account { nets: HashMap::new(), over: Entry::new(now), last_failure: now }
    }

    /// The networks' failures toward [`ACCOUNT_BUDGET`].
    fn share(&self, now: Instant) -> u32 {
        self.nets.values().filter(|e| e.fresh(now)).map(|e| e.failures.min(FREE_FAILURES)).sum()
    }

    fn net(&self, net: Net, now: Instant) -> Option<&Entry> {
        self.nets.get(&net).filter(|e| e.fresh(now))
    }
}

/// What to do with an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Open,
    /// Too many failures overall: go through [`LoginThrottle::slow_lane`] first.
    Slow,
    /// Too many failures (or attempts in flight) for this key: refuse for this long.
    Locked(Duration),
}

#[derive(Default)]
struct State {
    entries: HashMap<String, Account>,
    /// Times of the most recent failures (all keys), at most the global budget.
    recent: VecDeque<Instant>,
    /// Attempts past the gate and not yet done, per key and network (only those with any).
    in_flight: HashMap<(String, Net), u32>,
    /// The same, per key for all networks together.
    in_flight_key: HashMap<String, u32>,
    /// Attempts past the gate and not yet done, all keys together.
    in_flight_total: usize,
    /// When the global budget was last reported as exhausted.
    warned_at: Option<Instant>,
}

/// Who is queued in the slow lane.
#[derive(Default)]
struct Queue {
    waiters: usize,
    per_net: HashMap<Net, usize>,
}

pub struct LoginThrottle {
    state: Mutex<State>,
    global_budget: Option<usize>,
    slow_lane: Semaphore,
    queue: Mutex<Queue>,
}

impl Default for LoginThrottle {
    /// Per-key backoff plus the process-wide budget: for login.
    fn default() -> Self {
        LoginThrottle::new(Some(GLOBAL_BUDGET))
    }
}

/// A place in the slow lane's queue, given back when the attempt leaves it
/// (also when the client disconnects while waiting).
struct Place<'a> {
    queue: &'a Mutex<Queue>,
    net: Net,
}

impl<'a> Place<'a> {
    fn take(queue: &'a Mutex<Queue>, net: Net) -> Option<Self> {
        let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
        let mine = q.per_net.get(&net).copied().unwrap_or(0);
        if q.waiters >= SLOW_LANE_WAITERS || mine >= SLOW_LANE_PER_NET {
            return None;
        }
        q.waiters += 1;
        q.per_net.insert(net, mine + 1);
        Some(Place { queue, net })
    }
}

impl Drop for Place<'_> {
    fn drop(&mut self) {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.waiters -= 1;
        if let Some(n) = q.per_net.get_mut(&self.net) {
            *n -= 1;
            if *n == 0 {
                q.per_net.remove(&self.net);
            }
        }
    }
}

/// An attempt that passed the gate. Until it is done it counts against the
/// key's free failures and the global budget; report how it ended with
/// [`Attempt::failure`] or [`Attempt::success`]. Dropping it (an error, a
/// second factor still due, a client that went away) releases it uncounted.
#[must_use = "an attempt holds its reservation until it is dropped"]
pub struct Attempt<'a> {
    throttle: &'a LoginThrottle,
    key: String,
    net: Net,
}

impl Attempt<'_> {
    /// Records the failure; returns the lock it triggered for the key, if any.
    pub fn failure(self) -> Option<Duration> {
        self.throttle.failure(&self.key, self.net)
    }

    pub fn success(self) {
        self.throttle.success(&self.key, self.net);
    }
}

fn release<K: std::hash::Hash + Eq>(map: &mut HashMap<K, u32>, k: &K) {
    if let Some(n) = map.get_mut(k) {
        *n -= 1;
        if *n == 0 {
            map.remove(k);
        }
    }
}

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        let mut s = self.throttle.state();
        s.in_flight_total = s.in_flight_total.saturating_sub(1);
        let k = (std::mem::take(&mut self.key), self.net);
        release(&mut s.in_flight, &k);
        release(&mut s.in_flight_key, &k.0);
    }
}

fn key(username: &str) -> String {
    username.trim().to_lowercase()
}

/// The lock after `failures`, the first `free` - 1 of which cost nothing.
fn backoff(failures: u32, free: u32) -> Option<Duration> {
    if failures < free {
        return None;
    }
    let exp = (failures - free).min(20);
    Some(Duration::from_secs(1u64 << exp).min(MAX_LOCK))
}

#[cfg(test)]
fn lock_for(failures: u32) -> Option<Duration> {
    backoff(failures, FREE_FAILURES)
}

impl LoginThrottle {
    /// Per-key backoff only. For keys an anonymous caller cannot choose (a
    /// signed-in user's id), where a shared budget would only let one user
    /// lock out the others.
    pub fn per_key() -> Self {
        LoginThrottle::new(None)
    }

    fn new(global_budget: Option<usize>) -> Self {
        LoginThrottle { state: Mutex::default(), global_budget, slow_lane: Semaphore::new(1), queue: Mutex::default() }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether an attempt for `key` from `net` may go ahead right now, without reserving it.
    #[cfg(test)]
    pub fn check(&self, key: &str, net: Net) -> Gate {
        self.check_at(key, net, Instant::now())
    }

    #[cfg(test)]
    fn check_at(&self, k: &str, net: Net, now: Instant) -> Gate {
        self.gate(&mut self.state(), &key(k), net, now)
    }

    /// Reserves an attempt for `key` from `net` if it may go ahead. `slowed` is
    /// for an attempt that has had its turn in the slow lane: over the global
    /// budget, it goes ahead instead of being sent to the slow lane again.
    pub fn begin(&self, key: &str, net: Net, slowed: bool) -> Result<Attempt<'_>, Gate> {
        self.begin_at(key, net, slowed, Instant::now())
    }

    fn begin_at(&self, k: &str, net: Net, slowed: bool, now: Instant) -> Result<Attempt<'_>, Gate> {
        let k = key(k);
        let mut s = self.state();
        match self.gate(&mut s, &k, net, now) {
            Gate::Open => {}
            Gate::Slow if slowed => {}
            refused => return Err(refused),
        }
        s.in_flight_total += 1;
        *s.in_flight.entry((k.clone(), net)).or_insert(0) += 1;
        *s.in_flight_key.entry(k.clone()).or_insert(0) += 1;
        Ok(Attempt { throttle: self, key: k, net })
    }

    fn gate(&self, s: &mut State, k: &str, net: Net, now: Instant) -> Gate {
        let account = s.entries.get(k);
        let entry = account.and_then(|a| a.net(net, now));
        if let Some(wait) = account.and_then(|a| a.over.locked(now)).max(entry.and_then(|e| e.locked(now))) {
            return Gate::Locked(wait);
        }
        // Were all attempts in flight to fail, a lock would already be due:
        // wait for them. Past the free failures, one attempt at a time.
        let failures = entry.map_or(0, |e| e.failures);
        let in_flight = s.in_flight.get(&(k.to_owned(), net)).copied().unwrap_or(0);
        if in_flight >= FREE_FAILURES.saturating_sub(failures).max(1) {
            return Gate::Locked(BUSY);
        }
        let share = account.map_or(0, |a| a.share(now));
        let in_flight_key = s.in_flight_key.get(k).copied().unwrap_or(0);
        if in_flight_key >= ACCOUNT_BUDGET.saturating_sub(share).max(1) {
            return Gate::Locked(BUSY);
        }
        let Some(budget) = self.global_budget else { return Gate::Open };
        while s.recent.front().is_some_and(|t| now.duration_since(*t) >= GLOBAL_WINDOW) {
            s.recent.pop_front();
        }
        if s.recent.len() + s.in_flight_total < budget {
            return Gate::Open;
        }
        if s.recent.len() >= budget && s.warned_at.is_none_or(|t| now.duration_since(t) >= GLOBAL_WINDOW) {
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
    /// in arrival order. `false` when [`SLOW_LANE_WAITERS`] are already
    /// queued, or [`SLOW_LANE_PER_NET`] from `net`.
    pub async fn slow_lane(&self, net: Net) -> bool {
        let Some(_place) = Place::take(&self.queue, net) else { return false };
        let Ok(_turn) = self.slow_lane.acquire().await else { return false };
        tokio::time::sleep(GLOBAL_PENALTY).await;
        true
    }

    /// Records a failed attempt; returns the lock it triggered for the key, if any.
    pub fn failure(&self, key: &str, net: Net) -> Option<Duration> {
        self.failure_at(key, net, Instant::now())
    }

    fn failure_at(&self, k: &str, net: Net, now: Instant) -> Option<Duration> {
        let mut s = self.state();
        if let Some(budget) = self.global_budget {
            // Failures not reserved through `begin` can overshoot; keep the newest.
            while s.recent.len() >= budget {
                s.recent.pop_front();
            }
            s.recent.push_back(now);
        }
        let k = key(k);
        let entries = &mut s.entries;
        if !entries.contains_key(&k) && entries.len() >= MAX_ENTRIES {
            entries.retain(|_, a| now.duration_since(a.last_failure) < FORGET_AFTER);
            // Still full: evict the stalest key rather than not recording this failure.
            while entries.len() >= MAX_ENTRIES {
                let Some(oldest) = entries.iter().min_by_key(|(_, a)| a.last_failure).map(|(k, _)| k.clone()) else {
                    break;
                };
                entries.remove(&oldest);
            }
        }
        let account = entries.entry(k).or_insert_with(|| Account::new(now));
        account.last_failure = now;
        account.nets.retain(|_, e| e.fresh(now));
        if !account.nets.contains_key(&net) && account.nets.len() >= MAX_NETS {
            // So many networks already lock the key for all of them.
            let oldest = account.nets.iter().min_by_key(|(_, e)| e.last_failure).map(|(n, _)| *n);
            oldest.map(|n| account.nets.remove(&n));
        }
        let lock = account.nets.entry(net).or_insert_with(|| Entry::new(now)).fail(now, FREE_FAILURES);
        if account.share(now) < ACCOUNT_BUDGET {
            return lock;
        }
        account.over.fail(now, 1).max(lock)
    }

    /// Clears the key's failures from `net`; those from other networks stay.
    pub fn success(&self, key: &str, net: Net) {
        let k = self::key(key);
        let mut s = self.state();
        let Some(account) = s.entries.get_mut(&k) else { return };
        account.nets.remove(&net);
        if account.nets.is_empty() && account.over.failures == 0 {
            s.entries.remove(&k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: Net = Net(None);

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
            assert_eq!(t.failure_at("Alice", N, now), None);
        }
        assert_eq!(t.check_at("alice", N, now), Gate::Open);
        assert_eq!(t.failure_at("ALICE", N, now), Some(Duration::from_secs(1)));
        assert_eq!(t.check_at(" alice ", N, now), Gate::Locked(Duration::from_secs(1)));
        assert_eq!(t.check_at("alice", N, now + Duration::from_secs(2)), Gate::Open, "lock expires");
        assert_eq!(t.check_at("bob", N, now), Gate::Open, "other users are unaffected");
        assert_eq!(t.failure_at("alice", N, now + Duration::from_secs(2)), Some(Duration::from_secs(2)));
        t.success("alice", N);
        assert_eq!(t.check_at("alice", N, now + Duration::from_secs(2)), Gate::Open);
        assert_eq!(t.failure_at("alice", N, now + Duration::from_secs(3)), None, "counter starts over");
    }

    #[test]
    fn old_failures_are_forgotten() {
        let t = LoginThrottle::default();
        let now = Instant::now();
        for _ in 0..FREE_FAILURES + 2 {
            t.failure_at("carol", N, now);
        }
        assert_eq!(t.failure_at("carol", N, now + FORGET_AFTER), None);
    }

    #[test]
    fn a_full_table_still_throttles_a_new_username() {
        // Per-key only, so the global budget cannot be what locks the victim.
        let t = LoginThrottle::per_key();
        let now = Instant::now();
        for i in 0..MAX_ENTRIES {
            t.failure_at(&format!("filler-{i}"), N, now);
        }
        for _ in 0..FREE_FAILURES {
            t.failure_at("victim", N, now);
        }
        assert!(matches!(t.check_at("victim", N, now), Gate::Locked(_)), "overflow must not disable the lock");
        assert!(t.state().entries.len() <= MAX_ENTRIES, "the table stays bounded");
    }

    #[test]
    fn spraying_many_usernames_slows_sign_in_instead_of_refusing_it() {
        let t = LoginThrottle::default();
        let now = Instant::now();
        for i in 0..GLOBAL_BUDGET {
            assert_eq!(
                t.check_at(&format!("user-{i}"), N, now),
                Gate::Open,
                "one guess each is not locked per username"
            );
            t.failure_at(&format!("user-{i}"), N, now);
        }
        // Not a refusal: anyone else (the administrator with the right password) gets through, slowly.
        assert_eq!(t.check_at("someone-else", N, now), Gate::Slow);
        assert_eq!(t.check_at("someone-else", N, now + GLOBAL_WINDOW / 2), Gate::Slow);
        assert_eq!(t.check_at("someone-else", N, now + GLOBAL_WINDOW), Gate::Open, "the window slides");
    }

    #[test]
    fn a_locked_username_is_refused_even_while_everyone_is_slowed() {
        let t = LoginThrottle::default();
        let now = Instant::now();
        for i in 0..GLOBAL_BUDGET - FREE_FAILURES as usize {
            t.failure_at(&format!("user-{i}"), N, now);
        }
        for _ in 0..FREE_FAILURES {
            t.failure_at("victim", N, now);
        }
        assert_eq!(t.check_at("victim", N, now), Gate::Locked(Duration::from_secs(1)));
        assert_eq!(t.check_at("someone-else", N, now), Gate::Slow);
    }

    #[test]
    fn the_global_window_slides_and_stays_bounded() {
        let t = LoginThrottle::default();
        let start = Instant::now();
        // Spread the budget over the window, then one more failure per step:
        // the slowdown ends when the oldest failure in the window expires.
        let step = GLOBAL_WINDOW / GLOBAL_BUDGET as u32;
        for i in 0..GLOBAL_BUDGET as u32 * 3 {
            t.failure_at(&format!("u{i}"), N, start + step * i);
        }
        assert!(t.state().recent.len() <= GLOBAL_BUDGET);
        let last = start + step * (GLOBAL_BUDGET as u32 * 3 - 1);
        assert_eq!(t.check_at("x", N, last), Gate::Slow);
        assert_eq!(t.check_at("x", N, last + step), Gate::Open);
    }

    #[test]
    fn the_penalty_is_the_budget_rate() {
        assert_eq!(GLOBAL_PENALTY, GLOBAL_WINDOW / GLOBAL_BUDGET as u32);
        assert_eq!(GLOBAL_PENALTY, Duration::from_secs(2));
    }

    /// A client in its own /24.
    fn net(i: usize) -> Net {
        Net::of(Some(std::net::Ipv4Addr::new(10, (i >> 8) as u8, i as u8, 1).into()))
    }

    fn waiters(t: &LoginThrottle) -> usize {
        t.queue.lock().unwrap().waiters
    }

    #[tokio::test(start_paused = true)]
    async fn the_slow_lane_lets_one_attempt_through_per_penalty_and_bounds_its_queue() {
        let t = std::sync::Arc::new(LoginThrottle::default());
        let start = tokio::time::Instant::now();
        let mut queued = Vec::new();
        for i in 0..SLOW_LANE_WAITERS {
            let t = t.clone();
            queued.push(tokio::spawn(async move { t.slow_lane(net(i)).await.then(tokio::time::Instant::now) }));
        }
        while waiters(&t) < SLOW_LANE_WAITERS {
            tokio::task::yield_now().await;
        }
        assert!(!t.slow_lane(net(SLOW_LANE_WAITERS)).await, "a full queue refuses the next attempt");
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
        assert!(t.slow_lane(N).await, "the queue has room again");
        // A client that gives up while queued gives its place back.
        let gave_up = tokio::time::timeout(Duration::from_millis(1), t.slow_lane(N)).await;
        assert!(gave_up.is_err());
        assert_eq!(waiters(&t), 0);
        assert!(t.queue.lock().unwrap().per_net.is_empty());
    }

    /// GH#187: one client cannot fill the queue and so refuse everyone else.
    #[tokio::test(start_paused = true)]
    async fn one_network_gets_only_its_share_of_the_slow_lane() {
        let t = std::sync::Arc::new(LoginThrottle::default());
        let flood = Net::of(Some("203.0.113.7".parse().unwrap()));
        let same_24 = Net::of(Some("203.0.113.200".parse().unwrap()));
        let mut queued = Vec::new();
        for _ in 0..SLOW_LANE_PER_NET {
            let t = t.clone();
            queued.push(tokio::spawn(async move { t.slow_lane(flood).await }));
        }
        while waiters(&t) < SLOW_LANE_PER_NET {
            tokio::task::yield_now().await;
        }
        assert!(!t.slow_lane(flood).await, "the network's share is used up");
        assert!(!t.slow_lane(same_24).await, "another address in the same /24 shares it");
        assert!(t.slow_lane(net(1)).await, "another network still gets a place");
        for q in queued {
            assert!(q.await.unwrap());
        }
        assert!(t.slow_lane(flood).await, "places are given back");
    }

    #[test]
    fn a_key_admits_only_its_free_failures_at_once() {
        let t = LoginThrottle::default();
        let now = Instant::now();
        let attempts: Vec<_> = (0..FREE_FAILURES).map(|_| t.begin_at("Alice", N, false, now).expect("free")).collect();
        assert_eq!(t.begin_at("alice", N, false, now).err(), Some(Gate::Locked(BUSY)), "all free failures in flight");
        assert_eq!(t.check_at("bob", N, now), Gate::Open, "other users are unaffected");
        let mut locked = None;
        for a in attempts {
            locked = a.failure();
        }
        assert_eq!(locked, Some(Duration::from_secs(1)), "the last one in flight locks the key");
        assert!(matches!(t.check_at("alice", N, now), Gate::Locked(_)));
        // Past the free failures, one attempt at a time.
        let later = now + Duration::from_secs(2);
        let one = t.begin_at("alice", N, false, later).expect("lock expired");
        assert_eq!(t.begin_at("alice", N, false, later).err(), Some(Gate::Locked(BUSY)));
        drop(one);
        assert!(t.begin_at("alice", N, false, later).is_ok(), "a dropped attempt is released uncounted");
        let s = t.state();
        assert_eq!((s.in_flight.len(), s.in_flight_total), (0, 0));
        assert_eq!(s.entries["alice"].nets[&N].failures, FREE_FAILURES);
    }

    #[test]
    fn a_success_frees_the_key_but_not_the_attempts_in_flight() {
        let t = LoginThrottle::default();
        let now = Instant::now();
        for _ in 1..FREE_FAILURES {
            t.failure_at("dave", N, now);
        }
        let right = t.begin_at("dave", N, false, now).unwrap();
        assert_eq!(t.begin_at("dave", N, false, now).err(), Some(Gate::Locked(BUSY)));
        right.success();
        let a: Vec<_> =
            (0..FREE_FAILURES).map(|_| t.begin_at("dave", N, false, now).expect("counter cleared")).collect();
        assert!(t.begin_at("dave", N, false, now).is_err());
        drop(a);
    }

    #[test]
    fn attempts_in_flight_count_against_the_global_budget() {
        let t = LoginThrottle::default();
        let now = Instant::now();
        let a: Vec<_> = (0..GLOBAL_BUDGET)
            .map(|i| t.begin_at(&format!("user-{i}"), N, false, now).expect("within budget"))
            .collect();
        assert_eq!(t.begin_at("someone-else", N, false, now).err(), Some(Gate::Slow));
        let slowed = t.begin_at("someone-else", N, true, now).expect("goes ahead after the slow lane");
        drop(slowed);
        drop(a);
        assert_eq!(t.check_at("someone-else", N, now), Gate::Open, "released without failing");
    }

    /// GH#187: guessing from one network cannot lock the account holder out
    /// from another; a correct password from there still gets its check.
    #[test]
    fn failures_from_one_network_lock_the_key_for_that_network_only() {
        let t = LoginThrottle::default();
        let now = Instant::now();
        for _ in 0..FREE_FAILURES * 10 {
            t.failure_at("admin", net(1), now);
        }
        assert!(matches!(t.check_at("admin", net(1), now), Gate::Locked(_)));
        assert_eq!(t.check_at("admin", net(2), now), Gate::Open, "the owner's network is not locked");
        let owner = t.begin_at("admin", net(2), false, now).expect("checked");
        owner.success();
        assert!(matches!(t.check_at("admin", net(1), now), Gate::Locked(_)), "the guesser stays locked");
        // No address (no proxy header, no peer) is one network like any other.
        assert_eq!(t.check_at("admin", N, now), Gate::Open);
    }

    /// Guesses spread over many networks (or forged forwarding headers) still
    /// lock the key for every network once they reach the account budget.
    #[test]
    fn failures_from_many_networks_lock_the_key_everywhere() {
        let t = LoginThrottle::default();
        let now = Instant::now();
        let nets = (ACCOUNT_BUDGET / FREE_FAILURES) as usize;
        for i in 0..nets {
            for _ in 1..FREE_FAILURES {
                assert_eq!(t.failure_at("admin", net(i), now), None);
            }
        }
        assert_eq!(t.check_at("admin", net(99), now), Gate::Open, "below the account budget");
        let mut locked = None;
        for i in 0..nets {
            locked = t.failure_at("admin", net(i), now);
        }
        assert_eq!(locked, Some(Duration::from_secs(1)), "the budget's last failure locks the key");
        assert_eq!(t.check_at("admin", net(99), now), Gate::Locked(Duration::from_secs(1)), "for every network");
        assert_eq!(t.check_at("admin", net(99), now + Duration::from_secs(2)), Gate::Open, "with the same backoff");
        assert_eq!(t.failure_at("admin", net(99), now + Duration::from_secs(2)), Some(Duration::from_secs(2)));
        assert_eq!(t.check_at("bob", net(99), now), Gate::Open);
    }

    #[test]
    fn a_network_is_a_v4_24_or_a_v6_64() {
        let n = |s: &str| Net::of(Some(s.parse().unwrap()));
        assert_eq!(n("198.51.100.7"), n("198.51.100.250"));
        assert_ne!(n("198.51.100.7"), n("198.51.101.7"));
        assert_eq!(n("::ffff:198.51.100.7"), n("198.51.100.1"), "mapped IPv4 is IPv4");
        assert_eq!(n("2001:db8:1:2::1"), n("2001:db8:1:2:ffff::9"));
        assert_ne!(n("2001:db8:1:2::1"), n("2001:db8:1:3::1"));
        assert_eq!(format!("{:?}", n("198.51.100.7")), "Net(..)", "no address in Debug output");
    }

    /// Networks in flight count toward the account budget as if they failed.
    #[test]
    fn attempts_in_flight_from_many_networks_count_against_the_account() {
        let t = LoginThrottle::default();
        let now = Instant::now();
        let a: Vec<_> = (0..ACCOUNT_BUDGET as usize)
            .map(|i| t.begin_at("admin", net(i), false, now).expect("within the account budget"))
            .collect();
        assert_eq!(t.begin_at("admin", net(999), false, now).err(), Some(Gate::Locked(BUSY)));
        drop(a);
        assert!(t.begin_at("admin", net(999), false, now).is_ok());
    }

    #[test]
    fn per_key_throttle_has_no_global_budget() {
        let t = LoginThrottle::per_key();
        let now = Instant::now();
        for i in 0..GLOBAL_BUDGET * 2 {
            t.failure_at(&format!("user-{i}"), N, now);
        }
        assert_eq!(t.check_at("someone-else", N, now), Gate::Open);
    }
}
