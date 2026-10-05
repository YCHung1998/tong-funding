//! WebSocket hosts for public market-data feeds. Public (unsigned, read-only) hosts may appear
//! only under `exchange/public`, never in the signed client (see `exchange::static_checks`).
#![allow(dead_code)]

/// Binance USD-M futures, all-market mark price array, 1-second push (design D3). Measured
/// 2026-10-05: ~1000 ms per symbol, versus ~3000 ms for the default `!markPrice@arr`.
pub const BINANCE_MARK_PRICE_WS_URL: &str = "wss://fstream.binance.com/market/ws/!markPrice@arr@1s";

/// Update period the stream promises (ms); feeds `FeedHealth`'s expected period.
pub const BINANCE_MARK_PRICE_PERIOD_MS: i64 = 1_000;
