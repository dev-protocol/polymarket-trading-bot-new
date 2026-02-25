//! Route-based rate limiting middleware for reqwest.
//!
//! This crate provides a [`RateLimitMiddleware`] that can be used with
//! [`reqwest_middleware`] to enforce rate limits based on endpoint matching.
//!
//! # Features
//!
//! - **Endpoint matching**: Match requests by host, HTTP method, and path prefix
//! - **Multiple rate limits**: Stack burst and sustained limits on the same endpoint
//! - **Configurable behavior**: Choose to delay requests or return errors per endpoint
//! - **Lock-free performance**: Uses GCRA algorithm with atomic operations
//! - **Shared state**: Rate limits are tracked across all client clones
//!
//! # Route Matching Behavior
//!
//! Routes are checked in the order they are defined, and **all matching routes'
//! limits are applied**. This means you can layer general limits with specific ones:
//!
//! ```rust,no_run
//! use route_ratelimit::RateLimitMiddleware;
//! use std::time::Duration;
//!
//! let middleware = RateLimitMiddleware::builder()
//!     // General limit: 9000 requests per 10 seconds for all endpoints
//!     .host("api.example.com", |host| {
//!         host.route(|r| r.limit(9000, Duration::from_secs(10)))
//!             // Specific limit: /book endpoints also limited to 1500/10s
//!             // Both limits are enforced - a request to /book must pass BOTH
//!             .route(|r| r.path("/book").limit(1500, Duration::from_secs(10)))
//!     })
//!     .build();
//! ```
//!
//! # Host Matching
//!
//! Host matching uses only the hostname portion of the URL, **excluding the port**.
//! For example, `host("api.example.com")` will match `https://api.example.com:8443/path`.
//!
//! # Path Matching
//!
//! Path matching uses **segment boundaries**, not simple prefix matching:
//! - `/order` matches `/order`, `/order/`, and `/order/123`
//! - `/order` does **NOT** match `/orders` or `/order-test`
//!
//! # Example
//!
//! ```rust,no_run
//! use route_ratelimit::{Method, RateLimitMiddleware, ThrottleBehavior};
//! use reqwest_middleware::ClientBuilder;
//! use std::time::Duration;
//!
//! # async fn example() {
//! let middleware = RateLimitMiddleware::builder()
//!     // Configure routes by host for clean organization
//!     .host("clob.polymarket.com", |host| {
//!         host.route(|r| r.limit(9000, Duration::from_secs(10)))  // General limit
//!             .route(|r| r.path("/book").limit(1500, Duration::from_secs(10)))
//!             .route(|r| r.path("/price").limit(1500, Duration::from_secs(10)))
//!             .route(|r| {
//!                 r.method(Method::POST)
//!                     .path("/order")
//!                     .limit(3500, Duration::from_secs(10))   // Burst
//!                     .limit(36000, Duration::from_secs(600)) // Sustained
//!             })
//!     })
//!     .build();
//!
//! let client = ClientBuilder::new(reqwest::Client::new())
//!     .with(middleware)
//!     .build();
//!
//! // Requests will be automatically rate-limited
//! client.get("https://clob.polymarket.com/book").send().await.unwrap();
//! # }
//! ```

mod builder;
mod error;
mod gcra;
mod middleware;
mod types;

// Public re-exports
pub use builder::{HostBuilder, HostRouteBuilder, RateLimitBuilder, RouteBuilder};
pub use error::RateLimitError;
pub use http::Method;
pub use middleware::RateLimitMiddleware;
pub use types::{RateLimit, Route, ThrottleBehavior};

#[cfg(test)]
mod tests {
    use super::*;
    use http::Method;
    use std::time::Duration;

    #[test]
    fn test_route_matching_all() {
        let route = Route {
            host: None,
            method: None,
            path_prefix: String::new(),
            limits: vec![],
            on_limit: ThrottleBehavior::Delay,
        };

        let req = reqwest::Client::new()
            .get("https://example.com/test")
            .build()
            .unwrap();

        assert!(route.matches(&req));
    }

    #[test]
    fn test_route_matching_host() {
        let route = Route {
            host: Some("api.example.com".to_string()),
            method: None,
            path_prefix: String::new(),
            limits: vec![],
            on_limit: ThrottleBehavior::Delay,
        };

        let req_match = reqwest::Client::new()
            .get("https://api.example.com/test")
            .build()
            .unwrap();
        let req_no_match = reqwest::Client::new()
            .get("https://other.example.com/test")
            .build()
            .unwrap();

        assert!(route.matches(&req_match));
        assert!(!route.matches(&req_no_match));
    }

    #[test]
    fn test_route_matching_method() {
        let route = Route {
            host: None,
            method: Some(Method::POST),
            path_prefix: String::new(),
            limits: vec![],
            on_limit: ThrottleBehavior::Delay,
        };

        let req_match = reqwest::Client::new()
            .post("https://example.com/test")
            .build()
            .unwrap();
        let req_no_match = reqwest::Client::new()
            .get("https://example.com/test")
            .build()
            .unwrap();

        assert!(route.matches(&req_match));
        assert!(!route.matches(&req_no_match));
    }

    #[test]
    fn test_route_matching_path_prefix() {
        let route = Route {
            host: None,
            method: None,
            path_prefix: "/api/v1".to_string(),
            limits: vec![],
            on_limit: ThrottleBehavior::Delay,
        };

        let req_match = reqwest::Client::new()
            .get("https://example.com/api/v1/users")
            .build()
            .unwrap();
        let req_no_match = reqwest::Client::new()
            .get("https://example.com/api/v2/users")
            .build()
            .unwrap();

        assert!(route.matches(&req_match));
        assert!(!route.matches(&req_no_match));
    }

    #[test]
    fn test_route_matching_path_segment_boundary() {
        let route = Route {
            host: None,
            method: None,
            path_prefix: "/order".to_string(),
            limits: vec![],
            on_limit: ThrottleBehavior::Delay,
        };

        // Should match: exact, with trailing slash, with sub-path
        let req_exact = reqwest::Client::new()
            .get("https://example.com/order")
            .build()
            .unwrap();
        let req_trailing = reqwest::Client::new()
            .get("https://example.com/order/")
            .build()
            .unwrap();
        let req_subpath = reqwest::Client::new()
            .get("https://example.com/order/123")
            .build()
            .unwrap();

        assert!(route.matches(&req_exact), "/order should match /order");
        assert!(route.matches(&req_trailing), "/order should match /order/");
        assert!(
            route.matches(&req_subpath),
            "/order should match /order/123"
        );

        // Should NOT match: different path that starts with same chars
        let req_orders = reqwest::Client::new()
            .get("https://example.com/orders")
            .build()
            .unwrap();
        let req_order_dash = reqwest::Client::new()
            .get("https://example.com/order-test")
            .build()
            .unwrap();

        assert!(
            !route.matches(&req_orders),
            "/order should NOT match /orders"
        );
        assert!(
            !route.matches(&req_order_dash),
            "/order should NOT match /order-test"
        );
    }

    #[test]
    fn test_emission_interval() {
        let limit = RateLimit::new(100, Duration::from_secs(10));
        assert_eq!(limit.emission_interval(), Duration::from_millis(100));

        let limit = RateLimit::new(1000, Duration::from_secs(60));
        assert_eq!(limit.emission_interval(), Duration::from_millis(60));
    }

    #[test]
    #[should_panic(expected = "requests must be greater than 0")]
    fn test_zero_requests_panics() {
        RateLimit::new(0, Duration::from_secs(10));
    }

    #[test]
    #[should_panic(expected = "window must be greater than 0")]
    fn test_zero_window_panics() {
        RateLimit::new(100, Duration::ZERO);
    }

    #[test]
    #[should_panic(expected = "window must not exceed u64::MAX nanoseconds")]
    fn test_overflow_window_panics() {
        // u64::MAX nanoseconds is ~585 years, so 600 years should overflow
        RateLimit::new(100, Duration::from_secs(600 * 365 * 24 * 60 * 60));
    }
}
