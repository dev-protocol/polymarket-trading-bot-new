//! Builder API for configuring the rate limiting middleware.

use http::Method;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::gcra::GcraState;
use crate::middleware::RateLimitMiddleware;
use crate::types::{RateLimit, Route, ThrottleBehavior};

/// Builder for configuring a [`RateLimitMiddleware`].
#[derive(Debug, Default, Clone)]
pub struct RateLimitBuilder {
    pub(crate) routes: Vec<Route>,
}

impl RateLimitBuilder {
    /// Create a new builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a route using a closure-based configuration.
    ///
    /// # Panics
    ///
    /// Panics if no limits are configured via `.limit()`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use route_ratelimit::RateLimitMiddleware;
    /// use std::time::Duration;
    ///
    /// let middleware = RateLimitMiddleware::builder()
    ///     .route(|r| r.limit(15000, Duration::from_secs(10)))
    ///     .route(|r| r.path("/api").limit(1000, Duration::from_secs(10)))
    ///     .build();
    /// ```
    #[must_use]
    pub fn route<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(RouteBuilder) -> RouteBuilder,
    {
        let builder = RouteBuilder::new();
        let configured = configure(builder);
        self.routes.push(configured.build());
        self
    }

    /// Configure routes for a specific host using a scoped builder.
    ///
    /// This is the preferred way to configure multiple routes for the same host,
    /// as it avoids repeating the host for each route.
    ///
    /// # Example
    ///
    /// ```rust
    /// use route_ratelimit::{RateLimitMiddleware, ThrottleBehavior};
    /// use std::time::Duration;
    /// use http::Method;
    ///
    /// let middleware = RateLimitMiddleware::builder()
    ///     .host("clob.polymarket.com", |host| {
    ///         host
    ///             .route(|r| r.limit(9000, Duration::from_secs(10)))
    ///             .route(|r| r.path("/book").limit(1500, Duration::from_secs(10)))
    ///             .route(|r| r.path("/price").limit(1500, Duration::from_secs(10)))
    ///             .route(|r| {
    ///                 r.method(Method::POST)
    ///                     .path("/order")
    ///                     .limit(3500, Duration::from_secs(10))
    ///                     .limit(36000, Duration::from_secs(600))
    ///             })
    ///     })
    ///     .host("data-api.polymarket.com", |host| {
    ///         host
    ///             .route(|r| r.limit(1000, Duration::from_secs(10)))
    ///             .route(|r| r.path("/trades").limit(200, Duration::from_secs(10)))
    ///     })
    ///     .build();
    /// ```
    #[must_use]
    pub fn host<F>(mut self, host: impl Into<String>, configure: F) -> Self
    where
        F: FnOnce(HostBuilder) -> HostBuilder,
    {
        let host_str = host.into();
        let host_builder = HostBuilder::new(host_str);
        let configured = configure(host_builder);
        self.routes.extend(configured.routes);
        self
    }

    /// Add a pre-configured route built via [`RouteBuilder::build`].
    ///
    /// # Panics
    ///
    /// Panics if the route has no limits configured.
    #[must_use]
    pub fn add_route(mut self, route: Route) -> Self {
        assert!(
            !route.limits.is_empty(),
            "route must have at least one limit configured via .limit()"
        );
        self.routes.push(route);
        self
    }

    /// Build the middleware.
    ///
    /// # Warnings
    ///
    /// If the `tracing` feature is enabled, this method will emit a warning
    /// when catch-all routes (routes with no host, method, or path filters)
    /// are followed by more specific routes. This pattern may cause unexpected
    /// behavior since all matching routes' limits are applied.
    #[must_use]
    pub fn build(self) -> RateLimitMiddleware {
        #[cfg(feature = "tracing")]
        self.warn_catch_all_route_order();

        // Precompute flat offsets for each route's state entries
        let mut offsets = Vec::with_capacity(self.routes.len());
        let mut total = 0usize;
        for route in &self.routes {
            offsets.push(total);
            total += route.limits.len();
        }

        let states: Vec<GcraState> = (0..total).map(|_| GcraState::new()).collect();

        RateLimitMiddleware {
            routes: Arc::from(self.routes),
            states: Arc::from(states),
            route_offsets: Arc::from(offsets),
            start_instant: Instant::now(),
        }
    }

    /// Emit a warning if catch-all routes precede more specific routes.
    #[cfg(feature = "tracing")]
    fn warn_catch_all_route_order(&self) {
        let mut last_catch_all = None;
        for (i, route) in self.routes.iter().enumerate() {
            if route.is_catch_all() {
                last_catch_all = Some(i);
            } else if let Some(catch_all_index) = last_catch_all {
                tracing::warn!(
                    catch_all_route_index = catch_all_index,
                    specific_route_index = i,
                    "Catch-all route (index {}) precedes more specific route (index {}). \
                     All matching routes' limits are applied, so the catch-all will affect \
                     requests intended for the specific route. Consider reordering routes \
                     or using host-scoped builders.",
                    catch_all_index,
                    i
                );
                last_catch_all = None;
            }
        }
    }
}

/// Builder for configuring routes within a specific host scope.
///
/// Created by [`RateLimitBuilder::host`]. All routes created within this builder
/// will automatically have the host set.
#[derive(Debug, Clone)]
pub struct HostBuilder {
    host: String,
    routes: Vec<Route>,
}

impl HostBuilder {
    fn new(host: String) -> Self {
        Self {
            host,
            routes: Vec::new(),
        }
    }

    /// Add a route within this host using a closure-based configuration.
    ///
    /// The host is automatically set for each route.
    ///
    /// # Panics
    ///
    /// Panics if no limits are configured via `.limit()`.
    #[must_use]
    pub fn route<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(HostRouteBuilder) -> HostRouteBuilder,
    {
        let builder = HostRouteBuilder::new();
        let configured = configure(builder);
        assert!(
            !configured.limits.is_empty(),
            "route must have at least one limit configured via .limit()"
        );
        let route = Route {
            host: Some(self.host.clone()),
            method: configured.method,
            path_prefix: configured.path_prefix,
            limits: configured.limits,
            on_limit: configured.on_limit,
        };
        self.routes.push(route);
        self
    }
}

/// Builder for configuring a single route within a host scope.
///
/// Created by [`HostBuilder::route`] closure. Configure the route and the
/// closure will automatically add it to the host.
#[derive(Debug, Default, Clone)]
pub struct HostRouteBuilder {
    method: Option<Method>,
    path_prefix: String,
    limits: Vec<RateLimit>,
    on_limit: ThrottleBehavior,
}

impl HostRouteBuilder {
    fn new() -> Self {
        Self::default()
    }

    /// Set the HTTP method to match.
    #[must_use]
    pub fn method(mut self, method: Method) -> Self {
        self.method = Some(method);
        self
    }

    /// Set the path prefix to match (e.g., "/order").
    #[must_use]
    pub fn path(mut self, path_prefix: impl Into<String>) -> Self {
        self.path_prefix = path_prefix.into();
        self
    }

    /// Add a rate limit.
    #[must_use]
    pub fn limit(mut self, requests: u32, window: Duration) -> Self {
        self.limits.push(RateLimit::new(requests, window));
        self
    }

    /// Set the behavior when rate limit is exceeded.
    ///
    /// Defaults to [`ThrottleBehavior::Delay`] if not called.
    #[must_use]
    pub fn on_limit(mut self, behavior: ThrottleBehavior) -> Self {
        self.on_limit = behavior;
        self
    }
}

/// Builder for configuring a single route (without host scope).
///
/// Created by [`RateLimitBuilder::route`] closure. Configure the route and
/// the closure will automatically add it to the middleware.
#[derive(Debug, Default, Clone)]
pub struct RouteBuilder {
    host: Option<String>,
    method: Option<Method>,
    path_prefix: String,
    limits: Vec<RateLimit>,
    on_limit: ThrottleBehavior,
}

impl RouteBuilder {
    fn new() -> Self {
        Self::default()
    }

    /// Build the route.
    ///
    /// Returns a [`Route`] that can be passed to [`RateLimitBuilder::add_route`].
    ///
    /// # Panics
    ///
    /// Panics if no limits are configured via `.limit()`.
    #[must_use]
    pub fn build(self) -> Route {
        assert!(
            !self.limits.is_empty(),
            "route must have at least one limit configured via .limit()"
        );
        Route {
            host: self.host,
            method: self.method,
            path_prefix: self.path_prefix,
            limits: self.limits,
            on_limit: self.on_limit,
        }
    }

    /// Set the host to match (e.g., "api.example.com").
    ///
    /// Note: Consider using [`RateLimitBuilder::host`] instead if you're
    /// configuring multiple routes for the same host.
    #[must_use]
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    /// Set the HTTP method to match.
    #[must_use]
    pub fn method(mut self, method: Method) -> Self {
        self.method = Some(method);
        self
    }

    /// Set the path prefix to match (e.g., "/order").
    #[must_use]
    pub fn path(mut self, path_prefix: impl Into<String>) -> Self {
        self.path_prefix = path_prefix.into();
        self
    }

    /// Add a rate limit.
    #[must_use]
    pub fn limit(mut self, requests: u32, window: Duration) -> Self {
        self.limits.push(RateLimit::new(requests, window));
        self
    }

    /// Set the behavior when rate limit is exceeded.
    ///
    /// Defaults to [`ThrottleBehavior::Delay`] if not called.
    #[must_use]
    pub fn on_limit(mut self, behavior: ThrottleBehavior) -> Self {
        self.on_limit = behavior;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_api() {
        let middleware = RateLimitMiddleware::builder()
            .route(|r| {
                r.host("api.example.com")
                    .method(Method::POST)
                    .path("/order")
                    .limit(100, Duration::from_secs(10))
                    .limit(1000, Duration::from_secs(60))
                    .on_limit(ThrottleBehavior::Delay)
            })
            .route(|r| {
                r.path("/data")
                    .limit(50, Duration::from_secs(10))
                    .on_limit(ThrottleBehavior::Error)
            })
            .build();

        assert_eq!(middleware.routes.len(), 2);
        assert_eq!(middleware.routes[0].limits.len(), 2);
        assert_eq!(middleware.routes[1].limits.len(), 1);
    }

    #[test]
    fn test_host_scoped_builder() {
        let middleware = RateLimitMiddleware::builder()
            .host("clob.polymarket.com", |host| {
                host.route(|r| r.limit(9000, Duration::from_secs(10)))
                    .route(|r| r.path("/book").limit(1500, Duration::from_secs(10)))
                    .route(|r| r.path("/price").limit(1500, Duration::from_secs(10)))
                    .route(|r| {
                        r.method(Method::POST)
                            .path("/order")
                            .limit(3500, Duration::from_secs(10))
                            .limit(36000, Duration::from_secs(600))
                            .on_limit(ThrottleBehavior::Delay)
                    })
            })
            .host("data-api.polymarket.com", |host| {
                host.route(|r| r.limit(1000, Duration::from_secs(10)))
                    .route(|r| r.path("/trades").limit(200, Duration::from_secs(10)))
            })
            .build();

        // 4 routes for CLOB + 2 routes for Data API = 6 routes
        assert_eq!(middleware.routes.len(), 6);

        // Check that all CLOB routes have the correct host
        for i in 0..4 {
            assert_eq!(
                middleware.routes[i].host.as_deref(),
                Some("clob.polymarket.com")
            );
        }

        // Check that all Data API routes have the correct host
        for i in 4..6 {
            assert_eq!(
                middleware.routes[i].host.as_deref(),
                Some("data-api.polymarket.com")
            );
        }

        // Check the trading endpoint has burst + sustained limits
        assert_eq!(middleware.routes[3].path_prefix, "/order");
        assert_eq!(middleware.routes[3].method, Some(Method::POST));
        assert_eq!(middleware.routes[3].limits.len(), 2);
    }

    #[test]
    fn test_mixed_builder_styles() {
        // Can mix host-scoped and non-scoped routes
        let middleware = RateLimitMiddleware::builder()
            // Global catch-all limit (no host)
            .route(|r| r.limit(15000, Duration::from_secs(10)))
            // Host-scoped routes
            .host("api.example.com", |host| {
                host.route(|r| r.path("/data").limit(100, Duration::from_secs(10)))
            })
            .build();

        assert_eq!(middleware.routes.len(), 2);
        assert!(middleware.routes[0].host.is_none()); // Global route
        assert_eq!(
            middleware.routes[1].host.as_deref(),
            Some("api.example.com")
        );
    }

    #[test]
    fn test_single_line_routes() {
        // Demonstrate rustfmt-friendly one-line route syntax
        let middleware = RateLimitMiddleware::builder()
            .host("api.example.com", |host| {
                host.route(|r| r.path("/a").limit(100, Duration::from_secs(10)))
                    .route(|r| r.path("/b").limit(200, Duration::from_secs(10)))
                    .route(|r| r.path("/c").limit(300, Duration::from_secs(10)))
            })
            .build();

        assert_eq!(middleware.routes.len(), 3);
    }

    #[test]
    #[should_panic(expected = "route must have at least one limit")]
    fn test_route_without_limit_panics() {
        let _middleware = RateLimitMiddleware::builder()
            .route(|r| r.path("/test"))
            .build();
    }

    #[test]
    #[should_panic(expected = "route must have at least one limit")]
    fn test_host_route_without_limit_panics() {
        let _middleware = RateLimitMiddleware::builder()
            .host("api.example.com", |host| host.route(|r| r.path("/test")))
            .build();
    }
}
