//! Per-`context_token` send budgeting and lifecycle.
//!
//! iLink accepts at most [`MAX_MSGS_PER_CONTEXT`] reply messages per inbound
//! `context_token`; the next send is rejected with `ret=-2`. Every user message
//! yields a fresh token with a fresh budget. This registry tracks, per user,
//! the tokens seen (newest last), how many sends each has spent, and when it
//! arrived, so the sender can:
//!   - spend the newest token's budget one slot at a time,
//!   - block until a new token arrives once the budget is exhausted (the user
//!     is prompted to send "继续"), then resume on the fresh token,
//!   - and garbage-collect spent / aged-out tokens, always keeping each user's
//!     newest token so there is something to reply on.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// Max reply messages iLink accepts per inbound `context_token`.
pub const MAX_MSGS_PER_CONTEXT: usize = 10;

/// A `context_token` older than this is eligible for GC — unless it is the
/// newest token for its user, which is always retained.
pub const TOKEN_MAX_AGE: Duration = Duration::from_secs(20 * 60);

struct TokenEntry {
    token: String,
    used: usize,
    created: Instant,
}

/// Outcome of trying to claim a send slot for a user.
pub enum Slot {
    /// Granted — send on `token`. `last` is true when this consumed the final
    /// (budget-th) slot of the user's **newest** token; the caller should append
    /// the "send 继续" hint to that message and expect to block on the next
    /// claim until a fresh token arrives.
    Grant { token: String, last: bool },
    /// The newest token is fully spent. `stale` is true once it has also aged
    /// past [`TOKEN_MAX_AGE`], in which case the caller should give up rather
    /// than wait forever.
    Exhausted { stale: bool },
    /// No token has ever been recorded for this user.
    Unknown,
}

/// Shared, cloneable registry of per-user `context_token` budgets.
#[derive(Clone)]
pub struct TokenRegistry {
    inner: Arc<Mutex<HashMap<String, Vec<TokenEntry>>>>,
    /// Pulsed whenever a new token is recorded, to wake blocked senders.
    notify: Arc<Notify>,
    budget: usize,
    max_age: Duration,
}

impl TokenRegistry {
    pub fn new(budget: usize, max_age: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            notify: Arc::new(Notify::new()),
            budget,
            max_age,
        }
    }

    /// Recover the map guard even if a previous holder panicked. Nothing held
    /// across `.await`, so poisoning is benign.
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<TokenEntry>>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Seed from persisted `user → latest token` so known users are reachable
    /// immediately after a restart. Treats each as a fresh (unspent) token.
    pub fn seed(&self, tokens: HashMap<String, String>) {
        let mut map = self.map();
        for (user, token) in tokens {
            map.entry(user).or_default().push(TokenEntry {
                token,
                used: 0,
                created: Instant::now(),
            });
        }
    }

    /// Record a freshly-received inbound token as the newest for `user`, then
    /// wake any blocked senders. No-op if it duplicates the current newest
    /// (so a redelivered update does not reset the count).
    pub fn record(&self, user: &str, token: &str) {
        {
            let mut map = self.map();
            let entries = map.entry(user.to_string()).or_default();
            if entries.last().map(|e| e.token.as_str()) == Some(token) {
                return;
            }
            entries.push(TokenEntry {
                token: token.to_string(),
                used: 0,
                created: Instant::now(),
            });
        }
        self.notify.notify_waiters();
    }

    /// Try to claim one send slot on the user's **newest** token only.
    ///
    /// iLink only honours replies on the most recent inbound's `context_token`
    /// (~10 of them); older tokens' reply windows are closed and sends on them
    /// fail with `ret=-2`. So we never spend stale tokens — we spend the newest
    /// until its budget runs out, then block for a fresh inbound. `Grant.last`
    /// marks the final (budget-th) slot, where the caller appends the hint.
    pub fn claim(&self, user: &str) -> Slot {
        let mut map = self.map();
        match map.get_mut(user).and_then(|v| v.last_mut()) {
            None => Slot::Unknown,
            Some(e) if e.used < self.budget => {
                e.used += 1;
                Slot::Grant {
                    token: e.token.clone(),
                    last: e.used == self.budget,
                }
            }
            Some(e) => Slot::Exhausted {
                stale: e.created.elapsed() > self.max_age,
            },
        }
    }

    /// Mark `token` as fully spent (e.g. the server rejected a send on it with
    /// `ret=-2`). Subsequent claims report `Exhausted` until a new token
    /// arrives, so the caller blocks instead of hammering a dead token.
    pub fn exhaust(&self, user: &str, token: &str) {
        if let Some(entries) = self.map().get_mut(user) {
            for e in entries.iter_mut().filter(|e| e.token == token) {
                e.used = self.budget;
            }
        }
    }

    /// Whether the user's newest token is fully spent — i.e. the send queue is
    /// (or is about to be) blocked waiting for a fresh token. Used to decide
    /// whether an incoming "继续" is a token top-up (swallow it) or a real
    /// prompt (forward it). False when the user has no token yet.
    pub fn is_exhausted(&self, user: &str) -> bool {
        self.map()
            .get(user)
            .and_then(|v| v.last())
            .map(|e| e.used >= self.budget)
            .unwrap_or(false)
    }

    /// Block until a new token may have arrived (notified) or `poll` elapses,
    /// whichever comes first. The timeout guards against a notification that
    /// races ahead of the waiter.
    pub async fn wait(&self, poll: Duration) {
        let _ = tokio::time::timeout(poll, self.notify.notified()).await;
    }

    /// Newest token for `user`, if any (used as a peer-enrichment fallback).
    pub fn latest(&self, user: &str) -> Option<String> {
        self.map()
            .get(user)
            .and_then(|v| v.last())
            .map(|e| e.token.clone())
    }

    /// Drop entries that are spent or aged out, keeping each user's newest.
    pub fn gc(&self) {
        let budget = self.budget;
        let max_age = self.max_age;
        let mut map = self.map();
        for entries in map.values_mut() {
            if entries.len() <= 1 {
                continue;
            }
            let newest = entries.last().map(|e| e.token.clone());
            entries.retain(|e| {
                Some(&e.token) == newest.as_ref()
                    || (e.used < budget && e.created.elapsed() <= max_age)
            });
        }
        map.retain(|_, v| !v.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> TokenRegistry {
        TokenRegistry::new(3, Duration::from_secs(600))
    }

    #[test]
    fn spends_budget_then_exhausts() {
        let r = reg();
        r.record("u", "t1");
        // budget = 3: two normal grants, then the last grant, then exhausted.
        match r.claim("u") {
            Slot::Grant { last, .. } => assert!(!last),
            _ => panic!("expected grant"),
        }
        assert!(matches!(r.claim("u"), Slot::Grant { last: false, .. }));
        assert!(matches!(r.claim("u"), Slot::Grant { last: true, .. }));
        assert!(matches!(r.claim("u"), Slot::Exhausted { stale: false }));
    }

    #[test]
    fn spends_only_newest_token() {
        let r = reg(); // budget = 3
        r.record("u", "a");
        r.record("u", "b");
        // Only the newest token (b) is spent; the stale "a" is never touched.
        for n in 1..=3 {
            match r.claim("u") {
                Slot::Grant { token, last } => {
                    assert_eq!(token, "b", "should only spend newest token");
                    assert_eq!(last, n == 3, "last flag wrong at slot {n}");
                }
                _ => panic!("expected grant at slot {n}"),
            }
        }
        assert!(matches!(r.claim("u"), Slot::Exhausted { .. }));
    }

    #[test]
    fn exhaust_forces_block_until_new_token() {
        let r = reg();
        r.record("u", "a");
        // Server rejected a send on "a" → mark it dead even though budget remained.
        r.exhaust("u", "a");
        assert!(matches!(r.claim("u"), Slot::Exhausted { .. }));
        // A fresh token unblocks.
        r.record("u", "b");
        match r.claim("u") {
            Slot::Grant { token, .. } => assert_eq!(token, "b"),
            _ => panic!("expected grant on fresh token"),
        }
    }

    #[test]
    fn new_token_resets_budget() {
        let r = reg();
        r.record("u", "t1");
        for _ in 0..3 {
            assert!(matches!(r.claim("u"), Slot::Grant { .. }));
        }
        assert!(matches!(r.claim("u"), Slot::Exhausted { .. }));
        r.record("u", "t2");
        match r.claim("u") {
            Slot::Grant { token, .. } => assert_eq!(token, "t2"),
            _ => panic!("expected grant on fresh token"),
        }
    }

    #[test]
    fn duplicate_record_keeps_count() {
        let r = reg();
        r.record("u", "t1");
        assert!(matches!(r.claim("u"), Slot::Grant { .. }));
        r.record("u", "t1"); // duplicate — must not reset used
        assert!(matches!(r.claim("u"), Slot::Grant { last: false, .. }));
        assert!(matches!(r.claim("u"), Slot::Grant { last: true, .. }));
    }

    #[test]
    fn unknown_user() {
        let r = reg();
        assert!(matches!(r.claim("ghost"), Slot::Unknown));
    }

    #[test]
    fn gc_keeps_newest_drops_spent() {
        let r = reg();
        r.record("u", "t1");
        for _ in 0..3 {
            r.claim("u");
        } // t1 spent
        r.record("u", "t2"); // t2 newest, unspent
        r.gc();
        // t1 (spent, not newest) dropped; t2 retained and claimable.
        match r.claim("u") {
            Slot::Grant { token, .. } => assert_eq!(token, "t2"),
            _ => panic!("expected t2"),
        }
    }

    #[test]
    fn gc_keeps_newest_even_if_spent() {
        let r = reg();
        r.record("u", "t1");
        for _ in 0..3 {
            r.claim("u");
        }
        r.gc();
        // Only one (spent) token, but it is the newest → retained.
        assert!(matches!(r.claim("u"), Slot::Exhausted { .. }));
    }
}
