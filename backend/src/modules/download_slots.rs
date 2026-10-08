//! Places for streamed downloads (the import error report, the inventory
//! export): each holds a thread or a database connection while the client
//! reads, so they are capped per process and per user (GH#351). Never waits:
//! a download past a cap is refused at once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use crate::http::error::{AppError, ErrorCode};

/// The downloads of one kind in progress in this process.
pub struct Slots {
    global: Arc<Semaphore>,
    per_user: Arc<Mutex<HashMap<Uuid, usize>>>,
    max_per_user: usize,
    /// For the messages: "error report downloads", "inventory exports".
    what: &'static str,
}

/// A download in progress; gives its places back when dropped.
pub struct Slot {
    _global: OwnedSemaphorePermit,
    _user: UserSlot,
}

/// One of a user's places; given back when dropped.
struct UserSlot {
    per_user: Arc<Mutex<HashMap<Uuid, usize>>>,
    user: Uuid,
}

impl Drop for UserSlot {
    fn drop(&mut self) {
        if let Ok(mut m) = self.per_user.lock()
            && let Some(n) = m.get_mut(&self.user)
        {
            *n -= 1;
            if *n == 0 {
                m.remove(&self.user);
            }
        }
    }
}

impl Slots {
    pub fn new(max: usize, max_per_user: usize, what: &'static str) -> Self {
        Slots { global: Arc::new(Semaphore::new(max)), per_user: Arc::default(), max_per_user, what }
    }

    /// 429 RATE_LIMITED past the user's cap, 503 SERVER_BUSY past the
    /// process's. A caller without a user id shares the nil user's places.
    pub fn acquire(&self, user: Option<Uuid>) -> Result<Slot, AppError> {
        let user = user.unwrap_or(Uuid::nil());
        {
            let mut m = self.per_user.lock().map_err(|_| AppError::internal())?;
            let n = m.entry(user).or_insert(0);
            if *n >= self.max_per_user {
                let mut err = AppError::new(
                    ErrorCode::RateLimited,
                    format!(
                        "You already have {} {} in progress; retry when one has finished",
                        self.max_per_user, self.what
                    ),
                );
                err.retry_after = Some(1);
                return Err(err);
            }
            *n += 1;
        }
        // Refused below, the user's place goes back with `slot`.
        let slot = UserSlot { per_user: self.per_user.clone(), user };
        let global = self.global.clone().try_acquire_owned().map_err(|_| {
            tracing::warn!(what = self.what, "download refused: too many in progress");
            let mut err = AppError::new(
                ErrorCode::ServerBusy,
                format!("The server is sending too many {}; retry shortly", self.what),
            );
            err.retry_after = Some(1);
            err
        })?;
        Ok(Slot { _global: global, _user: slot })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downloads_past_the_user_cap_or_the_server_cap_are_refused() {
        let slots = Slots::new(2, 1, "downloads");
        let (alice, bob, carol) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let first = slots.acquire(Some(alice)).unwrap();
        assert_eq!(slots.acquire(Some(alice)).err().map(|e| e.code), Some(ErrorCode::RateLimited));
        let _second = slots.acquire(Some(bob)).unwrap();
        assert_eq!(slots.acquire(Some(carol)).err().map(|e| e.code), Some(ErrorCode::ServerBusy));
        // Carol's refusal gave her place back, and a finished download gives both back.
        drop(first);
        let _third = slots.acquire(Some(carol)).unwrap();
        assert_eq!(slots.acquire(Some(alice)).err().map(|e| e.code), Some(ErrorCode::ServerBusy));
    }
}
