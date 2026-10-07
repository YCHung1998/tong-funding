//! Compile-time endpoint constants of the signed clients. Only demo/testnet hosts exist here:
//! there is no constructor taking a base URL and no host is read from the environment or a file.
//! The host is chosen by the enums below, which is the only knob a caller has.

/// Binance USDS-M Futures hosts that may receive signed requests.
/// UNVERIFIED which of the two accepts a user's demo key (task 4.2 decides); both are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinanceHost {
    /// `testnet.binancefuture.com` (default in the Python reference's `.env.example`).
    Testnet,
    /// `demo-fapi.binance.com` (fallback named in the same file).
    Demo,
}

/// Bybit v5 hosts that may receive signed requests (Demo Trading only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BybitHost {
    Demo,
}

pub const BINANCE_TESTNET_HOST: &str = "testnet.binancefuture.com";
pub const BINANCE_DEMO_HOST: &str = "demo-fapi.binance.com";
pub const BYBIT_DEMO_HOST: &str = "api-demo.bybit.com";

/// Every host name a signed request may ever be sent to.
pub const ALLOWED_SIGNED_HOSTS: [&str; 3] = [BINANCE_TESTNET_HOST, BINANCE_DEMO_HOST, BYBIT_DEMO_HOST];

impl BinanceHost {
    pub fn host(self) -> &'static str {
        match self {
            BinanceHost::Testnet => BINANCE_TESTNET_HOST,
            BinanceHost::Demo => BINANCE_DEMO_HOST,
        }
    }
    pub fn base_url(self) -> &'static str {
        match self {
            BinanceHost::Testnet => "https://testnet.binancefuture.com",
            BinanceHost::Demo => "https://demo-fapi.binance.com",
        }
    }
}

impl BybitHost {
    pub fn host(self) -> &'static str {
        match self {
            BybitHost::Demo => BYBIT_DEMO_HOST,
        }
    }
    pub fn base_url(self) -> &'static str {
        match self {
            BybitHost::Demo => "https://api-demo.bybit.com",
        }
    }
}

pub const BINANCE_BALANCE_PATH: &str = "/fapi/v2/balance";
pub const BINANCE_POSITIONS_PATH: &str = "/fapi/v2/positionRisk";
pub const BINANCE_OPEN_ORDERS_PATH: &str = "/fapi/v1/openOrders";
/// Binance: leverage brackets of one symbol (signed GET, `symbol` parameter).
pub const BINANCE_LEVERAGE_BRACKET_PATH: &str = "/fapi/v1/leverageBracket";

pub const BYBIT_BALANCE_PATH: &str = "/v5/account/wallet-balance";
pub const BYBIT_POSITIONS_PATH: &str = "/v5/position/list";
pub const BYBIT_OPEN_ORDERS_PATH: &str = "/v5/order/realtime";
/// Bybit v5: instrument catalog entry of one linear symbol (`leverageFilter.maxLeverage`).
pub const BYBIT_INSTRUMENTS_PATH: &str = "/v5/market/instruments-info";

/// Page size requested from Bybit (documented maxima: position list 200, realtime orders 50;
/// UNVERIFIED against a real account, design Open Question #4).
pub const BYBIT_POSITIONS_PAGE_LIMIT: u32 = 200;
pub const BYBIT_ORDERS_PAGE_LIMIT: u32 = 50;
/// Page cap per list (design D6; UNVERIFIED). Hitting it yields `Incomplete`.
pub const MAX_PAGES: usize = 20;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_urls_are_https_and_match_their_host_constants() {
        for (url, host) in [
            (BinanceHost::Testnet.base_url(), BinanceHost::Testnet.host()),
            (BinanceHost::Demo.base_url(), BinanceHost::Demo.host()),
            (BybitHost::Demo.base_url(), BybitHost::Demo.host()),
        ] {
            assert_eq!(url, format!("https://{host}"));
            assert!(ALLOWED_SIGNED_HOSTS.contains(&host));
        }
    }
}
