// Copyright (C) 2025-2026 Michael Herstine <sp1ff@pobox.com>
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

//! # Direct Rate Limiting
//!
//! ## Introduction
//!
//! [tower-gcra](crate) supports both "direct" and "keyed" [tower] [Layer]s. This module implements
//! the former. A direct rate-limiting layer maintains a single state pertaining to all requests
//! that pass through it. The [tower] [Service] trait is actually implemented on type [Governor].
//! The reader may also be interested in the discussion [here].
//!
//! [tower]: https://docs.rs/tower/latest/tower/index.html
//! [Layer]: https://docs.rs/tower/latest/tower/trait.Layer.html
//! [here]: https://docs.rs/governor/latest/governor/_guide/index.html
use std::{
    marker::PhantomData,
    pin::Pin,
    result::Result as StdResult,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use governor::{
    NotUntil, Quota, RateLimiter,
    clock::{Clock, DefaultClock},
    middleware::{NoOpMiddleware, RateLimitingMiddleware},
    state::{DirectStateStore, InMemoryState, NotKeyed},
};
use pin_project_lite::pin_project;
use tokio::time::{Sleep, sleep};
use tower::Service;

/// A [tower] [Service] that rate-limits requests using direct rate limiting
///
/// ## Constructing a Governor
///
/// The [Governor] type is a generic parameterized by four type variables:
///
/// - S: the "inner" service which will see its requests rate-limited by the [Governor]
/// - KS: an implementation of [DirectStateStore], which implements state storage for the Governor
/// - C: an implementation of the [Clock] trait, which defines the time source
/// - MW: an implementation of [RateLimitingMiddleware]
///
/// The caller will presumably already have a [Service] for which they're interested in
/// rate-limiting incoming requests. The other three are defined by the [governor] crate which also
/// supplies various implementations.
///
/// The simplest way to construct a [Governor] instance is the [default()](Governor::default) method, which
/// takes the inner [Service] and the [Quota] to be enforced, selecting default implementations for
/// all other types.
///
/// The most flexible way to construct a [Governor] instance is the [new()][Governor::new] method, which
/// will accept arguments of all four types.
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
#[derive(Clone, Debug)]
pub struct Governor<S, Request, KS, C, MW>
where
    // These trait bounds are all required by `RateLimiter`
    KS: DirectStateStore,
    C: Clock,
    MW: RateLimitingMiddleware<NotKeyed, C::Instant>,
{
    inner: S,
    phantom: PhantomData<Request>,
    limiter: Arc<RateLimiter<NotKeyed, KS, C, MW>>,
}

impl<S, Request> Governor<S, Request, InMemoryState, DefaultClock, NoOpMiddleware> {
    pub fn default(inner: S, quota: Quota) -> Self {
        Self {
            inner,
            phantom: PhantomData,
            limiter: Arc::new(RateLimiter::direct(quota)),
        }
    }
}

impl<S, Request, KS, C, MW> Governor<S, Request, KS, C, MW>
where
    KS: DirectStateStore,
    C: Clock,
    MW: RateLimitingMiddleware<NotKeyed, C::Instant>,
{
    pub fn new(inner: S, quota: Quota, state: KS, clock: C, middleware: MW) -> Self {
        Self {
            inner,
            phantom: PhantomData,
            limiter: Arc::new(RateLimiter::new(quota, state, clock, middleware)),
        }
    }
    pub fn new_with_limiter(inner: S, limiter: Arc<RateLimiter<NotKeyed, KS, C, MW>>) -> Self {
        Self {
            inner,
            phantom: PhantomData,
            limiter,
        }
    }
    #[cfg(test)]
    fn inner(&self) -> &S {
        &self.inner
    }
}

pin_project! {
    #[project = FutureStateProj]
    enum FutureState<F, KS, C, MW, S, Request>
    where S: Service<Request>,
          KS: DirectStateStore,
          C: Clock,
          MW: RateLimitingMiddleware<NotKeyed, C::Instant>,
    {
        Ready {
            #[pin]
            fut: Sleep,
            // Nothing else needs to be pinned:
            limiter: Arc<RateLimiter<NotKeyed, KS, C, MW>>,
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
    // Has to be `pub`, since it's named (as the associated type `Future`) in `Service`, below
    pub struct CallFuture<Request, S, KS, C, MW>
    where S: Service<Request>,
          KS: DirectStateStore,
          C: Clock,
          MW: RateLimitingMiddleware<NotKeyed, C::Instant>,
    {
        #[pin]
        state: FutureState<<S as Service<Request>>::Future, KS, C, MW, S, Request>,
    }
}

impl<Request, S, KS, C, MW> CallFuture<Request, S, KS, C, MW>
where
    S: Service<Request>,
    KS: DirectStateStore,
    C: Clock,
    MW: RateLimitingMiddleware<NotKeyed, C::Instant>,
{
    pub fn new_from_until(
        dur: Duration,
        limiter: Arc<RateLimiter<NotKeyed, KS, C, MW>>,
        inner: S,
        req: Request,
    ) -> Self {
        Self {
            state: FutureState::Ready {
                fut: sleep(dur),
                limiter,
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

impl<Request, S, KS, C, MW> Future for CallFuture<Request, S, KS, C, MW>
where
    S: Service<Request>,
    KS: DirectStateStore,
    C: Clock,
    MW: RateLimitingMiddleware<NotKeyed, C::Instant, NegativeOutcome = NotUntil<C::Instant>>,
{
    type Output = StdResult<<S as Service<Request>>::Response, <S as Service<Request>>::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        loop {
            match this.state.as_mut().project() {
                FutureStateProj::Ready {
                    fut,
                    limiter,
                    inner,
                    req,
                } => match fut.poll(cx) {
                    Poll::Ready(_) => {
                        // Take care to only move items *here*; if we're pending, then we don't want
                        // to disturb our state.
                        let limiter = limiter.clone();
                        let inner = inner.take();
                        let req = req.take();
                        match limiter.check() {
                            Ok(_) => this.state.set(FutureState::Call {
                                fut: inner.unwrap().call(req.unwrap()),
                            }),
                            Err(not_until) => this.state.set(FutureState::Ready {
                                fut: sleep(not_until.wait_time_from(limiter.clock().now())),
                                limiter,
                                inner,
                                req,
                            }),
                        }
                    }
                    Poll::Pending => {
                        return Poll::Pending;
                    }
                },
                FutureStateProj::Call { fut } => return fut.poll(cx),
            }
        }
    }
}

impl<S, Request, KS, C, MW> Service<Request> for Governor<S, Request, KS, C, MW>
where
    S: Service<Request> + Clone,
    KS: DirectStateStore,
    C: Clock,
    MW: RateLimitingMiddleware<NotKeyed, C::Instant, NegativeOutcome = NotUntil<C::Instant>>,
{
    type Response = S::Response;

    type Error = S::Error;

    type Future = CallFuture<Request, S, KS, C, MW>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        match self.limiter.check() {
            Ok(_) => CallFuture::new(self.inner.clone(), req),
            Err(not_until) => CallFuture::new_from_until(
                not_until.wait_time_from(self.limiter.clock().now()),
                self.limiter.clone(),
                self.inner.clone(),
                req,
            ),
        }
    }
}

/// A [tower] [Layer](tower::Layer) providing direct rate-limiting to an inner [Service]
///
/// ## Constructing a Layer
///
/// The type parameters are the same as for [Governor] (where they are documented in detail). The
/// simplest way to construct a [Layer] instance is with [default()](Layer::default) which will
/// select default implementations of all parameters. The most flexibile is [new()](Layer::new)
/// which allows maximum flexibility in selecting implementations. Finally, the caller can construct
/// a [RateLimiter] separately and use [new_with_limiter](Layer::new_with_limiter).
pub struct Layer<Request, KS, C, MW>
where
    KS: DirectStateStore,
    C: Clock,
    MW: RateLimitingMiddleware<NotKeyed, C::Instant>,
{
    limiter: Arc<RateLimiter<NotKeyed, KS, C, MW>>,
    phantom: PhantomData<Request>,
}

impl<Request> Layer<Request, InMemoryState, DefaultClock, NoOpMiddleware> {
    pub fn default(quota: Quota) -> Self {
        Self {
            limiter: Arc::new(RateLimiter::direct(quota)),
            phantom: PhantomData,
        }
    }
}

impl<Request, KS, C, MW> Layer<Request, KS, C, MW>
where
    KS: DirectStateStore,
    C: Clock,
    MW: RateLimitingMiddleware<NotKeyed, C::Instant>,
{
    // Making `Request` a type parameter is irritating, because without type annotations, the
    // compiler can't deduce what it is for these functions. If I don't, however, I can't write
    // the associated `Service` type in my `Layer` implementation below.
    pub fn new(quota: Quota, state: KS, clock: C, middleware: MW) -> Self {
        Self {
            limiter: Arc::new(RateLimiter::new(quota, state, clock, middleware)),
            phantom: PhantomData,
        }
    }
    pub fn new_with_limiter(limiter: RateLimiter<NotKeyed, KS, C, MW>) -> Self {
        Self {
            limiter: Arc::new(limiter),
            phantom: PhantomData,
        }
    }
}

impl<S, Request, KS, C, MW> tower::Layer<S> for Layer<Request, KS, C, MW>
where
    KS: DirectStateStore,
    C: Clock,
    MW: RateLimitingMiddleware<NotKeyed, C::Instant>,
{
    type Service = Governor<S, Request, KS, C, MW>;

    fn layer(&self, inner: S) -> Self::Service {
        Governor::new_with_limiter(inner, self.limiter.clone())
    }
}

#[cfg(test)]
mod test {
    use nonzero::nonzero;
    use tower::{Service, ServiceBuilder, ServiceExt};

    use crate::fixtures::RecordingService;

    use super::*;

    async fn requests<KS, C, MW>(
        mut gov: Governor<RecordingService, usize, KS, C, MW>,
        num_requests: usize,
    ) -> Vec<Duration>
    where
        KS: DirectStateStore,
        C: Clock,
        MW: RateLimitingMiddleware<NotKeyed, C::Instant, NegativeOutcome = NotUntil<C::Instant>>,
    {
        for i in 0..num_requests {
            gov.ready().await.unwrap().call(i).await.unwrap();
        }
        gov.inner().intervals()
    }

    #[tokio::test]
    async fn direct_rate_limiting_smoke_test() {
        let governor = Governor::new_with_limiter(
            RecordingService::new(),
            Arc::new(governor::RateLimiter::direct(Quota::per_second(nonzero!(
                1u32
            )))),
        );

        let intervals = requests(governor, 3).await;

        intervals.iter().for_each(|d| {
            assert!(d.as_millis() >= 999);
        });
    }

    #[tokio::test]
    async fn default() {
        let governor = Governor::<_, usize, _, _, _>::default(
            RecordingService::new(),
            Quota::per_second(nonzero!(100u32)),
        );

        let intervals = requests(governor, 101).await;

        // Rate-limiting should start showing-up on the last invocation:
        assert!(intervals.iter().last().unwrap().as_millis() > 1);
    }

    #[test]
    fn layer_tests() {
        async fn handle(n: usize) -> usize {
            n
        }

        let _ = ServiceBuilder::new()
            .layer(Layer::<usize, _, _, _>::default(Quota::per_second(
                nonzero!(10u32),
            )))
            .service_fn(handle);

        let _ = ServiceBuilder::new()
            .layer(Layer::<usize, _, _, _>::new(
                Quota::per_second(nonzero!(10u32)),
                InMemoryState::default(),
                DefaultClock::default(),
                NoOpMiddleware::default(),
            ))
            .service_fn(handle);

        let _ = ServiceBuilder::new()
            .layer(Layer::<usize, _, _, _>::new_with_limiter(
                RateLimiter::direct(Quota::per_second(nonzero!(10u32))),
            ))
            .service_fn(handle);
    }
}
