//! The rate limiting middleware implementation.

use async_trait::async_trait;
use http::Extensions;
use rand::Rng;
use reqwest::{Request, Response};
use reqwest_middleware::{Middleware, Next, Result as MiddlewareResult};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;

use crate::builder::RateLimitBuilder;
use crate::error::RateLimitError;
use crate::gcra::GcraState;
use crate::types::{Route, ThrottleBehavior};

/// Jitter adds up to 50% of the wait duration (denominator of the fraction).
const JITTER_FRACTION_DENOM: u128 = 2;

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

    /// Check and apply all rate limits for a request.
    #[doc(hidden)]
    pub async fn check_and_apply_limits(&self, req: &Request) -> Result<(), RateLimitError> {
        // Pre-extract URL components once before the route matching loop
        let url = req.url();
        let host = url.host_str();
        let method = req.method();
        let path = url.path();

        // Track position to avoid double-counting tokens from already-passed limits.
        // On delay retry, we resume from the exact limit that failed rather than
        // restarting from the beginning (which would re-consume tokens).
        let mut route_idx = 0;
        let mut limit_start = 0;

        while route_idx < self.routes.len() {
            let now = self.now_nanos();
            let route = &self.routes[route_idx];

            if !route.matches_extracted(host, method, path) {
                route_idx += 1;
                limit_start = 0;
                continue;
            }

            let offset = self.route_offsets[route_idx];
            let mut limit_idx = limit_start;
            let mut all_passed = true;

            while limit_idx < route.limits.len() {
                let limit = &route.limits[limit_idx];
                let state = &self.states[offset + limit_idx];

                if let Err(wait_duration) =
                    state.try_acquire(now, limit.emission_interval_nanos, limit.window_nanos)
                {
                    self.handle_rate_limited(route, wait_duration).await?;
                    limit_start = limit_idx; // retry from this limit
                    all_passed = false;
                    break;
                }
                limit_idx += 1;
            }

            if all_passed {
                route_idx += 1;
                limit_start = 0;
            }
        }

        Ok(())
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
                // Add jitter to prevent thundering herd
                let jitter_max_nanos =
                    u64::try_from(wait_duration.as_nanos() / JITTER_FRACTION_DENOM)
                        .unwrap_or(u64::MAX);
                let jitter_nanos = if jitter_max_nanos > 0 {
                    rand::rng().random_range(0..=jitter_max_nanos)
                } else {
                    0
                };
                let sleep_duration = wait_duration + Duration::from_nanos(jitter_nanos);
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
