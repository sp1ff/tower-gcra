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

use std::{
    marker::PhantomData,
    pin::Pin,
    result::Result as StdResult,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use governor::{
    NotUntil, RateLimiter,
    clock::Clock,
    middleware::RateLimitingMiddleware,
    state::{DirectStateStore, NotKeyed},
};
use pin_project_lite::pin_project;
use tokio::time::{Sleep, sleep};
use tower::Service;

/// A [tower] [Service] that rate-limits requests using direct rate limiting
// Do I actually want this to be Clone? How to prevent evading rate limits by simply cloning
// instances? Why can this be Clone, but tower::limit::rate::RateLimit can't?
// I think it's OK, since the rate-limiting will be in the (shared, single instance) of
// the RateLimiter itself.
#[derive(Clone, Debug)]
pub struct DirectGovernor<S, Request, KS, C, MW>
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

impl<S, Request, KS, C, MW> DirectGovernor<S, Request, KS, C, MW>
where
    KS: DirectStateStore,
    C: Clock,
    MW: RateLimitingMiddleware<NotKeyed, C::Instant>,
{
    pub fn new(inner: S, limiter: Arc<RateLimiter<NotKeyed, KS, C, MW>>) -> Self {
        Self {
            inner,
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

impl<S, Request, KS, C, MW> Service<Request> for DirectGovernor<S, Request, KS, C, MW>
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

pub struct Layer<Request, KS, C, MW>
where
    KS: DirectStateStore,
    C: Clock,
    MW: RateLimitingMiddleware<NotKeyed, C::Instant>,
{
    limiter: Arc<RateLimiter<NotKeyed, KS, C, MW>>,
    phantom: PhantomData<Request>,
}

impl<S, Request, KS, C, MW> tower::Layer<S> for Layer<Request, KS, C, MW>
where
    KS: DirectStateStore,
    C: Clock,
    MW: RateLimitingMiddleware<NotKeyed, C::Instant>,
{
    type Service = DirectGovernor<S, Request, KS, C, MW>;

    fn layer(&self, inner: S) -> Self::Service {
        DirectGovernor::new(inner, self.limiter.clone())
    }
}

#[cfg(test)]
mod first_test {
    use governor::Quota;
    use nonzero::nonzero;
    use tower::ServiceExt;

    use super::*;
    use crate::fixtures::RecordingService;

    #[tokio::test]
    async fn direct_rate_limiting_smoke_test() {
        let inner = RecordingService::new();
        let limiter = Arc::new(governor::RateLimiter::direct(Quota::per_second(nonzero!(
            1u32
        ))));
        let mut governor = DirectGovernor::new(inner, limiter);

        for i in 0..3 {
            governor.ready().await.unwrap().call(i).await.unwrap();
        }
        governor.inner().intervals().iter().for_each(|d| {
            eprintln!("{}", d.as_millis());
            assert!(d.as_millis() >= 999);
        });
    }
}
