

# Introduction

tower-gcra is a [tower](https://github.com/tower-rs/tower) middleware that provides rate-limiting; that is, it limits that rate at which requests are processed by the [Service](https://docs.rs/tower/latest/tower/trait.Service.html) which it wraps. It does so via the [Generic Cell Rate Algorithm](https://brandur.org/rate-limiting) (AKA GCRA). [GCRA](https://en.wikipedia.org/wiki/Generic_cell_rate_algorithm) is a particularly effective variant of the [Leaky Bucket](https://grokipedia.com/page/Leaky_bucket) approach to rate-limiting.

In particular, tower-gcra provides for both "direct" and "keyed" rate-limiting. The former defines a single quota governing the rate at which all requests will be processed. The latter defines "per-key" state, where a key is some aspect of the requests being serviced (the peer IP address, for instance), so that the quota applies per instance of that key (when the key is peer IP address, this would mean that each peer IP is rate-limited individually). Furthermore, tower-gcra allows per-key <span class="underline">quotas</span> as well (so, for instance, we could build a rate-limiting client that applies different rate limits depending on the destination).

tower-gcra depends on the [governor](https://github.com/antifuchs/governor) crate for the core GCRA implementation. At the time of this writing, the per-key quota functionality depends on an as-yet-unmerged governor [PR](https://github.com/boinkor-net/governor/pull/292).


## Comparisons To Other tower Rate-Limiting Crates


### RateLimit

[tower](https://github.com/tower-rs/tower) ships with a rate-limiting middleware: [RateLimit](https://docs.rs/tower/latest/tower/limit/rate/struct.RateLimit.html). [RateLimit](https://docs.rs/tower/latest/tower/limit/rate/struct.RateLimit.html) "enforces a rate limit on the number of requests the underlying service can handle over a period of time." And indeed, it counts the number of requests per given unit of time & will pend any request over & above the given permissible number. In this way it differs from Leaky Bucket in that a client can make requests at an arbitrarily high rate until it exhausts its quota for any given time period.

When rate-limited, calls to `poll_ready()` will pend (not `call()`), which seems preferrable. This crate doesn't do that because without a request, we can't extract a key for rate-limiting.

[RateLimit](https://docs.rs/tower/latest/tower/limit/rate/struct.RateLimit.html) is not `Clone`, which can be inconvenient. For instance, [RetryLayer](https://docs.rs/tower/latest/tower/retry/struct.Retry.html) requries that the `Service` it wraps be `Clone`. The suggested [workaround](https://github.com/tokio-rs/axum/discussions/987#discussioncomment-2678595) is to wrap it in a [BufferLayer](https://docs.rs/tower/latest/tower/buffer/struct.BufferLayer.html). Regrettably, that erases the inner error type to `Box<dyn Error + Send + Sync>`.


### tower-governor

[tower-governor](https://docs.rs/tower_governor/latest/tower_governor/index.html) is another tower middleware building on the governor crate, but is explicitly targeted at HTTP servers. For instance, it only implements `Service` for [http](https://docs.rs/http/latest/http/index.html) [Request](https://docs.rs/http/latest/http/request/struct.Request.html)s, and rather than pending rate-limited requests, it returns an HTTP 429 Too Many Requests response, taking care to insert "X-RateLimit-After" and "Retry-After" headers.

It is, however, `Clone`. It, like this crate, handles rate-limiting in `call()` (rather than `poll_ready()`), presumably also because it has access to the incoming request there. Lastly, at the time of this writing, it doesn't support per-key quotas.


# License

tower-gcra is released under the [GPL v3.0](https://spdx.org/licenses/GPL-3.0-or-later.html).


# Prerequisites

tower-gcra uses the Rust 2024 edition, so Rust 1.85 is required.


# Installation

    cargo add tower-gcra


# Status & Roadmap

This is a new crate. I've chosen the version number (0.x) in an attempt to convey that.

Bugs, comments, suggestions and contributions are welcome, on [X](https://x.com/unwoundstack), [Mastodon](https://indieweb.social/@sp1ff), in the [issues](https://github.com/sp1ff/tower-gcra/issues) or at [sp1ff@pobox.com](mailto:sp1ff@pobox.com).

