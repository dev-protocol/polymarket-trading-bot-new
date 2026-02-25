use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use http::Method;
use route_ratelimit::{RateLimitMiddleware, ThrottleBehavior};
use std::time::Duration;

/// Benchmark the full check_and_apply_limits path with varying route counts.
fn bench_check_and_apply_limits(c: &mut Criterion) {
    let mut group = c.benchmark_group("check_and_apply_limits");

    for route_count in [1, 5, 10, 25] {
        let mut builder = RateLimitMiddleware::builder();
        for i in 0..route_count {
            let path = format!("/path{}", i);
            builder = builder.route(move |r| {
                r.host("api.example.com")
                    .path(&path)
                    .limit(100_000, Duration::from_secs(10))
            });
        }
        let middleware = builder.build();

        // Request that matches the last route (worst case: checks all routes)
        let req = reqwest::Client::new()
            .get(format!(
                "https://api.example.com/path{}",
                route_count - 1
            ))
            .build()
            .unwrap();

        group.bench_with_input(
            BenchmarkId::new("routes", route_count),
            &route_count,
            |b, _| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .unwrap();
                b.iter(|| {
                    rt.block_on(async {
                        black_box(middleware.check_and_apply_limits(&req).await)
                    })
                })
            },
        );
    }

    group.finish();
}

/// Benchmark route matching in isolation.
fn bench_route_matching(c: &mut Criterion) {
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
        .build();

    let req_hit = reqwest::Client::new()
        .get("https://clob.polymarket.com/book")
        .build()
        .unwrap();

    let req_miss = reqwest::Client::new()
        .get("https://other.example.com/test")
        .build()
        .unwrap();

    let mut group = c.benchmark_group("route_matching");

    group.bench_function("hit", |b| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        b.iter(|| {
            rt.block_on(async {
                black_box(middleware.check_and_apply_limits(&req_hit).await)
            })
        })
    });

    group.bench_function("miss", |b| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        b.iter(|| {
            rt.block_on(async {
                black_box(middleware.check_and_apply_limits(&req_miss).await)
            })
        })
    });

    group.finish();
}

/// Benchmark with multiple stacked limits per route.
fn bench_stacked_limits(c: &mut Criterion) {
    let mut group = c.benchmark_group("stacked_limits");

    for limit_count in [1, 2, 4] {
        let middleware = RateLimitMiddleware::builder()
            .route(|r| {
                let mut builder = r.host("api.example.com").path("/data");
                for _ in 0..limit_count {
                    builder = builder.limit(100_000, Duration::from_secs(10));
                }
                builder
            })
            .build();

        let req = reqwest::Client::new()
            .get("https://api.example.com/data")
            .build()
            .unwrap();

        group.bench_with_input(
            BenchmarkId::new("limits", limit_count),
            &limit_count,
            |b, _| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .unwrap();
                b.iter(|| {
                    rt.block_on(async {
                        black_box(middleware.check_and_apply_limits(&req).await)
                    })
                })
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_check_and_apply_limits,
    bench_route_matching,
    bench_stacked_limits,
);
criterion_main!(benches);
