// Copyright (C) 2025-2026 Michael Herstine <sp1ff@pobox.com>
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

// I plan on adding some `KeyExtractor` implementations here. For now, I'm just going to add keyed
// middleware that I hope to contribute to the governor crate at some point.

#![cfg(feature = "dashmap")]

use std::{fmt::Debug, hash::Hash};

use dashmap::DashMap;
use governor::{
    NotUntil,
    clock::Reference,
    gcra::{Gcra, StateSnapshot},
    middleware::RateLimitingMiddleware,
};

#[derive(Debug)]
pub struct KeyedDashmapMiddleware<K: Eq + Hash> {
    keys: DashMap<K, Gcra>,
}

impl<T, K> From<T> for KeyedDashmapMiddleware<K>
where
    K: Eq + Hash,
    T: IntoIterator<Item = (K, Gcra)>,
{
    fn from(value: T) -> Self {
        Self {
            keys: DashMap::from_iter(value),
        }
    }
}

impl<K, P> RateLimitingMiddleware<K, P> for KeyedDashmapMiddleware<K>
where
    K: Debug + Eq + Hash,
    P: Reference,
{
    type PositiveOutcome = ();

    type NegativeOutcome = NotUntil<P>;

    fn allow(_: &K, _: impl Into<StateSnapshot>) -> Self::PositiveOutcome {}

    fn disallow(_: &K, state: impl Into<StateSnapshot>, start_time: P) -> Self::NegativeOutcome {
        NotUntil::new(state.into(), start_time)
    }

    fn check_quota(
        &self,
        key: &K,
        f: &dyn Fn(&Gcra) -> Result<Self::PositiveOutcome, Self::NegativeOutcome>,
    ) -> Option<Result<Self::PositiveOutcome, Self::NegativeOutcome>> {
        self.keys.get(key).map(|gcra| f(gcra.value()))
    }
}
