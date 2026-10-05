//! Clock sync, rate-limit backoff, the Binance mark-price feed and the freshness-aware cache.
//! Owned by the feed/health work.
#![allow(dead_code)]

pub mod cache;
pub mod clock_sync;
pub mod feed;
pub mod gated;
pub mod ratelimit;
