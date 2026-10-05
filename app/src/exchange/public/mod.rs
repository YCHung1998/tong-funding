//! Public (unsigned) market-data clients. The ONLY place production host names may appear.
//! Owned by the public-adapters work, except `feed_endpoints.rs` (feed work).
#![allow(dead_code)]

pub mod adapter;
pub mod binance;
pub mod bybit;
pub mod endpoints;
pub mod feed_endpoints;
pub mod okx;
pub mod refetch;
