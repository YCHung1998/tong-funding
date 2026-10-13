//! WebSocket hosts for public market-data feeds. Public (unsigned, read-only) hosts may appear
//! only under `exchange/public`, never in the signed client (see `exchange::static_checks`).
#![allow(dead_code)]

/// Binance USD-M futures, all-market mark price array, 1-second push (design D3). Measured
/// 2026-10-05: ~1000 ms per symbol, versus ~3000 ms for the default `!markPrice@arr`.
pub const BINANCE_MARK_PRICE_WS_URL: &str = "wss://fstream.binance.com/market/ws/!markPrice@arr@1s";

/// Update period the stream promises (ms); feeds `FeedHealth`'s expected period.
pub const BINANCE_MARK_PRICE_PERIOD_MS: i64 = 1_000;

/// Bybit v5 public linear market stream: `tickers.<SYMBOL>` topics (snapshot first, then deltas that
/// carry only the changed fields). Public data, production host (same as the Bybit REST scan).
pub const BYBIT_TICKERS_WS_URL: &str = "wss://stream.bybit.com/v5/public/linear";

/// Topics per `subscribe` request: Bybit accepted 100 (9 requests for 801 symbols, all acknowledged,
/// measured 2026-10-13).
pub const BYBIT_SUBSCRIBE_CHUNK: usize = 100;

/// How often the application-level `{"op":"ping"}` is sent to keep the connection alive (ms).
pub const BYBIT_WS_PING_MS: u64 = 20_000;

/// Environment variable of the roll-back switch (read only in `ui::live`, like the Binance host
/// switch): `rest` turns the Bybit WebSocket off and leaves the REST poll as the only source.
pub const BYBIT_FEED_ENV: &str = "TONG_FUNDING_BYBIT_FEED";

/// Where the Bybit scan data comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BybitFeedMode {
    /// REST poll as the base with the WebSocket overlaid (the default).
    Ws,
    /// REST poll only (the roll-back).
    Rest,
}

impl BybitFeedMode {
    /// `rest` (any case, surrounding spaces ignored) selects [`BybitFeedMode::Rest`]; anything else,
    /// including a missing or empty value, is [`BybitFeedMode::Ws`].
    pub fn from_env_value(value: Option<&str>) -> BybitFeedMode {
        match value.map(str::trim) {
            Some(v) if v.eq_ignore_ascii_case("rest") => BybitFeedMode::Rest,
            _ => BybitFeedMode::Ws,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bybit_feed_mode_only_the_word_rest_turns_the_websocket_off() {
        for (value, mode) in [
            (None, BybitFeedMode::Ws),
            (Some(""), BybitFeedMode::Ws),
            (Some("ws"), BybitFeedMode::Ws),
            (Some("WS"), BybitFeedMode::Ws),
            (Some("garbage"), BybitFeedMode::Ws),
            (Some("restful"), BybitFeedMode::Ws),
            (Some("rest"), BybitFeedMode::Rest),
            (Some("REST"), BybitFeedMode::Rest),
            (Some("Rest"), BybitFeedMode::Rest),
            (Some("  rest \n"), BybitFeedMode::Rest),
        ] {
            assert_eq!(BybitFeedMode::from_env_value(value), mode, "{value:?}");
        }
    }

    #[test]
    fn bybit_ws_constants_have_the_agreed_values() {
        assert!(BYBIT_TICKERS_WS_URL.starts_with("wss://stream.bybit.com/"));
        assert_eq!((BYBIT_SUBSCRIBE_CHUNK, BYBIT_WS_PING_MS, BYBIT_FEED_ENV), (100, 20_000, "TONG_FUNDING_BYBIT_FEED"));
    }
}
