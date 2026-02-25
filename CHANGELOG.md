# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-02-25

### Added

- Initial release
- Route-based rate limiting for reqwest-middleware
- GCRA (Generic Cell Rate Algorithm) implementation with lock-free atomic operations
- Host, method, and path prefix matching for routes
- Support for multiple rate limits per route (burst + sustained)
- `ThrottleBehavior::Delay` (default) - delays requests until allowed
- `ThrottleBehavior::Error` - returns error immediately when rate limited
- Host-scoped builder API for clean multi-route configuration
- Shared state across client clones
- Optional `tracing` feature for diagnostic logging

[Unreleased]: https://github.com/haut/route-ratelimit/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/haut/route-ratelimit/releases/tag/v0.1.0
