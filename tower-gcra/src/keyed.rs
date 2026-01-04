// Copyright (C) 2025 Michael Herstine <sp1ff@pobox.com>
//
// This file is part of tower-gcra.
//
// tower-gcra is free software: you can redistribute it and/or modify it under the terms of the GNU
// General Public License as published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// tower-gcra is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without
// even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU
// General Public License for more details.
//
// You should have received a copy of the GNU General Public License along with tower-gcra. If not,
// see <http://www.gnu.org/licenses/>.

//! # Keyed Rate Limiting
//!
//! ## Introduction
//!
//! [tower-gcra](crate) supports both "direct" and "keyed" [tower] [Layer]s. This module implements
//! the latter. A keyed rate-limiter maintains rate-limiting state per key, where by "key" we mean
//! some request attribute (the value of the Host header for HTTP requests, for instance). The
//! [tower] [Service] trait is actually implemented on type [Governor].
//! The reader may also be interested in the discussion [here].
//!
//! [tower]: https://docs.rs/tower/latest/tower/index.html
//! [Layer]: https://docs.rs/tower/latest/tower/trait.Layer.html
//! [here]: https://docs.rs/governor/latest/governor/_guide/index.html

use std::{
    error::Error as StdError,
    hash::Hash,
    marker::PhantomData,
    pin::Pin,
    result::Result as StdResult,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use either::Either;
use governor::{
    NotUntil, Quota, RateLimiter,
    clock::{Clock, DefaultClock},
    middleware::{NoOpMiddleware, RateLimitingMiddleware},
    state::keyed::{DefaultKeyedStateStore, KeyedStateStore},
};
use pin_project_lite::pin_project;
use tokio::time::{Sleep, sleep};
use tower::Service;

/// Types implementing `KeyExtractor<Request>` extract rate-limiting keys from `Request`s.
/// A "key" is any type that implements [Hash].
pub trait KeyExtractor<Request> {
    type Key: Hash;
    type Error: StdError + Send + Sync + 'static;
    fn extract(&self, req: &Request) -> StdResult<Self::Key, Self::Error>;
}

/// A [tower] [Service] that rate-limits requests using keyed rate limiting
///
/// ## Constructing a Governor
///
/// The [Governor] type is a generic parameterized by five type variables:
///
/// - S: the "inner" service which will see its requests rate-limited by the [Governor]
/// - KE: the key extractor
/// - KS: an implementation of [KeyedStateStore], which implements state storage for the Governor
/// - C: an implementation of the [Clock] trait, which defines the time source
/// - MW: an implementation of [RateLimitingMiddleware]
///
/// The caller will presumably already have a [Service] for which they're interested in
/// rate-limiting incoming requests. The caller is also expected to have a [KeyExtractor]
/// implementation suitable to their use case. The final three are defined by the [governor] crate
/// which also supplies various implementations.
///
/// The simplest way to construct a [Governor] instance is the [default()](Governor::default)
/// method, which takes the inner [Service] and the [Quota] to be enforced, selecting default
/// implementations for all other types.
///
/// The most flexible way to construct a [Governor] instance is the [new()][Governor::new] method,
/// which will accept arguments of all four types.
///
/// Finally, if you've already constructed a [RateLimiter], you can create a [Governor] instance via
/// [new_with_limiter()](Governor::new_with_limiter).
///
/// All this said, one is more likely to construct an instance indirectly through [Layer].
///
/// ## On Governor Being `Clone`
///
/// [Governor] instances can be cloned; they share a reference (via an [Arc]) to a single rate
/// limiter, so cloning won't result in rate limits being evaded.
#[derive(Clone)]
pub struct Governor<S, KE, Request, KS, C, MW>
where
    KE: KeyExtractor<Request> + Clone,
    KS: KeyedStateStore<<KE as KeyExtractor<Request>>::Key>,
    C: Clock,
    MW: RateLimitingMiddleware<<KE as KeyExtractor<Request>>::Key, C::Instant>,
{
    inner: S,
    key_extractor: KE,
    phantom: PhantomData<Request>,
    limiter: Arc<RateLimiter<<KE as KeyExtractor<Request>>::Key, KS, C, MW>>,
}

impl<S, KE, Request>
    Governor<
        S,
        KE,
        Request,
        DefaultKeyedStateStore<<KE as KeyExtractor<Request>>::Key>,
        DefaultClock,
        NoOpMiddleware,
    >
where
    KE: KeyExtractor<Request> + Clone,
    <KE as KeyExtractor<Request>>::Key: Clone + Eq,
{
    pub fn default(inner: S, key_extractor: KE, quota: Quota) -> Self {
        Self {
            inner,
            key_extractor,
            phantom: PhantomData,
            limiter: Arc::new(RateLimiter::keyed(quota)),
        }
    }
}

impl<S, KE, Request, KS, C, MW> Governor<S, KE, Request, KS, C, MW>
where
    KE: KeyExtractor<Request> + Clone,
    KS: KeyedStateStore<<KE as KeyExtractor<Request>>::Key>,
    C: Clock,
    MW: RateLimitingMiddleware<<KE as KeyExtractor<Request>>::Key, C::Instant>,
{
    pub fn new(
        inner: S,
        key_extractor: KE,
        quota: Quota,
        state: KS,
        clock: C,
        middleware: MW,
    ) -> Self {
        Self {
            inner,
            key_extractor,
            phantom: PhantomData,
            limiter: Arc::new(RateLimiter::new(quota, state, clock, middleware)),
        }
    }
    pub fn new_with_limiter(
        inner: S,
        key_extractor: KE,
        limiter: Arc<RateLimiter<<KE as KeyExtractor<Request>>::Key, KS, C, MW>>,
    ) -> Self {
        Self {
            inner,
            key_extractor,
            phantom: PhantomData,
            limiter,
        }
    }
    pub fn inner(&self) -> &S {
        &self.inner
    }
}

pin_project! {
    #[project = FutureStateProj]
    enum FutureState<S, KE, Request, KS, C, MW, F>
    where S: Service<Request>,
          KE: KeyExtractor<Request>,
          KS: KeyedStateStore<<KE as KeyExtractor<Request>>::Key>,
          C: Clock,
          MW: RateLimitingMiddleware<<KE as KeyExtractor<Request>>::Key, C::Instant>
    {
        Error {
            source: Option<<KE as KeyExtractor<Request>>::Error>,
        },
        Ready {
            #[pin]
            fut: Sleep,
            // Nothing else needs to be pinned:
            limiter: Arc<RateLimiter<<KE as KeyExtractor<Request>>::Key, KS, C, MW>>,
            key: Option<<KE as KeyExtractor<Request>>::Key>,
            inner: Option<S>,
            req: Option<Request>
        },
        Call {
            #[pin]
            fut: F
        }
    }
}

pin_project! {
    pub struct CallFuture<S, KE, Request, KS, C, MW>
    where S: Service<Request>,
          KE: KeyExtractor<Request>,
          KS: KeyedStateStore<<KE as KeyExtractor<Request>>::Key>,
          C: Clock,
          MW: RateLimitingMiddleware<<KE as KeyExtractor<Request>>::Key, C::Instant>
    {
        #[pin]
        state: FutureState<S, KE, Request, KS, C, MW, <S as Service<Request>>::Future>,
    }
}

impl<S, KE, Request, KS, C, MW> CallFuture<S, KE, Request, KS, C, MW>
where
    S: Service<Request>,
    KE: KeyExtractor<Request>,
    KS: KeyedStateStore<<KE as KeyExtractor<Request>>::Key>,
    C: Clock,
    MW: RateLimitingMiddleware<<KE as KeyExtractor<Request>>::Key, C::Instant>,
{
    pub fn new_from_extract_error(source: <KE as KeyExtractor<Request>>::Error) -> Self {
        Self {
            state: FutureState::Error {
                source: Some(source),
            },
        }
    }
    pub fn new_from_until(
        dur: Duration,
        limiter: Arc<RateLimiter<<KE as KeyExtractor<Request>>::Key, KS, C, MW>>,
        key: <KE as KeyExtractor<Request>>::Key,
        inner: S,
        req: Request,
    ) -> Self {
        Self {
            state: FutureState::Ready {
                fut: sleep(dur),
                limiter,
                key: Some(key),
                inner: Some(inner),
                req: Some(req),
            },
        }
    }
    pub fn new(mut inner: S, req: Request) -> Self {
        Self {
            state: FutureState::Call {
                fut: inner.call(req),
            },
        }
    }
}

impl<S, KE, Request, KS, C, MW> Future for CallFuture<S, KE, Request, KS, C, MW>
where
    S: Service<Request>,
    KE: KeyExtractor<Request>,
    KS: KeyedStateStore<<KE as KeyExtractor<Request>>::Key>,
    C: Clock,
    MW: RateLimitingMiddleware<
            <KE as KeyExtractor<Request>>::Key,
            C::Instant,
            NegativeOutcome = NotUntil<C::Instant>,
        >,
{
    type Output = StdResult<
        <S as Service<Request>>::Response,
        Either<<KE as KeyExtractor<Request>>::Error, <S as Service<Request>>::Error>,
    >;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();

        loop {
            match this.state.as_mut().project() {
                FutureStateProj::Error { source } => {
                    return Poll::Ready(Err(Either::Left(std::mem::take(source).unwrap())));
                }
                FutureStateProj::Ready {
                    fut,
                    limiter,
                    key,
                    inner,
                    req,
                } => match fut.poll(cx) {
                    Poll::Ready(_) => {
                        // let fut = this.inner.take().unwrap().call(this.req.take().unwrap());
                        // this.state.set(FutureState::Call { fut })
                        let limiter = limiter.clone();
                        let key = key.take();
                        let inner = inner.take();
                        let req = req.take();
                        match limiter.check_key(key.as_ref().unwrap()) {
                            Ok(_) => this.state.set(FutureState::Call {
                                fut: inner.unwrap().call(req.unwrap()),
                            }),
                            Err(not_until) => this.state.set(FutureState::Ready {
                                fut: sleep(not_until.wait_time_from(limiter.clock().now())),
                                limiter,
                                key,
                                inner,
                                req,
                            }),
                        }
                    }
                    Poll::Pending => return Poll::Pending,
                },
                FutureStateProj::Call { fut } => return fut.poll(cx).map_err(Either::Right),
            }
        }
    }
}

impl<S, KE, Request, KS, C, MW> Service<Request> for Governor<S, KE, Request, KS, C, MW>
where
    S: Service<Request> + Clone,
    KE: KeyExtractor<Request> + Clone,
    KS: KeyedStateStore<<KE as KeyExtractor<Request>>::Key>,
    C: Clock,
    MW: RateLimitingMiddleware<
            <KE as KeyExtractor<Request>>::Key,
            C::Instant,
            NegativeOutcome = NotUntil<C::Instant>,
        >,
{
    type Response = S::Response;

    type Error = Either<<KE as KeyExtractor<Request>>::Error, S::Error>;

    type Future = CallFuture<S, KE, Request, KS, C, MW>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Either::Right)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let key = match self.key_extractor.extract(&req) {
            Ok(key) => key,
            Err(err) => {
                return CallFuture::new_from_extract_error(err);
            }
        };

        match self.limiter.check_key(&key) {
            Ok(_) => CallFuture::new(self.inner.clone(), req),
            Err(not_until) => CallFuture::new_from_until(
                not_until.wait_time_from(self.limiter.clock().now()),
                self.limiter.clone(),
                key,
                self.inner.clone(),
                req,
            ),
        }
    }
}

/// A [tower] [Layer](tower::Layer) providing keyed rate-limiting to an inner [Service]
///
/// ## Constructing a Layer
///
/// The type parameters are the same as for [Governor] (where they are documented in detail). The
/// simplest way to construct a [Layer] instance is with [default()](Layer::default) which will
/// select default implementations of all parameters. The most flexibile is [new()](Layer::new)
/// which allows maximum flexibility in selecting implementations. Finally, the caller can construct
/// a [RateLimiter] separately and use [new_with_limiter](Layer::new_with_limiter).
pub struct Layer<Request, KE, KS, C, MW>
where
    KE: KeyExtractor<Request> + Clone,
    KS: KeyedStateStore<<KE as KeyExtractor<Request>>::Key>,
    C: Clock,
    MW: RateLimitingMiddleware<<KE as KeyExtractor<Request>>::Key, C::Instant>,
{
    key_extractor: KE,
    limiter: Arc<RateLimiter<<KE as KeyExtractor<Request>>::Key, KS, C, MW>>,
    phantom: PhantomData<Request>,
}

impl<Request, KE>
    Layer<
        Request,
        KE,
        DefaultKeyedStateStore<<KE as KeyExtractor<Request>>::Key>,
        DefaultClock,
        NoOpMiddleware,
    >
where
    KE: KeyExtractor<Request> + Clone,
    <KE as KeyExtractor<Request>>::Key: Clone + Eq,
{
    pub fn default(key_extractor: KE, quota: Quota) -> Self {
        Self {
            key_extractor,
            limiter: Arc::new(RateLimiter::keyed(quota)),
            phantom: PhantomData,
        }
    }
}

impl<Request, KE, KS, C, MW> Layer<Request, KE, KS, C, MW>
where
    KE: KeyExtractor<Request> + Clone,
    KS: KeyedStateStore<<KE as KeyExtractor<Request>>::Key>,
    C: Clock,
    MW: RateLimitingMiddleware<<KE as KeyExtractor<Request>>::Key, C::Instant>,
{
    pub fn new(key_extractor: KE, quota: Quota, state: KS, clock: C, middleware: MW) -> Self {
        Self {
            key_extractor,
            limiter: Arc::new(RateLimiter::new(quota, state, clock, middleware)),
            phantom: PhantomData,
        }
    }
    pub fn new_with_limiter(
        key_extractor: KE,
        limiter: RateLimiter<<KE as KeyExtractor<Request>>::Key, KS, C, MW>,
    ) -> Self {
        Self {
            key_extractor,
            limiter: Arc::new(limiter),
            phantom: PhantomData,
        }
    }
}

impl<S, KE, Request, KS, C, MW> tower::Layer<S> for Layer<Request, KE, KS, C, MW>
where
    KE: KeyExtractor<Request> + Clone,
    KS: KeyedStateStore<<KE as KeyExtractor<Request>>::Key>,
    C: Clock,
    MW: RateLimitingMiddleware<<KE as KeyExtractor<Request>>::Key, C::Instant>,
{
    type Service = Governor<S, KE, Request, KS, C, MW>;

    fn layer(&self, inner: S) -> Self::Service {
        Governor::new_with_limiter(inner, self.key_extractor.clone(), self.limiter.clone())
    }
}

#[cfg(test)]
mod test {
    use std::{collections::HashMap, convert::Infallible, num::NonZeroU32, sync::RwLock};

    use dashmap::DashMap;
    use governor::{Quota, clock::QuantaClock, gcra::Gcra};
    use tower::ServiceExt;

    use crate::fixtures::RecordingService;

    use super::*;

    // Now, let's keep my `RecordingService`, but try keyed rate-limiting. The "key" will just be
    // the `usize` making-up the request.
    #[derive(Clone)]
    struct RecordingRequestKeyExtractor;

    impl KeyExtractor<usize> for RecordingRequestKeyExtractor {
        type Key = usize;
        type Error = Infallible;
        fn extract(&self, req: &usize) -> StdResult<Self::Key, Self::Error> {
            Ok(*req)
        }
    }

    // Next, I need some middleware. Let's try a very simple implementation:
    #[derive(Debug)]
    struct KeyedHashmapMiddleware {
        // The MW doesn't have to be Clone
        keys: RwLock<HashMap<usize, Gcra>>,
    }

    impl KeyedHashmapMiddleware {
        pub fn new() -> Self {
            Self {
                keys: RwLock::new(HashMap::from([(
                    1,
                    Gcra::new(Quota::per_second(NonZeroU32::new(2).unwrap())),
                )])),
            }
        }
    }

    impl RateLimitingMiddleware<usize, <QuantaClock as Clock>::Instant> for KeyedHashmapMiddleware {
        type PositiveOutcome = ();

        type NegativeOutcome = NotUntil<<QuantaClock as Clock>::Instant>;

        fn allow(_: &usize, _: impl Into<governor::gcra::StateSnapshot>) -> Self::PositiveOutcome {}

        fn disallow(
            _: &usize,
            state: impl Into<governor::gcra::StateSnapshot>,
            start_time: <QuantaClock as Clock>::Instant,
        ) -> Self::NegativeOutcome {
            NotUntil::new(state.into(), start_time)
        }

        fn check_quota(
            &self,
            key: &usize,
            f: &dyn Fn(&Gcra) -> Result<Self::PositiveOutcome, Self::NegativeOutcome>,
        ) -> Option<Result<Self::PositiveOutcome, Self::NegativeOutcome>> {
            self.keys.read().unwrap().get(key).map(|gcra| f(gcra))
        }
    }

    #[tokio::test]
    async fn keyed_rate_limiting_smoke_test() {
        let inner = RecordingService::new();
        let key_extractor = RecordingRequestKeyExtractor;
        let limiter = governor::RateLimiter::keyed(Quota::per_second(NonZeroU32::new(1).unwrap()))
            .use_middleware(KeyedHashmapMiddleware::new());
        let mut governor = Governor::new_with_limiter(inner, key_extractor, Arc::new(limiter));

        governor.ready().await.unwrap().call(0).await.unwrap();
        governor.ready().await.unwrap().call(0).await.unwrap(); // 0: Should be rate limited
        governor.ready().await.unwrap().call(1).await.unwrap(); // 1: Should go through
        governor.ready().await.unwrap().call(1).await.unwrap(); // 2: Should go through
        governor.ready().await.unwrap().call(0).await.unwrap(); // 3: Should be rate limited
        governor.ready().await.unwrap().call(2).await.unwrap(); // 4: Should go through-- new key

        let intervals = governor.inner().intervals();
        assert!(intervals[0].as_millis() >= 999);
        assert!(intervals[1].as_millis() < 1);
        assert!(intervals[2].as_millis() < 1);
        assert!(intervals[3].as_millis() >= 999);
        assert!(intervals[4].as_millis() < 1);
    }

    #[derive(Debug)]
    struct KeyedDashmapMiddleware {
        keys: DashMap<usize, Gcra>,
    }

    impl KeyedDashmapMiddleware {
        pub fn new() -> Self {
            Self {
                keys: DashMap::from_iter(
                    [(1, Gcra::new(Quota::per_second(NonZeroU32::new(2).unwrap())))].into_iter(),
                ),
            }
        }
    }

    impl RateLimitingMiddleware<usize, <QuantaClock as Clock>::Instant> for KeyedDashmapMiddleware {
        type PositiveOutcome = ();

        type NegativeOutcome = NotUntil<<QuantaClock as Clock>::Instant>;

        fn allow(_: &usize, _: impl Into<governor::gcra::StateSnapshot>) -> Self::PositiveOutcome {}

        fn disallow(
            _: &usize,
            state: impl Into<governor::gcra::StateSnapshot>,
            start_time: <QuantaClock as Clock>::Instant,
        ) -> Self::NegativeOutcome {
            NotUntil::new(state.into(), start_time)
        }

        fn check_quota(
            &self,
            key: &usize,
            f: &dyn Fn(&Gcra) -> Result<Self::PositiveOutcome, Self::NegativeOutcome>,
        ) -> Option<Result<Self::PositiveOutcome, Self::NegativeOutcome>> {
            self.keys.get(key).map(|gcra| f(gcra.value()))
        }
    }

    #[tokio::test]
    async fn keyed_rate_limiting_with_dashmap_smoke_test() {
        let inner = RecordingService::new();
        let key_extractor = RecordingRequestKeyExtractor;
        let limiter = governor::RateLimiter::keyed(Quota::per_second(NonZeroU32::new(1).unwrap()))
            .use_middleware(KeyedDashmapMiddleware::new());
        let mut governor = Governor::new_with_limiter(inner, key_extractor, Arc::new(limiter));

        governor.ready().await.unwrap().call(0).await.unwrap();
        governor.ready().await.unwrap().call(0).await.unwrap(); // 0: Should be rate limited
        governor.ready().await.unwrap().call(1).await.unwrap(); // 1: Should go through
        governor.ready().await.unwrap().call(1).await.unwrap(); // 2: Should go through
        governor.ready().await.unwrap().call(0).await.unwrap(); // 3: Should be rate limited
        governor.ready().await.unwrap().call(2).await.unwrap(); // 4: Should go through-- new key

        let intervals = governor.inner().intervals();
        assert!(intervals[0].as_millis() >= 999);
        assert!(intervals[1].as_millis() < 1);
        assert!(intervals[2].as_millis() < 1);
        assert!(intervals[3].as_millis() >= 999);
        assert!(intervals[4].as_millis() < 1);
    }
}
