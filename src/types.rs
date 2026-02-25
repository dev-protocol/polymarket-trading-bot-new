//! Core types for rate limit configuration.

use http::Method;
use reqwest::Request;
use std::time::Duration;

/// Behavior when a rate limit is exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThrottleBehavior {
    /// Delay the request until the rate limit window allows it.
    #[default]
    Delay,
    /// Return an error immediately.
    Error,
}

/// A single rate limit configuration.
#[derive(Debug, Clone)]
pub struct RateLimit {
    /// Maximum number of requests allowed in the window.
    pub requests: u32,
    /// Time window for the rate limit.
    pub window: Duration,
    /// Precomputed emission interval in nanoseconds (window / requests).
    pub(crate) emission_interval_nanos: u64,
    /// Precomputed window duration in nanoseconds.
    pub(crate) window_nanos: u64,
}

impl RateLimit {
    /// Create a new rate limit.
    ///
    /// # Panics
    ///
    /// Panics if:
    /// - `requests` is 0
    /// - `window` is zero
    /// - `window` exceeds `u64::MAX` nanoseconds (~585 years)
    pub fn new(requests: u32, window: Duration) -> Self {
        assert!(requests > 0, "requests must be greater than 0");
        assert!(!window.is_zero(), "window must be greater than 0");
        assert!(
            window.as_nanos() <= u64::MAX as u128,
            "window must not exceed u64::MAX nanoseconds (~585 years)"
        );
        let window_nanos = window.as_nanos() as u64;
        let emission_interval_nanos = (window / requests).as_nanos() as u64;
        Self {
            requests,
            window,
            emission_interval_nanos,
            window_nanos,
        }
    }

    /// Calculate the emission interval (time between requests).
    #[cfg(test)]
    pub(crate) fn emission_interval(&self) -> Duration {
        self.window / self.requests
    }
}

/// A route definition that matches requests and applies rate limits.
#[derive(Debug, Clone)]
pub struct Route {
    /// Optional host to match (e.g., "api.example.com").
    pub host: Option<String>,
    /// Optional HTTP method to match.
    pub method: Option<Method>,
    /// Path prefix to match (e.g., "/order"). Empty matches all paths.
    pub path_prefix: String,
    /// Rate limits to apply (all must pass).
    pub limits: Vec<RateLimit>,
    /// Behavior when rate limit is exceeded.
    pub on_limit: ThrottleBehavior,
}

impl Route {
    /// Returns `true` if this route has no filters (matches all requests).
    ///
    /// A catch-all route has no host, no method, and no path prefix constraints.
    #[cfg(feature = "tracing")]
    #[inline]
    pub(crate) fn is_catch_all(&self) -> bool {
        self.host.is_none() && self.method.is_none() && self.path_prefix.is_empty()
    }

    /// Check if this route matches a request.
    #[inline]
    pub(crate) fn matches(&self, req: &Request) -> bool {
        // Check host
        if let Some(ref host) = self.host {
            if let Some(req_host) = req.url().host_str() {
                if req_host != host {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Check method
        if let Some(ref method) = self.method {
            if req.method() != method {
                return false;
            }
        }

        // Check path prefix
        // Path prefix matching uses path segment boundaries:
        // - "/order" matches "/order", "/order/", "/order/123"
        // - "/order" does NOT match "/orders" or "/order-test"
        if !self.path_prefix.is_empty() {
            let path = req.url().path();
            if !path.starts_with(&self.path_prefix) {
                return false;
            }
            // Ensure we're matching at a path segment boundary
            let remaining = &path[self.path_prefix.len()..];
            if !remaining.is_empty() && !remaining.starts_with('/') {
                return false;
            }
        }

        true
    }
}

/// Unique key for a route's rate limit state.
///
/// Packed as a single `u64`: upper 32 bits = route_index, lower 32 bits = limit_index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct RouteKey(u64);

impl RouteKey {
    #[inline]
    pub fn new(route_index: usize, limit_index: usize) -> Self {
        debug_assert!(route_index <= u32::MAX as usize);
        debug_assert!(limit_index <= u32::MAX as usize);
        Self((route_index as u64) << 32 | (limit_index as u64))
    }

    #[inline]
    pub fn route_index(self) -> usize {
        (self.0 >> 32) as usize
    }

    #[inline]
    pub fn limit_index(self) -> usize {
        (self.0 & 0xFFFF_FFFF) as usize
    }
}
