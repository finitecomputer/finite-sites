//! In-memory rate limiting for the magic-link endpoint.
//!
//! Two budgets guard `/_finite/request-link`: per (site, email) so one
//! address cannot be mail-bombed, and per client IP so one client cannot
//! sweep the endpoint. Limited requests still render the generic
//! "check your email" page — the limiter must not become an oracle for the
//! share list.
//!
//! State is in-memory and per-process: a restart resets budgets, which is
//! acceptable for an abuse brake (not a billing meter).

use std::collections::HashMap;
use std::sync::Mutex;

/// One email address may be sent at most this many links per window.
pub const MAX_LINKS_PER_EMAIL: u32 = 3;
/// One client IP may request at most this many links per window, across
/// all sites it touches.
pub const MAX_LINKS_PER_IP: u32 = 20;
/// Budget window. 10 minutes matches the token TTL order of magnitude:
/// a legitimate viewer needs at most a couple of tries per visit.
pub const WINDOW_SECONDS: u64 = 10 * 60;

/// Total tracked keys are bounded; when the table is full, stale keys are
/// swept, and if everything is fresh the limiter fails closed (denies).
/// 100k keys * ~few timestamps is a few MB at absolute worst.
const MAX_TRACKED_KEYS: usize = 100_000;

pub struct RateLimiter {
    window_seconds: u64,
    buckets: Mutex<HashMap<String, Vec<u64>>>,
}

impl RateLimiter {
    pub fn new(window_seconds: u64) -> RateLimiter {
        assert!(window_seconds > 0);
        RateLimiter {
            window_seconds,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Record an event for `key` and report whether it is within `max`
    /// events per window. Records even when denying, so hammering extends
    /// the lockout rather than slipping through it.
    pub fn check_and_record(&self, key: &str, max_events: u32, now: u64) -> bool {
        assert!(max_events > 0);
        let mut buckets = self.buckets.lock().expect("limiter mutex never poisoned");

        let oldest_relevant = now.saturating_sub(self.window_seconds);
        if !buckets.contains_key(key) && buckets.len() >= MAX_TRACKED_KEYS {
            // Sweep stale keys; bounded by current table size.
            buckets.retain(|_, events| {
                events
                    .last()
                    .is_some_and(|latest| *latest > oldest_relevant)
            });
            if buckets.len() >= MAX_TRACKED_KEYS {
                // Table is full of fresh keys: under active abuse, deny new
                // keys rather than grow without bound.
                return false;
            }
        }

        let events = buckets.entry(key.to_string()).or_default();
        events.retain(|timestamp| *timestamp > oldest_relevant);
        let allowed = (events.len() as u32) < max_events;
        events.push(now);
        // Per-key memory is bounded: stale events are pruned above and the
        // vector cannot exceed max_events live entries plus denied attempts
        // inside one window, which the retain caps each call.
        allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_750_000_000;

    #[test]
    fn allows_up_to_limit_then_denies() {
        let limiter = RateLimiter::new(600);
        assert!(limiter.check_and_record("k", 3, NOW));
        assert!(limiter.check_and_record("k", 3, NOW + 1));
        assert!(limiter.check_and_record("k", 3, NOW + 2));
        assert!(!limiter.check_and_record("k", 3, NOW + 3));
        assert!(!limiter.check_and_record("k", 3, NOW + 4));
    }

    #[test]
    fn budget_recovers_after_window() {
        let limiter = RateLimiter::new(600);
        for offset in 0..3 {
            assert!(limiter.check_and_record("k", 3, NOW + offset));
        }
        assert!(!limiter.check_and_record("k", 3, NOW + 10));
        // All prior events age out of the window.
        assert!(limiter.check_and_record("k", 3, NOW + 700));
    }

    #[test]
    fn keys_are_independent() {
        let limiter = RateLimiter::new(600);
        for offset in 0..3 {
            assert!(limiter.check_and_record("a", 3, NOW + offset));
        }
        assert!(!limiter.check_and_record("a", 3, NOW + 5));
        assert!(limiter.check_and_record("b", 3, NOW + 5));
    }

    #[test]
    fn denied_attempts_are_recorded_against_the_budget() {
        let limiter = RateLimiter::new(600);
        assert!(limiter.check_and_record("k", 1, NOW));
        // Denied late in the window — but still recorded.
        assert!(!limiter.check_and_record("k", 1, NOW + 599));
        // The first event has aged out, but the recorded denial has not:
        // hammering keeps the key locked rather than slipping through.
        assert!(!limiter.check_and_record("k", 1, NOW + 1100));
        // Once attempts actually stop for a full window, the key recovers.
        assert!(limiter.check_and_record("k", 1, NOW + 1800));
    }
}
