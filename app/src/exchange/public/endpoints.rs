//! Production hosts and public (unsigned) market-data paths. Production host names may appear
//! ONLY in this file (design D1); the signed client must never see them.
#![allow(dead_code)]

pub const BINANCE_HOST: &str = "https://fapi.binance.com";
pub const BINANCE_PREMIUM_INDEX: &str = "/fapi/v1/premiumIndex";
pub const BINANCE_FUNDING_INFO: &str = "/fapi/v1/fundingInfo";
pub const BINANCE_EXCHANGE_INFO: &str = "/fapi/v1/exchangeInfo";
pub const BINANCE_TICKER_24H: &str = "/fapi/v1/ticker/24hr";
pub const BINANCE_BOOK_TICKER: &str = "/fapi/v1/ticker/bookTicker";

pub const BYBIT_HOST: &str = "https://api.bybit.com";
pub const BYBIT_TICKERS: &str = "/v5/market/tickers";
pub const BYBIT_INSTRUMENTS_INFO: &str = "/v5/market/instruments-info";

pub const OKX_HOST: &str = "https://www.okx.com";
pub const OKX_FUNDING_RATE: &str = "/api/v5/public/funding-rate";
pub const OKX_MARK_PRICE: &str = "/api/v5/public/mark-price";
pub const OKX_TICKERS: &str = "/api/v5/market/tickers";
pub const OKX_TICKER: &str = "/api/v5/market/ticker";
pub const OKX_INSTRUMENTS: &str = "/api/v5/public/instruments";

pub fn binance_url(path_and_query: &str) -> String {
    format!("{BINANCE_HOST}{path_and_query}")
}

pub fn bybit_url(path_and_query: &str) -> String {
    format!("{BYBIT_HOST}{path_and_query}")
}

pub fn okx_url(path_and_query: &str) -> String {
    format!("{OKX_HOST}{path_and_query}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_host_plus_path() {
        assert_eq!(binance_url("/fapi/v1/premiumIndex?symbol=BTCUSDT"), "https://fapi.binance.com/fapi/v1/premiumIndex?symbol=BTCUSDT");
        assert_eq!(bybit_url(BYBIT_TICKERS), "https://api.bybit.com/v5/market/tickers");
        assert_eq!(okx_url(OKX_INSTRUMENTS), "https://www.okx.com/api/v5/public/instruments");
    }

    #[test]
    fn production_hosts_appear_in_no_other_public_source_file() {
        let hosts = ["fapi.binance.com", "api.bybit.com", "www.okx.com"];
        let others = [
            ("adapter.rs", include_str!("adapter.rs")),
            ("binance.rs", include_str!("binance.rs")),
            ("bybit.rs", include_str!("bybit.rs")),
            ("okx.rs", include_str!("okx.rs")),
            ("refetch.rs", include_str!("refetch.rs")),
        ];
        for (name, src) in others {
            for h in hosts {
                assert!(!src.contains(h), "{name} mentions production host {h}; only endpoints.rs may");
            }
        }
    }
}
