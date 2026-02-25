//! GCRA (Generic Cell Rate Algorithm) implementation.
//!
//! The GCRA algorithm provides a smooth rate limiting mechanism that naturally
//! supports burst capacity while maintaining a sustained rate limit.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// GCRA (Generic Cell Rate Algorithm) state.
///
/// Stores the theoretical arrival time (TAT) as nanoseconds since epoch.
/// Uses atomic operations for lock-free concurrent access.
#[derive(Debug)]
pub(crate) struct GcraState {
    /// Theoretical arrival time in nanoseconds since the start instant.
    tat_nanos: AtomicU64,
}

impl GcraState {
    /// Create a new GCRA state.
    #[inline]
    pub fn new() -> Self {
        Self {
            tat_nanos: AtomicU64::new(0),
        }
    }

    /// Get the current theoretical arrival time (TAT) in nanoseconds.
    #[inline]
    pub fn tat(&self, ordering: Ordering) -> u64 {
        self.tat_nanos.load(ordering)
    }

    /// Try to acquire a token. Returns Ok(()) if allowed, or Err(wait_duration) if rate limited.
    #[inline]
    pub fn try_acquire(
        &self,
        now_nanos: u64,
        emission_interval_nanos: u64,
        limit_nanos: u64,
    ) -> Result<(), Duration> {
        loop {
            let tat = self.tat_nanos.load(Ordering::Acquire);

            // Calculate new TAT using saturating arithmetic to prevent overflow
            let new_tat = if tat <= now_nanos {
                // No pending requests, start fresh
                now_nanos.saturating_add(emission_interval_nanos)
            } else {
                // Add to the queue
                tat.saturating_add(emission_interval_nanos)
            };

            // Check if new TAT exceeds the limit (burst capacity exhausted)
            let limit_at = now_nanos.saturating_add(limit_nanos);
            if new_tat > limit_at {
                // Rate limited - calculate how long to wait
                let wait_nanos = new_tat.saturating_sub(limit_at);
                return Err(Duration::from_nanos(wait_nanos));
            }

            // Try to update TAT atomically
            match self.tat_nanos.compare_exchange_weak(
                tat,
                new_tat,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(_) => continue, // Retry on contention
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gcra_allows_burst() {
        let state = GcraState::new();
        let emission_interval = Duration::from_millis(100); // 10 req/s
        let window = Duration::from_secs(1);

        let now = 0u64;
        let emission_nanos = emission_interval.as_nanos() as u64;
        let limit_nanos = window.as_nanos() as u64;

        // Should allow up to 10 requests immediately (burst)
        for _ in 0..10 {
            assert!(state.try_acquire(now, emission_nanos, limit_nanos).is_ok());
        }

        // 11th request should be rate limited
        assert!(state.try_acquire(now, emission_nanos, limit_nanos).is_err());
    }

    #[test]
    fn test_gcra_recovers_after_time() {
        let state = GcraState::new();
        let emission_interval = Duration::from_millis(100);
        let window = Duration::from_secs(1);

        let emission_nanos = emission_interval.as_nanos() as u64;
        let limit_nanos = window.as_nanos() as u64;

        // Exhaust the burst at t=0
        let now = 0u64;
        for _ in 0..10 {
            let _ = state.try_acquire(now, emission_nanos, limit_nanos);
        }

        // After 100ms, one more request should be allowed
        let now = Duration::from_millis(100).as_nanos() as u64;
        assert!(state.try_acquire(now, emission_nanos, limit_nanos).is_ok());
    }
}
