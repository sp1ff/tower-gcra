// Copyright (C) 2025 Michael Herstine <sp1ff@pobox.com>
//
// This file is part of tower-client-governor.
//
// tower-client-governor is free software: you can redistribute it and/or modify it under the terms
// of the GNU General Public License as published by the Free Software Foundation, either version 3
// of the License, or (at your option) any later version.
//
// tower-client-governor is distributed in the hope that it will be useful, but WITHOUT ANY
// WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR
// PURPOSE. See the GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License along with
// tower-client-governor. If not, see <http://www.gnu.org/licenses/>.

//! # tower-gcra
//!
//! ## Introduction
//!
//! [tower-gcra] is a [tower] middleware that provides rate-limiting; that is, it limits the rate at
//! which requests are processed by the [Service] which it wraps. It does so using the [Generic Cell
//! Rate Algorithm] (AKA GCRA). [GCRA] is a particularly effective variant of the [Leaky Bucket]
//! approach to rate-limiting.
//!
//! [tower-gcra]: https://github.com/sp1ff/tower-gcra
//! [tower]: https://docs.rs/tower/latest/tower/index.html
//! [Service]: https://docs.rs/tower/latest/tower/trait.Service.html
//! [Generic Cell Rate Algorithm]: https://brandur.org/rate-limiting
//! [GCRA]: https://en.wikipedia.org/wiki/Generic_cell_rate_algorithm
//! [Leaky Bucket]: https://grokipedia.com/page/Leaky_bucket
//!
//! In particular, [tower-gcra] provides for both "direct" and "keyed" rate-limiting. The former
//! defines a single quota governing the rate at which all requests will be processed. The latter
//! defines "per-key" state, where a key is some aspect of the requests being serviced (the peer IP
//! address, for instance), so that the quota applies per instance of that key (when the key is peer
//! IP address, this would mean that each peer IP is rate-limited individually). Furthermore,
//! [tower-gcra] allows per-key _quotas_ as well (so, for instance, we could build a rate-limiting
//! client that applies different rate limits depending on the destination).
//!
//! [tower-gcra] depends on the [governor] crate for the core GCRA implementation. At the time of
//! this writing, the per-key quota functionality depends on an as-yet-unmerged governor [PR].
//!
//! [governor]: https://github.com/antifuchs/governor
//! [PR]: https://github.com/boinkor-net/governor/pull/292
//!
//! ## Examples
//!
//! ### Direct Rate-Limiting
//!
//! ```rust
//! use std::{convert::Infallible, result::Result, time::Instant};
//!
//! use governor::{Quota, RateLimiter, clock::{QuantaClock, QuantaInstant}, middleware::NoOpMiddleware, state::InMemoryState};
//! use nonzero::nonzero;
//! use tower::{BoxError, Service, ServiceBuilder, ServiceExt};
//! use tower_gcra::direct::Layer as GovernorLayer;
//!
//! // This will be our Service-- it just echo's its input.
//! async fn echo(request: &'static str) -> Result<&'static str, Infallible> {
//!     Ok(request)
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), BoxError> {
//! // Give ourselves a default direct RateLimiter at 1 request/second.
//! let rate_limiter = RateLimiter::direct(Quota::per_second(nonzero!(1u32)));
//! // Build-up a service...
//! let mut svc = ServiceBuilder::new()
//!     // with a single rate-limiting layer...
//!     .layer(GovernorLayer::<&'static str, _, _, _>::new_with_limiter(rate_limiter))
//!     // wrapping our echo function.
//!     .service_fn(echo);
//!
//! let then = Instant::now();
//! // Blast three requests through as fast as we can:
//! for _ in 0..3 {
//!     let response = svc.ready().await?.call("Hello, world!").await?;
//!     assert_eq!(response, "Hello, world!");
//! }
//! let duration = Instant::now() - then;
//! // Requests 1 & 2 (of 3) were rate-limited at 1/sec.
//! assert!(duration.as_millis() > 2000);
//! # Ok(())
//! # }
//! ```
//!
//! ### Keyed Rate-Limiting
//!
//! ```rust
//! use std::{convert::Infallible, fmt::Debug, hash::Hash, result::Result, time::Instant};
//!
//! use dashmap::DashMap;
//! use governor::{InsufficientCapacity, NotUntil, Quota, RateLimiter, clock::Reference,
//!                gcra::{Gcra, StateSnapshot}, middleware::RateLimitingMiddleware};
//! use http::{HeaderName, HeaderValue, Request, Response, StatusCode, header::HOST};
//! use nonzero::nonzero;
//! use tower::{BoxError, Service, ServiceBuilder, ServiceExt};
//! use tower_gcra::keyed::{KeyExtractor, Layer as GovernorLayer};
//!
//! // Let's build a Service that works in terms of http Requests & Responses
//! async fn say_hello(_: Request<()>) -> Result<Response<String>, http::Error> {
//!     Response::builder().status(StatusCode::OK).body("Hello, world!".to_owned())
//! }
//!
//! // At this time, the governor middleware for tracking per-key quotas needs to
//! // be implemented in the application. I hope to contribute such middleware
//! // to the governor crate.
//! #[derive(Debug)]
//! struct KeyedMw<K: Eq + Hash> {
//!     keys: DashMap<K, Gcra>
//! }
//!
//! impl<K, const N: usize> From<[(K, Quota); N]> for KeyedMw<K>
//! where
//!     K: Clone + Eq + Hash,
//! {
//!     fn from(value: [(K, Quota); N]) -> Self {
//!         Self {
//!             keys: DashMap::from_iter(value.into_iter().map(|(k, q)| (k, Gcra::new(q))))
//!         }
//!     }
//! }
//!
//! impl<K, P> RateLimitingMiddleware<K, P> for KeyedMw<K>
//! where
//!     K: Debug + Eq + Hash,
//!     P: Reference,
//! {
//!     type PositiveOutcome = ();
//!
//!     type NegativeOutcome = NotUntil<P>;
//!
//!     fn allow(_key: &K, _state: impl Into<StateSnapshot>) -> Self::PositiveOutcome {
//!         {}
//!     }
//!
//!     fn disallow(
//!         _key: &K,
//!         state: impl Into<StateSnapshot>,
//!         start_time: P,
//!     ) -> Self::NegativeOutcome {
//!         NotUntil::new(state.into(), start_time)
//!     }
//!
//!     fn check_quota(
//!         &self,
//!         key: &K,
//!         f: &dyn Fn(&Gcra) -> Result<Self::PositiveOutcome, Self::NegativeOutcome>,
//!     ) -> Option<Result<Self::PositiveOutcome, Self::NegativeOutcome>> {
//!         self.keys.get(key).map(|r| f(r.value()))
//!     }
//!
//!     fn check_quota_n(
//!         &self,
//!         key: &K,
//!         f: &dyn Fn(
//!             &Gcra,
//!         ) -> Result<
//!             Result<Self::PositiveOutcome, Self::NegativeOutcome>,
//!             InsufficientCapacity,
//!         >,
//!     ) -> Option<
//!         Result<Result<Self::PositiveOutcome, Self::NegativeOutcome>, InsufficientCapacity>,
//!     > {
//!         self.keys.get(key).map(|r| f(r.value()))
//!     }
//! }
//!
//! // Trivial key extractor for pulling the Host header out of incoming reuqests
//! #[derive(Clone)]
//! struct HostExtractor;
//!
//! impl KeyExtractor<Request<()>> for HostExtractor {
//!     type Key = HeaderValue;
//!     type Error = Infallible;
//!     fn extract(&self, req: &Request<()>) -> Result<Self::Key, Self::Error> {
//!         Ok(req.headers().get(HOST).cloned().unwrap_or(HeaderValue::from_static("No host")))
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), BoxError> {
//! // Simple keyed middleware with a "catch-all" quota of 3 requests per second, but
//! // special-case quotas for "foo.com" & "bar.com":
//! let rate_limiter = RateLimiter::keyed(Quota::per_second(nonzero!(3u32)))
//!     .use_middleware(KeyedMw::<HeaderValue>::from(
//!         [(HeaderValue::from_static("foo.com"), Quota::per_second(nonzero!(2u32))),
//!          (HeaderValue::from_static("bar.com"), Quota::per_second(nonzero!(1u32)))]));
//!
//! // Build a simple service...
//! let mut svc = ServiceBuilder::new()
//!     // with a keyed rate-limiting middleware...
//!     .layer(GovernorLayer::<Request<()>, HostExtractor, _, _, _>::new_with_limiter(HostExtractor, rate_limiter))
//!     // wrapping our simple Service:
//!     .service_fn(say_hello);
//!
//! let then = Instant::now();
//! // Blast three requests through as fast as we can, each to a different host
//! let _ = svc.ready().await?.call(Request::builder()
//!                                 .uri("https://foo.com")
//!                                 .header(HOST, "foo.com")
//!                                 .body(()).unwrap()).await?;
//! let _ = svc.ready().await?.call(Request::builder()
//!                                 .uri("https://bar.com")
//!                                 .header(HOST, "bar.com")
//!                                 .body(()).unwrap()).await?;
//! let _ = svc.ready().await?.call(Request::builder()
//!                                 .uri("https://splat.com")
//!                                 .header(HOST, "splat.com")
//!                                 .body(()).unwrap()).await?;
//! let duration = Instant::now() - then;
//! // None of these requests should be rate-limited, as each host has its own quota.
//! assert!(duration.as_millis() < 1000);
//! # Ok(())
//! # }
//! ```
//!
//! ## Comparisons To Other tower Rate-Limiting Crates
//!
//! ### RateLimit
//!
//! [tower] ships with a rate-limiting middleware: [RateLimit]. [RateLimit] "enforces a rate limit
//! on the number of requests the underlying service can handle over a period of time." And indeed,
//! it counts the number of requests per given unit of time & will pend any request over & above the
//! given permissible number. In this way it differs from [Leaky Bucket] in that a client can make
//! requests at an arbitrarily high rate until it exhausts its quota for any given time period.
//!
//! [RateLimit]: https://docs.rs/tower/latest/tower/limit/rate/struct.RateLimit.html
//!
//! When rate-limited, calls to [poll_ready()] will pend (not [call()]), which seems preferrable.
//! This crate doesn't do that because without a request, we can't extract a key for
//! rate-limiting.
//!
//! [poll_ready()]: tower::Service::poll_ready
//! [call()]: tower::Service::call
//!
//! [RateLimit] is not [Clone], which can be inconvenient. For instance, [RetryLayer] requries that
//! the [Service] it wraps be [Clone]. The suggested [workaround] is to wrap it in a [BufferLayer].
//! Regrettably, that erases the inner error type to `Box<dyn Error + Send + Sync>`.
//!
//! [RetryLayer]: https://docs.rs/tower/latest/tower/retry/struct.Retry.html
//! [workaround]: https://github.com/tokio-rs/axum/discussions/987#discussioncomment-2678595
//! [BufferLayer]: https://docs.rs/tower/latest/tower/buffer/struct.BufferLayer.html
//!
//! ### tower-governor
//!
//! [tower-governor] is another [tower] middleware building on the [governor] crate, but is
//! explicitly targeted at HTTP servers. For instance, it only implements [Service] for [http]
//! [Request]s, and rather than pending rate-limited requests, it returns an HTTP 429 Too Many
//! Requests response, taking care to insert "X-RateLimit-After" and "Retry-After" headers.
//!
//! [tower-governor]: https://docs.rs/tower_governor/latest/tower_governor/index.html
//! [http]: https://docs.rs/http/latest/http/index.html
//! [Request]: https://docs.rs/http/latest/http/request/struct.Request.html
//!
//! It is, however, [Clone]. It, like this crate, handles rate-limiting in [call()] (rather than
//! [poll_ready()]), presumably also because it has access to the incoming request there. Lastly,
//! at the time of this writing, it doesn't support per-key quotas.

pub mod direct;
pub mod extractors;
#[cfg(test)]
pub mod fixtures;
pub mod keyed;
