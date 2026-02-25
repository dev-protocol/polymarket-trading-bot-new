//! Core types for rate limit configuration.

use http::Method;
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
    #[cfg(test)]
    #[inline]
    pub(crate) fn matches(&self, req: &reqwest::Request) -> bool {
        self.matches_extracted(req.url().host_str(), req.method(), req.url().path())
    }

    /// Check if this route matches pre-extracted URL components.
    ///
    /// This avoids redundant URL component extraction when checking multiple routes.
    #[inline]
    pub(crate) fn matches_extracted(
        &self,
        req_host: Option<&str>,
        req_method: &Method,
        req_path: &str,
    ) -> bool {
        // Check host
        if let Some(ref host) = self.host {
            match req_host {
                Some(h) if h == host => {}
                _ => return false,
            }
        }

        // Check method
        if let Some(ref method) = self.method {
            if req_method != method {
                return false;
            }
        }

        // Check path prefix using segment boundaries:
        // - "/order" matches "/order", "/order/", "/order/123"
        // - "/order" does NOT match "/orders" or "/order-test"
        if !self.path_prefix.is_empty() {
            if !req_path.starts_with(&self.path_prefix) {
                return false;
            }
            let remaining = &req_path[self.path_prefix.len()..];
            if !remaining.is_empty() && !remaining.starts_with('/') {
                return false;
            }
        }

        true
    }
}
