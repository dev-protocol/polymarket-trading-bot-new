//! Integration tests for route-ratelimit middleware.
//!
//! These tests use wiremock to create realistic HTTP scenarios and verify
//! that rate limiting works correctly end-to-end.

use http::Method;
use reqwest_middleware::ClientBuilder;
use route_ratelimit::{RateLimitMiddleware, ThrottleBehavior};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use wiremock::matchers::{any, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn create_client(middleware: RateLimitMiddleware) -> reqwest_middleware::ClientWithMiddleware {
    ClientBuilder::new(reqwest::Client::new())
        .with(middleware)
        .build()
}

/// Helper to create a mock server with a simple OK response.
async fn setup_mock_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/test"))
        .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/order"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Order placed"))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/order"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Order cancelled"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Data"))
        .mount(&server)
        .await;
    server
}

// =============================================================================
// Error Behavior Tests
// =============================================================================

#[tokio::test]
async fn test_error_on_rate_limit_exceeded() {
    let server = setup_mock_server().await;

    // Create middleware with a very low limit and Error behavior
    let middleware = RateLimitMiddleware::builder()
        .route(|r| {
            r.limit(2, Duration::from_secs(10))
                .on_limit(ThrottleBehavior::Error)
        })
        .build();

    let client = create_client(middleware);

    let url = format!("{}/test", server.uri());

    // First 2 requests should succeed
    let resp1 = client.get(&url).send().await;
    assert!(resp1.is_ok(), "First request should succeed");

    let resp2 = client.get(&url).send().await;
    assert!(resp2.is_ok(), "Second request should succeed");

    // Third request should fail with rate limit error
    let resp3 = client.get(&url).send().await;
    assert!(
        resp3.is_err(),
        "Third request should fail due to rate limit"
    );

    let err = resp3.unwrap_err();
    assert!(
        err.to_string().contains("rate limit exceeded"),
        "Error should mention rate limit: {err}"
    );
}

#[tokio::test]
async fn test_error_includes_retry_duration() {
    let server = setup_mock_server().await;

    let middleware = RateLimitMiddleware::builder()
        .route(|r| {
            r.limit(1, Duration::from_millis(100))
                .on_limit(ThrottleBehavior::Error)
        })
        .build();

    let client = create_client(middleware);

    let url = format!("{}/test", server.uri());

    // First request succeeds
    client.get(&url).send().await.unwrap();

    // Second request fails with retry info
    let err = client.get(&url).send().await.unwrap_err();
    let err_str = err.to_string();
    assert!(
        err_str.contains("retry after"),
        "Error should include retry timing: {err_str}"
    );
}

// =============================================================================
// Delay Behavior Tests
// =============================================================================

#[tokio::test]
async fn test_delay_on_rate_limit_exceeded() {
    let server = setup_mock_server().await;

    // Create middleware with Delay behavior
    let middleware = RateLimitMiddleware::builder()
        .route(|r| {
            r.limit(2, Duration::from_millis(200))
                .on_limit(ThrottleBehavior::Delay)
        })
        .build();

    let client = create_client(middleware);

    let url = format!("{}/test", server.uri());

    let start = Instant::now();

    // Make 4 requests - first 2 are burst, next 2 should be delayed
    for i in 0..4 {
        let resp = client.get(&url).send().await;
        assert!(
            resp.is_ok(),
            "Request {i} should succeed (possibly after delay)"
        );
    }

    let elapsed = start.elapsed();

    // With 2 burst and 100ms emission interval, 4 requests should take ~200ms
    // (2 burst immediate, then wait ~100ms for 3rd, ~100ms for 4th)
    assert!(
        elapsed >= Duration::from_millis(150),
        "Should have waited for rate limit: {elapsed:?}"
    );
}

#[tokio::test]
async fn test_delay_does_not_lose_requests() {
    let server = setup_mock_server().await;
    let request_count = Arc::new(AtomicUsize::new(0));

    Mock::given(method("GET"))
        .and(path("/counted"))
        .respond_with({
            let count = request_count.clone();
            move |_: &wiremock::Request| {
                count.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200)
            }
        })
        .mount(&server)
        .await;

    let middleware = RateLimitMiddleware::builder()
        .route(|r| {
            r.limit(2, Duration::from_millis(100))
                .on_limit(ThrottleBehavior::Delay)
        })
        .build();

    let client = create_client(middleware);

    let url = format!("{}/counted", server.uri());

    // Send 5 requests
    for _ in 0..5 {
        client.get(&url).send().await.unwrap();
    }

    // All 5 should have made it to the server
    assert_eq!(
        request_count.load(Ordering::SeqCst),
        5,
        "All delayed requests should eventually complete"
    );
}

// =============================================================================
// Route Matching Tests
// =============================================================================

#[tokio::test]
async fn test_different_routes_have_separate_limits() {
    let server = setup_mock_server().await;

    let middleware = RateLimitMiddleware::builder()
        .route(|r| {
            r.path("/test")
                .limit(2, Duration::from_secs(10))
                .on_limit(ThrottleBehavior::Error)
        })
        .route(|r| {
            r.path("/data")
                .limit(2, Duration::from_secs(10))
                .on_limit(ThrottleBehavior::Error)
        })
        .build();

    let client = create_client(middleware);

    // Exhaust /test limit
    client
        .get(format!("{}/test", server.uri()))
        .send()
        .await
        .unwrap();
    client
        .get(format!("{}/test", server.uri()))
        .send()
        .await
        .unwrap();

    // /data should still work (separate limit)
    let resp = client.get(format!("{}/data", server.uri())).send().await;
    assert!(resp.is_ok(), "/data should have its own limit");

    // /test should fail now
    let resp = client.get(format!("{}/test", server.uri())).send().await;
    assert!(resp.is_err(), "/test should be rate limited");
}

#[tokio::test]
async fn test_method_specific_limits() {
    let server = setup_mock_server().await;

    let middleware = RateLimitMiddleware::builder()
        .route(|r| {
            r.method(Method::POST)
                .path("/order")
                .limit(1, Duration::from_secs(10))
                .on_limit(ThrottleBehavior::Error)
        })
        .route(|r| {
            r.method(Method::DELETE)
                .path("/order")
                .limit(1, Duration::from_secs(10))
                .on_limit(ThrottleBehavior::Error)
        })
        .build();

    let client = create_client(middleware);

    let order_url = format!("{}/order", server.uri());

    // POST and DELETE have separate limits
    client.post(&order_url).send().await.unwrap();
    client.delete(&order_url).send().await.unwrap();

    // Second POST should fail
    let resp = client.post(&order_url).send().await;
    assert!(resp.is_err(), "Second POST should be rate limited");

    // Second DELETE should also fail
    let resp = client.delete(&order_url).send().await;
    assert!(resp.is_err(), "Second DELETE should be rate limited");
}

#[tokio::test]
async fn test_unmatched_routes_not_limited() {
    let server = setup_mock_server().await;

    // Only limit /test path
    let middleware = RateLimitMiddleware::builder()
        .route(|r| {
            r.path("/test")
                .limit(1, Duration::from_secs(10))
                .on_limit(ThrottleBehavior::Error)
        })
        .build();

    let client = create_client(middleware);

    // Root path should not be limited
    for _ in 0..10 {
        let resp = client.get(format!("{}/", server.uri())).send().await;
        assert!(resp.is_ok(), "Unmatched route should not be limited");
    }
}

// =============================================================================
// Multiple Limits Tests
// =============================================================================

#[tokio::test]
async fn test_multiple_limits_all_must_pass() {
    let server = setup_mock_server().await;

    // Create route with two limits:
    // - Burst: 3 requests per 100ms
    // - Sustained: 5 requests per 1 second
    let middleware = RateLimitMiddleware::builder()
        .route(|r| {
            r.limit(3, Duration::from_millis(100)) // Burst limit
                .limit(5, Duration::from_secs(1)) // Sustained limit
                .on_limit(ThrottleBehavior::Error)
        })
        .build();

    let client = create_client(middleware);

    let url = format!("{}/test", server.uri());

    // First 3 should succeed (within burst)
    for i in 0..3 {
        let resp = client.get(&url).send().await;
        assert!(resp.is_ok(), "Request {i} should succeed within burst");
    }

    // 4th should fail (burst exhausted)
    let resp = client.get(&url).send().await;
    assert!(resp.is_err(), "4th request should fail - burst exhausted");
}

// =============================================================================
// Concurrent Request Tests
// =============================================================================

#[tokio::test]
async fn test_concurrent_requests_respect_limit() {
    let server = setup_mock_server().await;

    let middleware = RateLimitMiddleware::builder()
        .route(|r| {
            r.limit(5, Duration::from_millis(500))
                .on_limit(ThrottleBehavior::Error)
        })
        .build();

    let client = Arc::new(create_client(middleware));

    let url = format!("{}/test", server.uri());

    // Launch 10 concurrent requests
    let mut handles = vec![];
    for _ in 0..10 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move { client.get(&url).send().await }));
    }

    // Wait for all to complete
    let mut success_count = 0;
    let mut error_count = 0;
    for handle in handles {
        match handle.await.unwrap() {
            Ok(_) => success_count += 1,
            Err(_) => error_count += 1,
        }
    }

    // Should have exactly 5 successes and 5 failures
    assert_eq!(success_count, 5, "Should have 5 successful requests");
    assert_eq!(error_count, 5, "Should have 5 rate-limited requests");
}

#[tokio::test]
async fn test_shared_state_across_clones() {
    let server = setup_mock_server().await;

    let middleware = RateLimitMiddleware::builder()
        .route(|r| {
            r.limit(3, Duration::from_secs(10))
                .on_limit(ThrottleBehavior::Error)
        })
        .build();

    // Create two clients sharing the same middleware
    let client1 = create_client(middleware.clone());
    let client2 = create_client(middleware);

    let url = format!("{}/test", server.uri());

    // Use client1 twice
    client1.get(&url).send().await.unwrap();
    client1.get(&url).send().await.unwrap();

    // Use client2 once - should still work
    client2.get(&url).send().await.unwrap();

    // Now both clients should be rate limited (shared state)
    assert!(
        client1.get(&url).send().await.is_err(),
        "client1 should be rate limited"
    );
    assert!(
        client2.get(&url).send().await.is_err(),
        "client2 should be rate limited"
    );
}

// =============================================================================
// Recovery Tests
// =============================================================================

#[tokio::test]
async fn test_rate_limit_recovers_after_window() {
    let server = setup_mock_server().await;

    let middleware = RateLimitMiddleware::builder()
        .route(|r| {
            r.limit(2, Duration::from_millis(100))
                .on_limit(ThrottleBehavior::Error)
        })
        .build();

    let client = create_client(middleware);

    let url = format!("{}/test", server.uri());

    // Exhaust the limit
    client.get(&url).send().await.unwrap();
    client.get(&url).send().await.unwrap();
    assert!(
        client.get(&url).send().await.is_err(),
        "Should be rate limited"
    );

    // Wait for recovery (one emission interval = 50ms)
    tokio::time::sleep(Duration::from_millis(60)).await;

    // Should be able to make one more request
    let resp = client.get(&url).send().await;
    assert!(resp.is_ok(), "Should recover after waiting");
}

// =============================================================================
// Edge Cases
// =============================================================================

#[tokio::test]
async fn test_very_high_burst_limit() {
    let server = setup_mock_server().await;

    let middleware = RateLimitMiddleware::builder()
        .route(|r| {
            r.limit(1000, Duration::from_secs(10))
                .on_limit(ThrottleBehavior::Error)
        })
        .build();

    let client = create_client(middleware);

    let url = format!("{}/test", server.uri());

    // Should handle many requests within burst
    for i in 0..100 {
        let resp = client.get(&url).send().await;
        assert!(resp.is_ok(), "Request {i} should succeed within high burst");
    }
}

#[tokio::test]
async fn test_catch_all_route() {
    let server = setup_mock_server().await;

    // Empty path prefix = catch all
    let middleware = RateLimitMiddleware::builder()
        .route(|r| {
            r.limit(2, Duration::from_secs(10))
                .on_limit(ThrottleBehavior::Error)
        })
        .build();

    let client = create_client(middleware);

    // Different paths share the same limit
    client
        .get(format!("{}/test", server.uri()))
        .send()
        .await
        .unwrap();
    client
        .get(format!("{}/data", server.uri()))
        .send()
        .await
        .unwrap();

    // Third request to any path should fail
    let resp = client.get(format!("{}/", server.uri())).send().await;
    assert!(resp.is_err(), "Catch-all should apply to all paths");
}

// =============================================================================
// Host Matching with Ports
// =============================================================================

#[tokio::test]
async fn test_host_matching_ignores_port() {
    let server = setup_mock_server().await;

    // wiremock URIs are like http://127.0.0.1:<port>
    // The host is always 127.0.0.1 — the middleware should match
    // even though the URL includes a port number.
    let host = "127.0.0.1";

    // Configure rate limit using only the host (no port)
    let middleware = RateLimitMiddleware::builder()
        .host(host, |h| {
            h.route(|r| {
                r.limit(1, Duration::from_secs(10))
                    .on_limit(ThrottleBehavior::Error)
            })
        })
        .build();

    let client = create_client(middleware);

    // First request should match the host (port excluded) and succeed
    let resp = client.get(format!("{}/test", server.uri())).send().await;
    assert!(resp.is_ok(), "Host matching should ignore port");

    // Second should be rate limited (proves the host matched)
    let resp = client.get(format!("{}/test", server.uri())).send().await;
    assert!(
        resp.is_err(),
        "Should be rate limited, proving host matched despite port"
    );
}

// =============================================================================
// Default Middleware (No Routes)
// =============================================================================

#[tokio::test]
async fn test_default_middleware_allows_all_requests() {
    let server = setup_mock_server().await;

    // Default middleware has no routes - all requests should pass through
    let middleware = RateLimitMiddleware::default();

    let client = create_client(middleware);

    let url = format!("{}/test", server.uri());

    // Many requests should all succeed
    for i in 0..20 {
        let resp = client.get(&url).send().await;
        assert!(
            resp.is_ok(),
            "Request {i} should succeed with no routes configured"
        );
    }
}

// =============================================================================
// Query String Handling
// =============================================================================

#[tokio::test]
async fn test_path_matching_ignores_query_strings() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let middleware = RateLimitMiddleware::builder()
        .route(|r| {
            r.path("/order")
                .limit(1, Duration::from_secs(10))
                .on_limit(ThrottleBehavior::Error)
        })
        .build();

    let client = create_client(middleware);

    // Request with query string should match the path prefix
    let resp = client
        .get(format!("{}/order?id=123&status=open", server.uri()))
        .send()
        .await;
    assert!(resp.is_ok(), "First request with query string should match");

    // Second request (different query string, same path) should be rate limited
    let resp = client
        .get(format!("{}/order?id=456", server.uri()))
        .send()
        .await;
    assert!(
        resp.is_err(),
        "Second request should be rate limited (same path, different query)"
    );
}

// =============================================================================
// HTTP Method Matching
// =============================================================================

#[tokio::test]
async fn test_method_matching_put_patch_head_options() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let middleware = RateLimitMiddleware::builder()
        .route(|r| {
            r.method(Method::PUT)
                .path("/resource")
                .limit(1, Duration::from_secs(10))
                .on_limit(ThrottleBehavior::Error)
        })
        .route(|r| {
            r.method(Method::PATCH)
                .path("/resource")
                .limit(1, Duration::from_secs(10))
                .on_limit(ThrottleBehavior::Error)
        })
        .build();

    let client = create_client(middleware);

    let url = format!("{}/resource", server.uri());

    // PUT and PATCH have separate limits
    client.put(&url).send().await.unwrap();
    client.patch(&url).send().await.unwrap();

    // Second PUT should fail
    assert!(
        client.put(&url).send().await.is_err(),
        "Second PUT should be rate limited"
    );

    // Second PATCH should also fail
    assert!(
        client.patch(&url).send().await.is_err(),
        "Second PATCH should be rate limited"
    );

    // HEAD and OPTIONS should NOT be limited (no matching route)
    assert!(
        client.head(&url).send().await.is_ok(),
        "HEAD should not be rate limited (no matching route)"
    );
    assert!(
        client.head(&url).send().await.is_ok(),
        "HEAD should still not be rate limited"
    );
}

// =============================================================================
// Jitter Tests
// =============================================================================

#[tokio::test]
async fn test_delay_includes_jitter() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    // 1 burst request per 200ms window → emission interval = 200ms
    let middleware = RateLimitMiddleware::builder()
        .route(|r| r.limit(1, Duration::from_millis(200)))
        .build();

    let client = create_client(middleware);

    let url = format!("{}/test", server.uri());

    let start = Instant::now();

    // 1st request: immediate (burst). 2nd and 3rd: delayed by ~200ms + jitter each.
    for _ in 0..3 {
        client.get(&url).send().await.unwrap();
    }

    let elapsed = start.elapsed();

    // Without jitter, 2 delays × 200ms = 400ms minimum.
    // With 0-50% jitter, expected range is ~400-600ms.
    assert!(
        elapsed >= Duration::from_millis(400),
        "Should have waited at least the bare emission intervals: {elapsed:?}"
    );
    // Upper bound: 2 delays × (200ms + 100ms max jitter) = 600ms, plus some slack
    assert!(
        elapsed < Duration::from_millis(900),
        "Delay with jitter should not exceed reasonable upper bound: {elapsed:?}"
    );
}
