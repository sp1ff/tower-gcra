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

//! # tower-client-governor
//!
//! ## On why I was unable to implement this using one type for the Governor that can do both direct &
//! keyed rate-limiting.
//!
//! ### Trait bounds on generic trait methods
//!
//! The way to understand trait bounds on traits and on generic methods in traits is as a contract.
//! The trait defines the contract and the implementor must fulfill it. The implementor may provide
//! *looser* trait bounds, because they're over-delivering on the trait's contract. The implementor
//! may even impose trait bounds that appear to be stricter, so long as those bounds are provable
//! given the implementation (on which more below). But the implementor may *not* impose stricter
//! bounds that may not be satisified by the implementation, because theny they'd be under-delivering
//! on the contract.
//!
//! Here's a nice illustration from the conversation at
//! <https://users.rust-lang.org/t/about-trait-generic-method-bounds-propagation-to-impls/63941/9>:
//!
//! ```rust
//! use std::borrow::Borrow;
//!
//! pub trait Foo { fn method() where Self: Copy; }
//! pub trait Bar { fn method() where Self: Copy; }
//! pub trait Quz { fn method() where Self: Copy; }
//! pub trait Zap { fn method() where Self: Copy; }
//! pub trait Lop { fn method() where Self: Copy; }
//!
//! // This works because we're implementing trait `Foo` with fewer trait bounds than specified in
//! // the definition of the trait (we've dropped the requirement that Self be Copy, so we're
//! // over-delivering).
//! impl Foo for String {
//!     fn method() {
//!         println!("No complaints");
//!     }
//! }
//!
//! /*
//! // error[E0277]: the trait bound `String: Copy` is not satisfied
//! impl Bar for String {
//!     // The same error happens if unrelated (e.g. `Self: Borrow<u32>`)
//!     fn method() where Self: Copy {
//!         println!("No complaints");
//!     }
//! }
//! */
//!
//! /*
//! // error[E0276]: impl has stricter requirements than trait
//! impl<T> Quz for T {
//!     fn method() where Self: Borrow<u32> {
//!         println!("Also no complaints");
//!     }
//! }
//! */
//!
//! impl<T: Borrow<u32>> Zap for T {
//!     // OK as this is a vacuous bound within the context of this `impl`
//!     fn method() where Self: Borrow<u32> {
//!         println!("Also no complaints");
//!     }
//! }
//!
//! impl Lop for String {
//!     // OK as these are vacuous bounds within the context of this `impl`
//!     fn method() where Self: Clone + Borrow<str> {
//!         println!("No complaints");
//!     }
//! }
//! ```
//!
//! ### Rust Coherence & Trait Specialization
//!
//! Rust's coherence rule says that for any type that implements a trait, there must be precisely one
//! implementation of that trait; it's not like the compiler will consider multiple implementations,
//! rank them somehow, and choose the "best".
//!
//! In particular, this isn't like template specialization in C++.
//!
//! So this, for instance doesn't work:
//!
//! ```rust
//! trait Trait {
//!     fn foo(&self);
//! }
//!
//! impl<T> Trait for T
//! where T: std::fmt::Display {
//!     fn foo(&self) {
//!         println!("foo!");
//!     }
//! }
//!
//! // error[E0119]: conflicting implementations of trait `Trait`
//! // impl<T> Trait for T
//! // where T: std::fmt::Debug {
//! //     fn foo(&self) {
//! //         println!("{:#?}", self);
//! //     }
//! // }
//!
//! fn main() {
//!     let x = 1;
//!     x.foo();
//! }
//! ```
//!
//! It would be really nice, in this case, to be able to define a trait and provide a general-purpose
//! implmentation, then implement it just for, say, `NotKeyed` and give an alternate implementation
//! That's essentially not supported in Rust (right now, there's something on nightly that I haven't
//! looked into). You can *kind* of get it like this:
//!
//! ```rust
//! pub enum NotKeyed {
//!     NonKey,
//! }
//!
//! trait CanUseDefault {
//!     fn call_impl(&self) {
//!         println!("Default impl!");
//!     }
//! }
//!
//! impl CanUseDefault for NotKeyed {
//!     fn call_impl(&self) {
//!         println!("Direct impl!")
//!     }
//! }
//!
//! impl CanUseDefault for bool {}
//!
//! trait Service {
//!     fn call(&self);
//! }
//!
//! struct Governor<T> {
//!     x: T,
//! }
//!
//! impl<T> Service for Governor<T>
//! where T: CanUseDefault
//! {
//!     fn call(&self) {
//!         self.x.call_impl();
//!     }
//! }
//!
//! fn main() {
//!     let x = Governor { x: NotKeyed::NonKey, };
//!     x.call();
//!     let y = Governor { x: true, };
//!     y.call();
//!     // the method `call` exists for struct `Governor<u32>`, but its trait bounds were not satisfied
//!     // ...
//!     // note: trait bound `u32: CanUseDefault` was not satisfied
//!     // let z = Governor { x: 1u32 };
//!     // z.call();
//! }
//! ```
//!
//! The game is to define a trait with a provided implementation defining the generic implementation,
//! then implement the trait for just one type and override the provided implementation. The catch is
//! that if you want to use the provided behavior for any other type, you have to implement the
//! trait, accepting the default behavior.
//!
//! ### Why This Won't Work Here
//!
//! Rust's coherence rule and the rules around trait bounds combined to block me writing a single
//! `Governor` type that could handle both direct & keyed rate limiting (or, at least, I was unable
//! to find a way around them).
//!
//! It was easy enough to write such a `Governor` struct:
//!
//! ```rust
//! use std::{convert::Infallible, marker::PhantomData, sync::Arc, error::Error as StdError, result::Result as StdResult};
//!
//! use governor::{clock::Clock, middleware::RateLimitingMiddleware, RateLimiter, state::{StateStore, direct::NotKeyed}};
//!
//! pub trait KeyExtractor<Request> {
//!     type Key;
//!     type Error: StdError + Send + Sync + 'static;
//!     fn extract(&self, req: &Request) -> StdResult<Self::Key, Self::Error>;
//! }
//!
//! #[derive(Clone, Debug)]
//! pub struct DirectKeyExtractor;
//!
//! impl<Request> KeyExtractor<Request> for DirectKeyExtractor {
//!     type Key = NotKeyed;
//!     type Error = Infallible;
//!     fn extract(&self, _: &Request) -> StdResult<Self::Key, Self::Error> {
//!         Ok(NotKeyed::NonKey)
//!     }
//! }
//!
//! #[derive(Clone)]
//! pub struct Governor<S, KE, Request, KS, C, MW>
//! where
//!     KE: KeyExtractor<Request> + Clone,
//!     KS: StateStore<Key = <KE as KeyExtractor<Request>>::Key>,
//!     C: Clock,
//!     MW: RateLimitingMiddleware<<KE as KeyExtractor<Request>>::Key, C::Instant>,
//! {
//!     inner: S,
//!     key_extractor: KE,
//!     phantom: PhantomData<Request>,
//!     limiter: Arc<RateLimiter<<KE as KeyExtractor<Request>>::Key, KS, C, MW>>,
//! }
//! ```
//!
//! The problem came when it was time to implement [tower::Service]. I naively wrote two
//! implementations, using two different sets of trait bounds: one appropriate for keyed rate
//! limiting, one appropriate for direct. I assumed that the compiler would pick one or the other,
//! either via the trait bounds or through something like C++ template specialization (since the
//! second implementation fixed the key extractor as `DirectKeyExtractor`)-- no dice: "Conflicting
//! implementation for `Governor<_, DirectKeyExtractor, _, _, _, _>`".
//!
//! Then I thought to exploit the trait specialization trick: I could abstract-out the behavior I
//! needed (calling `until_ready()` or `until_key_ready()`) into `KeyExtractor`, provide an
//! implementation suitable for keyed rate limiting, and specialize in `DirectKeyExtractor` for
//! direct rate limiting.
//!
//! The problem there was that, since the new method would necessarily take a `RateLimiter`, I
//! couldn't write the trait bounds appropriately:
//!
//! - the methods `until_ready()` and `until_key_ready()` are only implemented on `RateLimiter` for
//! certain trait bounds on the generic type parameters thereto, so the only way I could prove to
//! the type checker that the methods I was calling were available was to write those trait bounds
//! into my trait method's signatures... which made them incompatible. IOW, my specialized
//! implementation of the trait method did not, and *could not* satisfy the same contract as the
//! provided, more general implementation.
//!
//! ### Conclusion
//!
//! I conclude that in Rust, if I have a type `T` that has type parameters `a`, `b`, `c`..., I _can_
//! express that "type `T<a, b, c...>` implements x if `a`, `b`, `c` satisfy the following
//! conditions," where "f" is a method or a trait. I can _not_, however, express, "type `T<a, b,
//! c...>` implements f if `a`, `b`, `c` satisfy the following conditions, or if they satisfy the
//! alternative set of conditions..."
//!
//! For what it's worth, I peeked at the code for tower-governor, and the author there didn't even
//! attempt to support the direct case.

use std::{
    cmp::Eq, error::Error as StdError, hash::Hash, marker::PhantomData,
    result::Result as StdResult, sync::Arc,
};

use futures::future::BoxFuture;
use governor::{
    NotUntil, RateLimiter,
    clock::{Clock, ReasonablyRealtime},
    middleware::RateLimitingMiddleware,
    state::{DirectStateStore, NotKeyed, keyed::KeyedStateStore},
};
use snafu::prelude::*;
use tower::Service;

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("While polling our inner service for readiness, {source}"))]
    // I hate erasing the underlying error type, here
    Call {
        source: Box<dyn StdError + Send + Sync + 'static>,
    },
    #[snafu(display("While extracting the key from our request, {source}"))]
    // I hate erasing the underlying error type, here
    Key {
        source: Box<dyn StdError + Send + Sync + 'static>,
    },
    #[snafu(display("While polling our inner service for readiness, {source}"))]
    // I hate erasing the underlying error type, here
    PollReady {
        source: Box<dyn StdError + Send + Sync + 'static>,
    },
}

// This is similar to tower_governor::key_extractor::KeyExtractor, except his implementation
// expects an http::request::Request<T>. IOW, I think `tower-governor` is meant to work with
// tower-http, whereas this can work with just tower.
pub trait KeyExtractor<Request> {
    type Key: std::hash::Hash;
    type Error: StdError + Send + Sync + 'static;
    fn extract(&self, req: &Request) -> StdResult<Self::Key, Self::Error>;
}

/// A [tower] [Service] that rate-limits requests using keyed rate limiting
// Do I actually want this to be Clone? How to prevent evading rate limits by simply cloning
// instances? Why can this be Clone, but tower::limit::rate::RateLimit can't?
// I think it's OK, since the rate-limiting will be in the (shared, single instance) of
// the RateLimiter itself.
#[derive(Clone)]
pub struct KeyedGovernor<S, KE, Request, KS, C, MW>
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

impl<S, KE, Request, KS, C, MW> KeyedGovernor<S, KE, Request, KS, C, MW>
where
    KE: KeyExtractor<Request> + Clone,
    KS: KeyedStateStore<<KE as KeyExtractor<Request>>::Key>,
    C: Clock,
    MW: RateLimitingMiddleware<<KE as KeyExtractor<Request>>::Key, C::Instant>,
{
    pub fn new(
        inner: S,
        key_extractor: KE,
        limiter: RateLimiter<<KE as KeyExtractor<Request>>::Key, KS, C, MW>,
    ) -> Self {
        Self {
            inner,
            key_extractor,
            phantom: PhantomData,
            limiter: Arc::new(limiter),
        }
    }
    pub fn inner(&self) -> &S {
        &self.inner
    }
}

// This implementation is for keyed rate limiting
impl<S, KE, Request, KS, C, MW> Service<Request> for KeyedGovernor<S, KE, Request, KS, C, MW>
where
    // Boy this is gnarly... can I get out of this, someow?
    S: Service<Request> + Clone + Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
    <S as Service<Request>>::Error: StdError + Send + Sync + 'static,
    <S as Service<Request>>::Response: Send,
    KE: KeyExtractor<Request> + Clone,
    <KE as KeyExtractor<Request>>::Key: Clone + Eq + Hash + Send + Sync + 'static,
    Request: Send + 'static,
    KS: KeyedStateStore<<KE as KeyExtractor<Request>>::Key> + Send + Sync + 'static,
    C: ReasonablyRealtime + Send + Sync + 'static,
    MW: RateLimitingMiddleware<
            <KE as KeyExtractor<Request>>::Key,
            C::Instant,
            NegativeOutcome = NotUntil<C::Instant>,
        > + Send
        + Sync
        + 'static,
    <MW as RateLimitingMiddleware<<KE as KeyExtractor<Request>>::Key, C::Instant>>::PositiveOutcome:
        Send,
{
    type Response = S::Response;

    type Error = Error;

    type Future = BoxFuture<'static, StdResult<Self::Response, Self::Error>>;

    // Ideally, I'd return Poll::Pending here if we're rate-limited. Unfortunately, the signature
    // gives me no access to the key! So, I'll have to do the check in `call()` (while honoring
    // our wrapped Service's `poll_ready()`, of course).
    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(|err| Error::PollReady {
            source: Box::new(err),
        })
    }

    fn call(&mut self, req: Request) -> Self::Future {
        use futures::future::FutureExt;

        let key = match self.key_extractor.extract(&req) {
            Ok(key) => key,
            Err(err) => {
                return futures::future::err(Error::Key {
                    source: Box::new(err),
                })
                .boxed();
            }
        };

        let limiter = self.limiter.clone();
        let mut inner = self.inner.clone();
        Box::pin(async move {
            limiter
                .until_key_ready(&key)
                .then(|_| inner.call(req))
                .await
                .map_err(|err| Error::Call {
                    source: Box::new(err),
                })
        })
    }
}

/// A [tower] [Service] that rate-limits requests using direct rate limiting
// Do I actually want this to be Clone? How to prevent evading rate limits by simply cloning
// instances? Why can this be Clone, but tower::limit::rate::RateLimit can't?
// I think it's OK, since the rate-limiting will be in the (shared, single instance) of
// the RateLimiter itself.
#[derive(Clone)]
pub struct DirectGovernor<S, Request, KS, C, MW>
where
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
    pub fn new(inner: S, limiter: RateLimiter<NotKeyed, KS, C, MW>) -> Self {
        Self {
            inner,
            phantom: PhantomData,
            limiter: Arc::new(limiter),
        }
    }
    pub fn inner(&self) -> &S {
        &self.inner
    }
}

// This implementation is for direct rate lmiters
impl<S, Request, KS, C, MW> Service<Request> for DirectGovernor<S, Request, KS, C, MW>
where
    // Boy this is gnarly... can I get out of this, someow?
    S: Service<Request> + Clone + Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
    <S as Service<Request>>::Error: StdError + Send + Sync + 'static,
    <S as Service<Request>>::Response: Send,
    Request: Send + 'static,
    KS: DirectStateStore + Send + Sync + 'static,
    C: ReasonablyRealtime + Send + Sync + 'static,
    MW: RateLimitingMiddleware<NotKeyed, C::Instant, NegativeOutcome = NotUntil<C::Instant>>
        + Send
        + Sync
        + 'static,
    <MW as RateLimitingMiddleware<NotKeyed, C::Instant>>::PositiveOutcome: Send,
{
    type Response = S::Response;

    type Error = Error;

    type Future = BoxFuture<'static, StdResult<Self::Response, Self::Error>>;

    // Ideally, I'd return Poll::Pending here if we're rate-limited. Unfortunately, the signature
    // gives me no access to the key! So, I'll have to do the check in `call()` (while honoring
    // our wrapped Service's `poll_ready()`, of course).
    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(|err| Error::PollReady {
            source: Box::new(err),
        })
    }

    fn call(&mut self, req: Request) -> Self::Future {
        use futures::future::FutureExt;

        let limiter = self.limiter.clone();
        let mut inner = self.inner.clone();
        Box::pin(async move {
            limiter
                .until_ready()
                .then(|_| inner.call(req))
                .await
                .map_err(|err| Error::Call {
                    source: Box::new(err),
                })
        })
    }
}

#[cfg(test)]
mod first_test {

    // Let's try the simplest case possible, just to validate what I've coded-up so far.

    // I'll need a `Service` to wrap, and it should record the times at which it was called. Let's
    // have it take "requests" of type `usize`, and have it echo the request back to the caller.
    use std::{collections::HashMap, convert::Infallible, num::NonZeroU32, sync::RwLock};

    use governor::{Quota, clock::QuantaClock, gcra::Gcra};
    use itertools::Itertools;
    use tower::ServiceExt;

    use super::*;

    #[derive(Debug, Clone)]
    struct RecordingService {
        calls: Arc<RwLock<Vec<(usize, std::time::Instant)>>>,
    }

    impl RecordingService {
        pub fn new() -> Self {
            Self {
                calls: Arc::new(RwLock::new(Vec::new())),
            }
        }
        pub fn intervals(&self) -> Vec<std::time::Duration> {
            self.calls
                .read()
                .unwrap()
                .iter()
                .tuple_windows()
                .map(|((_, then), (_, now))| *now - *then)
                .collect()
        }
    }

    impl Service<usize> for RecordingService {
        type Response = usize;

        type Error = Infallible;

        type Future = futures::future::Ready<Result<usize, Infallible>>;

        fn poll_ready(
            &mut self,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<StdResult<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: usize) -> Self::Future {
            self.calls
                .write()
                .unwrap()
                .push((req, std::time::Instant::now()));
            eprintln!(
                "Recording service called: {:#?}",
                self.calls.read().unwrap()
            );
            futures::future::ok(req)
        }
    }

    #[tokio::test]
    async fn direct_rate_limiting_smoke_test() {
        let inner = RecordingService::new();
        let limiter = governor::RateLimiter::direct(Quota::per_second(NonZeroU32::new(1).unwrap()));
        let mut governor = DirectGovernor::new(inner, limiter);

        for i in 0..3 {
            governor.ready().await.unwrap().call(i).await.unwrap();
        }
        governor.inner().intervals().iter().for_each(|d| {
            eprintln!("{}", d.as_millis());
            assert!(d.as_millis() >= 999);
        });
    }

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

    // Next, I need some middleware. Seems like this should be in the governor crate (?)
    #[derive(Debug)]
    struct KeyedHashmapMiddleware {
        // Pretty-sure we need to be clone, so...
        keys: Arc<RwLock<HashMap<usize, Gcra>>>,
    }

    impl KeyedHashmapMiddleware {
        pub fn new() -> Self {
            Self {
                keys: Arc::new(RwLock::new(HashMap::from([(
                    1,
                    Gcra::new(Quota::per_second(NonZeroU32::new(2).unwrap())),
                )]))),
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
            // self.keys.read().unwrap().get(key).map(|gcra| f(gcra))
            let res = self.keys.read().unwrap().get(key).map(|gcra| f(gcra));
            eprintln!("check_quota({key}) returning {res:#?}");
            res
        }
    }

    #[tokio::test]
    async fn keyed_rate_limiting_smoke_test() {
        let inner = RecordingService::new();
        let key_extractor = RecordingRequestKeyExtractor;
        let limiter = governor::RateLimiter::keyed(Quota::per_second(NonZeroU32::new(1).unwrap()))
            .use_middleware(KeyedHashmapMiddleware::new());
        let mut governor = KeyedGovernor::new(inner, key_extractor, limiter);

        governor.ready().await.unwrap().call(0).await.unwrap();
        governor.ready().await.unwrap().call(0).await.unwrap(); // 0: Should be rate limited
        governor.ready().await.unwrap().call(1).await.unwrap(); // 1: Should go through
        governor.ready().await.unwrap().call(1).await.unwrap(); // 2: Should go through
        governor.ready().await.unwrap().call(0).await.unwrap(); // 3: Should be rate limited
        governor.ready().await.unwrap().call(2).await.unwrap(); // 4: Should go through-- new key

        eprintln!("{:#?}", governor.inner());

        let intervals = governor.inner().intervals();
        eprintln!("{}", intervals[0].as_millis());
        assert!(intervals[0].as_millis() >= 999);
        eprintln!("{}", intervals[1].as_millis());
        assert!(intervals[1].as_millis() < 1);
        eprintln!("{}", intervals[2].as_millis());
        assert!(intervals[2].as_millis() < 1);
        eprintln!("{}", intervals[3].as_millis());
        assert!(intervals[3].as_millis() >= 999);
        eprintln!("{}", intervals[4].as_millis());
        assert!(intervals[4].as_millis() < 1);
    }
}
