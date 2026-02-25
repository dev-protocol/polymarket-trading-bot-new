//! The rate limiting middleware implementation.

use async_trait::async_trait;
use dashmap::DashMap;
use http::Extensions;
use rand::Rng;
use reqwest::{Request, Response};
use reqwest_middleware::{Middleware, Next, Result as MiddlewareResult};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tokio::time::sleep;

use crate::builder::RateLimitBuilder;
use crate::error::RateLimitError;
use crate::gcra::GcraState;
use crate::types::{Route, RouteKey, ThrottleBehavior};

/// The rate limiting middleware.
///
/// This middleware tracks rate limits and either delays or rejects requests
/// based on the configured routes.
///
/// # Thread Safety
///
/// `RateLimitMiddleware` is `Send + Sync` and can be safely shared across
/// threads and async tasks. The internal state uses lock-free atomic operations
/// (via [`DashMap`] and atomic integers) to ensure correct behavior under
/// concurrent access. When cloned, clones share the same rate limit state,
/// so limits are enforced across all clones.
#[derive(Debug, Clone)]
pub struct RateLimitMiddleware {
    pub(crate) routes: Arc<Vec<Route>>,
    pub(crate) state: Arc<DashMap<RouteKey, GcraState>>,
    pub(crate) start_instant: Instant,
}

impl RateLimitMiddleware {
    /// Create a new builder for configuring the middleware.
    #[must_use]
    pub fn builder() -> RateLimitBuilder {
        RateLimitBuilder::new()
    }

    #[inline]
    pub(crate) fn now_nanos(&self) -> u64 {
        // Use saturating conversion to prevent overflow on very long-running processes
        // (would require running for ~585 years to overflow)
        self.start_instant
            .elapsed()
            .as_nanos()
            .min(u64::MAX as u128) as u64
    }

    /// Remove stale rate limit state entries that haven't been accessed recently.
    ///
    /// An entry is considered stale when its theoretical arrival time (TAT) has
    /// recovered past twice the limit window, meaning the burst capacity has been
    /// fully recovered for an extended period.
    ///
    /// This method should be called periodically in long-running applications to
    /// prevent unbounded memory growth from accumulated state entries.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use route_ratelimit::RateLimitMiddleware;
    /// use std::time::Duration;
    ///
    /// # async fn example() {
    /// let middleware = RateLimitMiddleware::builder()
    ///     .route(|r| r.limit(100, Duration::from_secs(10)))
    ///     .build();
    ///
    /// // Call periodically to clean up stale entries
    /// middleware.cleanup();
    /// # }
    /// ```
    pub fn cleanup(&self) {
        let now = self.now_nanos();
        self.state.retain(|key, gcra_state| {
            // Bounds check to handle edge cases
            if key.route_index() >= self.routes.len() {
                return false;
            }
            let route = &self.routes[key.route_index()];
            if key.limit_index() >= route.limits.len() {
                return false;
            }

            let limit = &route.limits[key.limit_index()];
            let tat = gcra_state.tat(Ordering::Acquire);

            // Keep if TAT is within 2x window of now (recently active)
            // An entry with TAT far in the past has fully recovered and can be removed
            tat > now.saturating_sub(limit.window_nanos.saturating_mul(2))
        });
    }

    /// Returns the number of active rate limit state entries.
    ///
    /// This can be useful for monitoring memory usage.
    #[must_use]
    pub fn state_count(&self) -> usize {
        self.state.len()
    }

    /// Check and apply all rate limits for a request.
    #[doc(hidden)]
    pub async fn check_and_apply_limits(&self, req: &Request) -> Result<(), RateLimitError> {
        'outer: loop {
            let now = self.now_nanos();

            for (route_index, route) in self.routes.iter().enumerate() {
                if !route.matches(req) {
                    continue;
                }

                for (limit_index, limit) in route.limits.iter().enumerate() {
                    let key = RouteKey::new(route_index, limit_index);

                    let emission_interval_nanos = limit.emission_interval_nanos;
                    let limit_nanos = limit.window_nanos;

                    // Fast path: read lock only (allows concurrent readers on same shard)
                    let result = if let Some(state) = self.state.get(&key) {
                        state.try_acquire(now, emission_interval_nanos, limit_nanos)
                    } else {
                        // Cold path: first request for this route+limit, write lock needed
                        let state =
                            self.state.entry(key).or_insert_with(GcraState::new);
                        state.try_acquire(now, emission_interval_nanos, limit_nanos)
                    };

                    if let Err(wait_duration) = result {
                        match route.on_limit {
                            ThrottleBehavior::Delay => {
                                // Add jitter (0-50% of wait duration) to prevent thundering herd
                                let jitter_max_nanos = wait_duration.as_nanos() as u64 / 2;
                                let jitter_nanos = if jitter_max_nanos > 0 {
                                    rand::rng().random_range(0..=jitter_max_nanos)
                                } else {
                                    0
                                };
                                let sleep_duration = wait_duration
                                    + std::time::Duration::from_nanos(jitter_nanos);
                                sleep(sleep_duration).await;
                                // After sleeping, restart the entire check with fresh timestamp
                                continue 'outer;
                            }
                            ThrottleBehavior::Error => {
                                return Err(RateLimitError::RateLimited(wait_duration));
                            }
                        }
                    }
                }
            }

            // All limits passed, we can proceed
            break Ok(());
        }
    }
}

#[async_trait]
impl Middleware for RateLimitMiddleware {
    async fn handle(
        &self,
        req: Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> MiddlewareResult<Response> {
        // Check and apply rate limits
        self.check_and_apply_limits(&req).await?;

        // Proceed with the request
        next.run(req, extensions).await
    }
}

impl Default for RateLimitMiddleware {
    /// Create a middleware with no routes configured.
    ///
    /// All requests will pass through without any rate limiting.
    /// Use [`RateLimitMiddleware::builder()`] to configure routes.
    fn default() -> Self {
        Self::builder().build()
    }
}
