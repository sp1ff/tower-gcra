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

#![cfg(test)]

//! # A Trivial Service for Testing Purposes
//!
//! I suppose it would be more accurate to call this `test-scaffolding` or something like that, but
//! this is more succinct.
use std::{
    convert::Infallible,
    result::Result as StdResult,
    sync::{Arc, RwLock},
};

use itertools::Itertools;
use tower::Service;

#[derive(Debug, Clone)]
pub(crate) struct RecordingService {
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
        futures::future::ok(req)
    }
}
