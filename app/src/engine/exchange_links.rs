//! live-guardrails 4.1: "go to the exchange" links. Pure functions: the URL holds only the
//! exchange's public domain and the trading pair (no key, no signature, no query string). Opening
//! it in the default browser is the caller's job (integration phase).
//!
//! Formats (design decision 11; to be re-checked against the live sites at integration):
//! Binance `https://www.binance.com/en/futures/<SYMBOL>`, Bybit `https://www.bybit.com/trade/usdt/<SYMBOL>`.

use tong_funding_core::types::Exchange;

pub const BINANCE_HOST: &str = "www.binance.com";
pub const BYBIT_HOST: &str = "www.bybit.com";

/// A symbol is accepted only as upper-case ASCII letters and digits (as the exchanges name USDT
/// perpetuals), so nothing but the pair can ever reach the path.
fn plain_symbol(symbol: &str) -> bool {
    !symbol.is_empty() && symbol.len() <= 32 && symbol.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// Web trading page of `symbol` on `exchange`; `None` for an exchange without a live link (OKX is
/// not traded live) or a symbol that is not plain.
pub fn futures_url(exchange: Exchange, symbol: &str) -> Option<String> {
    if !plain_symbol(symbol) {
        return None;
    }
    match exchange {
        Exchange::Binance => Some(format!("https://{BINANCE_HOST}/en/futures/{symbol}")),
        Exchange::Bybit => Some(format!("https://{BYBIT_HOST}/trade/usdt/{symbol}")),
        Exchange::Okx => None,
    }
}

/// Both legs' links in the order (long, short).
pub fn pair_urls(long: Exchange, short: Exchange, symbol: &str) -> [Option<String>; 2] {
    [futures_url(long, symbol), futures_url(short, symbol)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_hold_only_the_public_domain_and_the_pair() {
        assert_eq!(futures_url(Exchange::Binance, "BTCUSDT").as_deref(), Some("https://www.binance.com/en/futures/BTCUSDT"));
        assert_eq!(futures_url(Exchange::Bybit, "1000PEPEUSDT").as_deref(), Some("https://www.bybit.com/trade/usdt/1000PEPEUSDT"));
        for ex in [Exchange::Binance, Exchange::Bybit] {
            let url = futures_url(ex, "ETHUSDT").unwrap();
            assert!(url.starts_with("https://"));
            assert!(!url.contains('?') && !url.contains('#') && !url.contains('@') && !url.contains('&'), "{url}");
            for secret_word in ["key", "sign", "secret", "token", "timestamp"] {
                assert!(!url.to_lowercase().contains(secret_word), "{url}");
            }
            let host = url.trim_start_matches("https://").split('/').next().unwrap();
            assert!(host == BINANCE_HOST || host == BYBIT_HOST, "{host}");
        }
    }

    #[test]
    fn a_symbol_that_is_not_plain_gets_no_link() {
        for bad in ["", "btcusdt", "BTC/USDT", "BTCUSDT?x=1", "BTCUSDT#", "BTC USDT", "../x", "BTC\nUSDT", "幣USDT", &"A".repeat(33)] {
            assert_eq!(futures_url(Exchange::Binance, bad), None, "{bad:?}");
            assert_eq!(futures_url(Exchange::Bybit, bad), None, "{bad:?}");
        }
        assert_eq!(futures_url(Exchange::Okx, "BTCUSDT"), None, "no live link for OKX");
    }

    #[test]
    fn a_pair_gets_one_link_per_leg() {
        let [l, s] = pair_urls(Exchange::Binance, Exchange::Bybit, "SOLUSDT");
        assert!(l.unwrap().contains("binance.com") && s.unwrap().contains("bybit.com"));
    }
}
