//! The rate limiting middleware implementation.

use async_trait::async_trait;
use http::Extensions;
use rand::Rng;
use reqwest::{Request, Response};
use reqwest_middleware::{Middleware, Next, Result as MiddlewareResult};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::time::sleep;

use crate::builder::RateLimitBuilder;
use crate::error::RateLimitError;
use crate::gcra::GcraState;
use crate::types::{Route, ThrottleBehavior};

/// The rate limiting middleware.
///
/// This middleware tracks rate limits and either delays or rejects requests
/// based on the configured routes.
///
/// # Thread Safety
///
/// `RateLimitMiddleware` is `Send + Sync` and can be safely shared across
/// threads and async tasks. The internal state uses lock-free atomic operations
/// to ensure correct behavior under concurrent access. When cloned, clones
/// share the same rate limit state, so limits are enforced across all clones.
#[derive(Debug, Clone)]
pub struct RateLimitMiddleware {
    pub(crate) routes: Arc<[Route]>,
    pub(crate) states: Arc<[GcraState]>,
    pub(crate) route_offsets: Arc<[usize]>,
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
        // Stay in u64 to avoid u128 arithmetic on the hot path.
        // Saturating at ~585 years of uptime.
        let d = self.start_instant.elapsed();
        d.as_secs()
            .saturating_mul(1_000_000_000)
            .saturating_add(d.subsec_nanos() as u64)
    }

    /// Reset stale rate limit state entries that haven't been accessed recently.
    ///
    /// An entry is considered stale when its theoretical arrival time (TAT) has
    /// recovered past twice the limit window, meaning the burst capacity has been
    /// fully recovered for an extended period.
    ///
    /// Rate limit state is stored in a fixed-size pre-allocated array, so memory
    /// usage is constant regardless of traffic patterns. This method resets stale
    /// entries to their initial state, which can improve [`state_count`](Self::state_count)
    /// accuracy.
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
    /// // Call periodically to reset stale entries
    /// middleware.cleanup();
    /// # }
    /// ```
    pub fn cleanup(&self) {
        let now = self.now_nanos();
        for (route_index, route) in self.routes.iter().enumerate() {
            for (limit_index, limit) in route.limits.iter().enumerate() {
                let state = &self.states[self.route_offsets[route_index] + limit_index];
                let tat = state.tat(Ordering::Acquire);
                if tat > 0
                    && tat <= now.saturating_sub(limit.window_nanos.saturating_mul(2))
                {
                    state.reset();
                }
            }
        }
    }

    /// Returns the number of active rate limit state entries.
    ///
    /// An entry is considered active if it has been accessed at least once
    /// and has not been reset by [`cleanup`](Self::cleanup).
    #[must_use]
    pub fn state_count(&self) -> usize {
        self.states
            .iter()
            .filter(|s| s.tat(Ordering::Relaxed) > 0)
            .count()
    }

    /// Check and apply all rate limits for a request.
    #[doc(hidden)]
    pub async fn check_and_apply_limits(&self, req: &Request) -> Result<(), RateLimitError> {
        // Pre-extract URL components once before the route matching loop
        let url = req.url();
        let host = url.host_str();
        let method = req.method();
        let path = url.path();

        'outer: loop {
            let now = self.now_nanos();

            for (route_index, route) in self.routes.iter().enumerate() {
                if !route.matches_extracted(host, method, path) {
                    continue;
                }

                let offset = self.route_offsets[route_index];
                for (limit_index, limit) in route.limits.iter().enumerate() {
                    let state = &self.states[offset + limit_index];
                    let result =
                        state.try_acquire(now, limit.emission_interval_nanos, limit.window_nanos);

                    if let Err(wait_duration) = result {
                        self.handle_rate_limited(route, wait_duration).await?;
                        continue 'outer;
                    }
                }
            }

            // All limits passed
            break Ok(());
        }
    }

    #[cold]
    #[inline(never)]
    async fn handle_rate_limited(
        &self,
        route: &Route,
        wait_duration: Duration,
    ) -> Result<(), RateLimitError> {
        match route.on_limit {
            ThrottleBehavior::Delay => {
                // Add jitter (0-50% of wait duration) to prevent thundering herd
                let jitter_max_nanos = wait_duration.as_nanos() as u64 / 2;
                let jitter_nanos = if jitter_max_nanos > 0 {
                    rand::rng().random_range(0..=jitter_max_nanos)
                } else {
                    0
                };
                let sleep_duration =
                    wait_duration + Duration::from_nanos(jitter_nanos);
                sleep(sleep_duration).await;
                Ok(())
            }
            ThrottleBehavior::Error => Err(RateLimitError::RateLimited(wait_duration)),
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
