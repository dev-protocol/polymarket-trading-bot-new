# route-ratelimit

[![Crates.io](https://img.shields.io/crates/v/route-ratelimit.svg)](https://crates.io/crates/route-ratelimit)
[![Documentation](https://docs.rs/route-ratelimit/badge.svg)](https://docs.rs/route-ratelimit)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![CI](https://github.com/haut/route-ratelimit/actions/workflows/ci.yml/badge.svg)](https://github.com/haut/route-ratelimit/actions/workflows/ci.yml)
[![MSRV](https://img.shields.io/badge/MSRV-1.88.0-blue.svg)](https://blog.rust-lang.org/2025/06/26/Rust-1.88.0.html)

Route-based rate limiting middleware for [reqwest](https://github.com/seanmonstar/reqwest).

## Features

- **Endpoint matching**: Match requests by host, HTTP method, and path prefix
- **Multiple rate limits**: Stack burst and sustained limits on the same endpoint
- **Configurable behavior**: Choose to delay requests or return errors per endpoint
- **Lock-free performance**: Uses GCRA algorithm with atomic operations
- **Shared state**: Rate limits are tracked across all client clones

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
route-ratelimit = "0.1"
reqwest = "0.12"
reqwest-middleware = "0.4"
```

## Quick Start

```rust
use route_ratelimit::RateLimitMiddleware;
use reqwest_middleware::ClientBuilder;
use std::time::Duration;

#[tokio::main]
async fn main() {
    let middleware = RateLimitMiddleware::builder()
        .route(|r| r.limit(100, Duration::from_secs(10)))
        .build();

    let client = ClientBuilder::new(reqwest::Client::new())
        .with(middleware)
        .build();

    // Requests are automatically rate-limited
    client.get("https://api.example.com/data").send().await.unwrap();
}
```

## Usage

### Host-Scoped Routes

Organize rate limits by host for cleaner configuration:

```rust
use route_ratelimit::RateLimitMiddleware;
use std::time::Duration;
use http::Method;

let middleware = RateLimitMiddleware::builder()
    .host("api.example.com", |host| {
        host
            // General limit for all endpoints on this host
            .route(|r| r.limit(9000, Duration::from_secs(10)))
            // Specific limit for /book endpoints (both limits apply)
            .route(|r| r.path("/book").limit(1500, Duration::from_secs(10)))
            // Method + path specific limits
            .route(|r| {
                r.method(Method::POST)
                    .path("/order")
                    .limit(3500, Duration::from_secs(10))   // Burst
                    .limit(36000, Duration::from_secs(600)) // Sustained
            })
    })
    .build();
```

### Multiple Limits (Burst + Sustained)

Apply both burst and sustained limits to the same endpoint:

```rust
use route_ratelimit::RateLimitMiddleware;
use std::time::Duration;

let middleware = RateLimitMiddleware::builder()
    .route(|r| {
        r.path("/api")
            .limit(100, Duration::from_secs(10))   // Burst: 100 req/10s
            .limit(1000, Duration::from_secs(600)) // Sustained: 1000 req/10min
    })
    .build();
```

### Error Behavior

By default, requests are delayed until they can proceed. Use `ThrottleBehavior::Error` to fail fast:

```rust
use route_ratelimit::{RateLimitMiddleware, ThrottleBehavior};
use std::time::Duration;

let middleware = RateLimitMiddleware::builder()
    .route(|r| {
        r.limit(10, Duration::from_secs(1))
            .on_limit(ThrottleBehavior::Error) // Return error immediately
    })
    .build();
```

## Route Matching

### All Matching Routes Apply

Routes are checked in order, and **all matching routes' limits are applied**. This allows layering general limits with specific ones:

```rust
use route_ratelimit::RateLimitMiddleware;
use std::time::Duration;

let middleware = RateLimitMiddleware::builder()
    .host("api.example.com", |host| {
        host
            // This applies to ALL requests to api.example.com
            .route(|r| r.limit(9000, Duration::from_secs(10)))
            // This ALSO applies to /book requests (both limits enforced)
            .route(|r| r.path("/book").limit(1500, Duration::from_secs(10)))
    })
    .build();
```

### Host Matching

Host matching uses only the hostname, **excluding the port**:

```rust
// Matches: https://api.example.com/path
// Matches: https://api.example.com:8443/path
// Does NOT match: https://other.example.com/path
.host("api.example.com", |h| h.route(|r| r.limit(100, Duration::from_secs(10))))
```

### Path Matching

Path matching uses **segment boundaries**, not simple prefix matching:

| Pattern  | Matches                        | Does NOT Match       |
|----------|--------------------------------|----------------------|
| `/order` | `/order`, `/order/`, `/order/123` | `/orders`, `/order-test` |
| `/api/v1` | `/api/v1/users`, `/api/v1/`   | `/api/v2`, `/api/v10` |

## Performance

The hot path is optimized for minimal overhead per request:

```
Request
  │
  ▼
Extract host, method, path (once)
  │
  ▼
┌─────────────────────────────┐
│  For each matching route:   │◄── all matching routes apply
│  check stacked limits via   │
│  GCRA (lock-free atomic)    │
└─────────┬───────────────────┘
          │
    ┌─────┴─────┐
    │  Allowed?  │
    ┌───┘       └───┐
    ▼               ▼
  Pass        Delay w/ jitter
              or return error
```

- **Lock-free**: GCRA algorithm uses `AtomicU64` compare-exchange — no mutexes or shard locks
- **Zero allocation on hot path**: all state pre-allocated at build time in a flat array, indexed by precomputed offsets
- **Single URL parse**: host, method, and path extracted once before iterating routes
- **Cold-path optimization**: rate-limited branch marked `#[cold]` to keep the happy path compact in instruction cache

## Optional Features

### Tracing Support

Enable the `tracing` feature for diagnostic logging:

```toml
[dependencies]
route-ratelimit = { version = "0.1", features = ["tracing"] }
```

This enables warnings for potentially problematic configurations (e.g., catch-all routes preceding specific routes).

## Examples

See the [examples](examples/) directory for complete usage examples:

- [Polymarket API](examples/polymarket.rs) - Complete rate limit configuration for a real-world API

## Minimum Supported Rust Version

This crate requires Rust 1.88.0 or later.

## License

Licensed under the MIT License. See [LICENSE](LICENSE) for details.
