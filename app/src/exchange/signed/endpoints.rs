//! Compile-time endpoint constants of the signed clients. Only demo/testnet hosts exist here:
//! there is no constructor taking a base URL and no host is read from the environment or a file.
//! The host is chosen by the enums below, which is the only knob a caller has.

use crate::exchange::transport::{DemoSignedTransport, HttpTransport, LiveSignedTransport};

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
/// OKX REST host (documentation 2026-10). Production and Demo Trading share it: demo is selected
/// only by the `x-simulated-trading: 1` header below, never by the host name.
pub const OKX_DEMO_HOST: &str = "openapi.okx.com";

/// Every host name a signed request may ever be sent to.
pub const ALLOWED_SIGNED_HOSTS: [&str; 4] = [BINANCE_TESTNET_HOST, BINANCE_DEMO_HOST, BYBIT_DEMO_HOST, OKX_DEMO_HOST];

/// Live (production) hosts that may receive signed requests: Binance and Bybit only (OKX has no
/// live path). Kept apart from [`ALLOWED_SIGNED_HOSTS`]; the Demo policy must never list these.
pub const BINANCE_LIVE_HOST: &str = "fapi.binance.com";
pub const BYBIT_LIVE_HOST: &str = "api.bybit.com";
pub const LIVE_SIGNED_HOSTS: [&str; 2] = [BINANCE_LIVE_HOST, BYBIT_LIVE_HOST];

/// A live signed-request host. Deliberately NOT a variant of [`BinanceHost`] / [`BybitHost`]: no
/// demo code path (and no environment variable) can name it by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveHost {
    Binance,
    Bybit,
}

impl LiveHost {
    pub const ALL: [LiveHost; 2] = [LiveHost::Binance, LiveHost::Bybit];
    pub fn host(self) -> &'static str {
        match self {
            LiveHost::Binance => BINANCE_LIVE_HOST,
            LiveHost::Bybit => BYBIT_LIVE_HOST,
        }
    }
    pub fn base_url(self) -> &'static str {
        match self {
            LiveHost::Binance => "https://fapi.binance.com",
            LiveHost::Bybit => "https://api.bybit.com",
        }
    }
}

/// Header names that only a signed request carries (case-insensitive): Binance `X-MBX-APIKEY`,
/// the whole Bybit `X-BAPI-*` family (key, signature, timestamp, recv window) and OKX `OK-ACCESS-*`.
pub fn is_exchange_auth_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "x-mbx-apikey" || n.starts_with("x-bapi-") || n.starts_with(OKX_AUTH_HEADER_PREFIX)
}

/// Query parameter names that carry a request signature (Binance `signature`, `sign`), any casing.
pub fn is_signature_param(name: &str) -> bool {
    name.eq_ignore_ascii_case("signature") || name.eq_ignore_ascii_case("sign")
}

/// The only places the OKX demo flag and the OKX auth header family are spelled out.
const SIMULATED_TRADING_HEADER: &str = "x-simulated-trading";
const SIMULATED_TRADING_VALUE: &str = "1";
const OKX_AUTH_HEADER_PREFIX: &str = "ok-access-";

/// OKX hosts that may receive signed requests. There is deliberately no `host()` / `base_url()`:
/// the only way to a URL is [`OkxHost::target`], which hands out the simulated-trading header
/// together with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OkxHost {
    Demo,
}

/// An OKX base URL that cannot be separated from its `x-simulated-trading: 1` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OkxTarget {
    base: &'static str,
    sim: (&'static str, &'static str),
}

impl OkxHost {
    pub fn target(self) -> OkxTarget {
        match self {
            OkxHost::Demo => OkxTarget { base: "https://openapi.okx.com", sim: (SIMULATED_TRADING_HEADER, SIMULATED_TRADING_VALUE) },
        }
    }
}

impl OkxTarget {
    /// `path_and_query` starts with `/`; the result is only ever sent together with [`Self::sim_header`].
    pub fn url(&self, path_and_query: &str) -> String {
        format!("{}{}", self.base, path_and_query)
    }
    pub fn sim_header(&self) -> (&'static str, &'static str) {
        self.sim
    }
}

/// True for the names no generic header API may set: the simulated-trading flag and the
/// `OK-ACCESS-*` family (only the OKX request constructors set these).
pub fn is_protected_okx_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == SIMULATED_TRADING_HEADER || n.starts_with(OKX_AUTH_HEADER_PREFIX)
}

/// One `OK-ACCESS-*` header name (case-insensitive).
pub fn is_okx_auth_header(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with(OKX_AUTH_HEADER_PREFIX)
}

/// Any `OK-ACCESS-*` header present (case-insensitive).
pub fn carries_okx_auth(headers: &[(String, String)]) -> bool {
    headers.iter().any(|(n, _)| is_okx_auth_header(n))
}

/// Exactly one `x-simulated-trading` header (any casing) and its value is exactly `1`.
/// Zero, duplicates (even with the same value) and any other value are all invalid.
pub fn okx_headers_valid(headers: &[(String, String)]) -> bool {
    let mut sim = headers.iter().filter(|(n, _)| n.eq_ignore_ascii_case(SIMULATED_TRADING_HEADER));
    matches!((sim.next(), sim.next()), (Some((_, v)), None) if v == SIMULATED_TRADING_VALUE)
}

/// `okx.com` or any subdomain of it (host names are already lower-case in a parsed URL). The
/// domain is derived from [`OKX_DEMO_HOST`] so that host literals stay in one place.
pub fn is_okx_host(candidate: &str) -> bool {
    let domain = OKX_DEMO_HOST.split_once('.').map_or(OKX_DEMO_HOST, |(_, d)| d);
    candidate == domain || candidate.strip_suffix(domain).is_some_and(|p| p.ends_with('.'))
}

const USDT_SWAP_SUFFIX: &str = "-USDT-SWAP";

/// `BTCUSDT` to `BTC-USDT-SWAP`; anything that is not a `BASEUSDT` symbol gives `None`.
pub fn okx_inst_id(symbol: &str) -> Option<String> {
    let base = symbol.strip_suffix("USDT").filter(|b| !b.is_empty())?;
    Some(format!("{base}{USDT_SWAP_SUFFIX}"))
}

/// `BTC-USDT-SWAP` to `BTCUSDT`; anything that is not a `-USDT-SWAP` instrument gives `None`.
pub fn okx_symbol(inst_id: &str) -> Option<String> {
    let base = inst_id.strip_suffix(USDT_SWAP_SUFFIX).filter(|b| !b.is_empty())?;
    Some(format!("{}USDT", base.replace('-', "")))
}

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

/// Which world a host type belongs to; it decides the `client_order_id` prefix an executor accepts
/// (`demo` for the demo world, `live` for the live world). A live executor cannot be assembled from
/// a demo client and a live client: the executor bounds both clients to the same realm.
pub trait Realm: Copy + Send + Sync + 'static {
    const ID_PREFIX: crate::engine::ids::IdPrefix;
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemoRealm;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveRealm;
impl Realm for DemoRealm {
    const ID_PREFIX: crate::engine::ids::IdPrefix = crate::engine::ids::IdPrefix::Demo;
}
impl Realm for LiveRealm {
    const ID_PREFIX: crate::engine::ids::IdPrefix = crate::engine::ids::IdPrefix::Live;
}

/// The host a Binance signed client talks to: a demo/testnet [`BinanceHost`] or the live
/// [`BinanceLive`]. Closed set (the only implementors are those two types, in this file).
pub trait BinanceEnv: Copy + Send + Sync + 'static {
    type Realm: Realm;
    fn host(self) -> &'static str;
    fn base_url(self) -> &'static str;
}

/// The host a Bybit signed client talks to: [`BybitHost`] (demo) or [`BybitLive`].
pub trait BybitEnv: Copy + Send + Sync + 'static {
    type Realm: Realm;
    fn host(self) -> &'static str;
    fn base_url(self) -> &'static str;
}

/// Binance live host (`fapi.binance.com`). A type of its own, deliberately not a [`BinanceHost`]
/// variant; it pairs only with a live signed transport (see [`SignedTransportFor`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BinanceLive;

/// Bybit live host (`api.bybit.com`); pairs only with a live signed transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BybitLive;

impl BinanceEnv for BinanceHost {
    type Realm = DemoRealm;
    fn host(self) -> &'static str {
        BinanceHost::host(self)
    }
    fn base_url(self) -> &'static str {
        BinanceHost::base_url(self)
    }
}
impl BybitEnv for BybitHost {
    type Realm = DemoRealm;
    fn host(self) -> &'static str {
        BybitHost::host(self)
    }
    fn base_url(self) -> &'static str {
        BybitHost::base_url(self)
    }
}
impl BinanceEnv for BinanceLive {
    type Realm = LiveRealm;
    fn host(self) -> &'static str {
        LiveHost::Binance.host()
    }
    fn base_url(self) -> &'static str {
        LiveHost::Binance.base_url()
    }
}
impl BybitEnv for BybitLive {
    type Realm = LiveRealm;
    fn host(self) -> &'static str {
        LiveHost::Bybit.host()
    }
    fn base_url(self) -> &'static str {
        LiveHost::Bybit.base_url()
    }
}

/// "This transport may carry signed requests to host type `E`": demo/testnet host types accept only
/// a `DemoSignedTransport`, live host types only a `LiveSignedTransport`. The signed client
/// constructors bound their transport by this, so the pairing is checked by the compiler.
pub trait SignedTransportFor<E>: HttpTransport {}
impl<T: DemoSignedTransport> SignedTransportFor<BinanceHost> for T {}
impl<T: DemoSignedTransport> SignedTransportFor<BybitHost> for T {}
impl<T: LiveSignedTransport> SignedTransportFor<BinanceLive> for T {}
impl<T: LiveSignedTransport> SignedTransportFor<BybitLive> for T {}

pub const BINANCE_BALANCE_PATH: &str = "/fapi/v2/balance";
pub const BINANCE_POSITIONS_PATH: &str = "/fapi/v2/positionRisk";
pub const BINANCE_OPEN_ORDERS_PATH: &str = "/fapi/v1/openOrders";
/// Binance: leverage brackets of one symbol (signed GET, `symbol` parameter).
/// Binance: public funding-rate history of one symbol.
pub const BINANCE_FUNDING_RATE_PATH: &str = "/fapi/v1/fundingRate";
pub const BINANCE_LEVERAGE_BRACKET_PATH: &str = "/fapi/v1/leverageBracket";

pub const BYBIT_BALANCE_PATH: &str = "/v5/account/wallet-balance";
pub const BYBIT_POSITIONS_PATH: &str = "/v5/position/list";
pub const BYBIT_OPEN_ORDERS_PATH: &str = "/v5/order/realtime";
/// Bybit v5: instrument catalog entry of one linear symbol (`leverageFilter.maxLeverage`).
/// Bybit v5: public funding-rate history of one linear symbol.
pub const BYBIT_FUNDING_HISTORY_PATH: &str = "/v5/market/funding/history";
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

    fn hdrs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(n, v)| (n.to_string(), v.to_string())).collect()
    }

    // ---- OKX demo boundary (spec: OKX 簽名請求只能以模擬交易身分送出) ----

    #[test]
    fn the_okx_target_is_the_signed_host_and_always_comes_with_the_simulated_trading_header() {
        let t = OkxHost::Demo.target();
        assert_eq!(t.url("/api/v5/account/balance"), "https://openapi.okx.com/api/v5/account/balance");
        assert_eq!(t.sim_header(), ("x-simulated-trading", "1"));
        assert_eq!(OKX_DEMO_HOST, "openapi.okx.com");
        assert!(ALLOWED_SIGNED_HOSTS.contains(&OKX_DEMO_HOST));
    }

    #[test]
    fn a_path_cannot_smuggle_another_host_into_an_okx_url() {
        let url = OkxHost::Demo.target().url("@evil.example/x");
        let host = url.strip_prefix("https://").unwrap().split(['/', '@']).next().unwrap().to_string();
        // `https://openapi.okx.com@evil.example/x` has userinfo: the transport policy refuses it
        assert_eq!(host, "openapi.okx.com");
        assert!(url.contains('@'), "the crafted path is still visible to the policy: {url}");
    }

    #[test]
    fn exactly_one_simulated_header_with_value_one_is_valid() {
        assert!(okx_headers_valid(&hdrs(&[("x-simulated-trading", "1")])));
        assert!(okx_headers_valid(&hdrs(&[("OK-ACCESS-KEY", "k"), ("X-Simulated-Trading", "1")])), "name is case-insensitive");
        assert!(!okx_headers_valid(&[]), "zero");
        assert!(!okx_headers_valid(&hdrs(&[("x-simulated-trading", "0")])), "value 0 is production");
        assert!(!okx_headers_valid(&hdrs(&[("x-simulated-trading", "")])));
        assert!(!okx_headers_valid(&hdrs(&[("x-simulated-trading", " 1")])), "exact match only");
        assert!(!okx_headers_valid(&hdrs(&[("x-simulated-trading", "1"), ("x-simulated-trading", "1")])), "duplicates");
        assert!(!okx_headers_valid(&hdrs(&[("x-simulated-trading", "1"), ("X-SIMULATED-TRADING", "0")])), "mixed-case duplicate");
        assert!(!okx_headers_valid(&hdrs(&[("x-simulated-trading", "0"), ("x-simulated-trading", "1")])), "duplicate 0 first");
    }

    #[test]
    fn protected_header_names_are_the_simulated_flag_and_every_ok_access_header() {
        for n in ["x-simulated-trading", "X-Simulated-Trading", "OK-ACCESS-KEY", "ok-access-sign", "Ok-Access-Passphrase", "OK-ACCESS-FUTURE"] {
            assert!(is_protected_okx_header(n), "{n}");
        }
        for n in ["X-MBX-APIKEY", "Content-Type", "x-simulated", "ok-access"] {
            assert!(!is_protected_okx_header(n), "{n}");
        }
        assert!(is_okx_auth_header("OK-ACCESS-KEY") && !is_okx_auth_header("x-simulated-trading"));
        assert!(carries_okx_auth(&hdrs(&[("ok-access-key", "k")])));
        assert!(!carries_okx_auth(&hdrs(&[("x-simulated-trading", "1")])));
    }

    #[test]
    fn okx_hosts_are_recognised_by_suffix_only_at_a_label_boundary() {
        for h in ["openapi.okx.com", "www.okx.com", "eea.okx.com", "okx.com", "a.b.okx.com"] {
            assert!(is_okx_host(h), "{h}");
        }
        for h in ["notokx.com", "okx.com.evil.example", "api-demo.bybit.com", "127.0.0.1"] {
            assert!(!is_okx_host(h), "{h}");
        }
    }

    // ---- one instrument-id pair for the whole crate ----

    #[test]
    fn instrument_ids_and_system_symbols_round_trip() {
        assert_eq!(okx_inst_id("BTCUSDT").as_deref(), Some("BTC-USDT-SWAP"));
        assert_eq!(okx_inst_id("1000PEPEUSDT").as_deref(), Some("1000PEPE-USDT-SWAP"));
        assert_eq!(okx_symbol("BTC-USDT-SWAP").as_deref(), Some("BTCUSDT"));
        assert_eq!(okx_symbol("1000PEPE-USDT-SWAP").as_deref(), Some("1000PEPEUSDT"));
        for bad in ["BTC-USD-SWAP", "BTC-USDT", "BTC-USDT-240329", "-USDT-SWAP", ""] {
            assert_eq!(okx_symbol(bad), None, "{bad}");
        }
        for bad in ["USDT", "BTCUSD", ""] {
            assert_eq!(okx_inst_id(bad), None, "{bad}");
        }
    }

    #[test]
    fn base_urls_are_https_and_match_their_host_constants() {
        let okx_url = OkxHost::Demo.target().url("");
        for (url, host) in [
            (BinanceHost::Testnet.base_url(), BinanceHost::Testnet.host()),
            (BinanceHost::Demo.base_url(), BinanceHost::Demo.host()),
            (BybitHost::Demo.base_url(), BybitHost::Demo.host()),
            (okx_url.as_str(), OKX_DEMO_HOST),
        ] {
            assert_eq!(url, format!("https://{host}"));
            assert!(ALLOWED_SIGNED_HOSTS.contains(&host));
        }
    }

    // ---- live hosts (change live-process 2.1): one source, never mixed with the demo lists ----

    #[test]
    fn live_hosts_list_constants_and_base_urls_come_from_one_source() {
        assert_eq!(LIVE_SIGNED_HOSTS, [BINANCE_LIVE_HOST, BYBIT_LIVE_HOST]);
        assert_eq!(LiveHost::ALL.map(LiveHost::host), LIVE_SIGNED_HOSTS);
        assert_eq!(BINANCE_LIVE_HOST, "fapi.binance.com");
        assert_eq!(BYBIT_LIVE_HOST, "api.bybit.com");
        for h in LiveHost::ALL {
            assert_eq!(h.base_url(), format!("https://{}", h.host()));
        }
        // the public module spells the same two production hosts; they must agree
        use crate::exchange::public::endpoints::{BINANCE_HOST, BYBIT_HOST};
        assert_eq!(BINANCE_HOST, LiveHost::Binance.base_url());
        assert_eq!(BYBIT_HOST, LiveHost::Bybit.base_url());
    }

    #[test]
    fn live_hosts_are_disjoint_from_every_demo_host_and_list() {
        for h in LIVE_SIGNED_HOSTS {
            assert!(!ALLOWED_SIGNED_HOSTS.contains(&h), "{h} leaked into the demo allow-list");
            assert!(![BinanceHost::Testnet.host(), BinanceHost::Demo.host(), BybitHost::Demo.host()].contains(&h));
        }
        for h in ALLOWED_SIGNED_HOSTS {
            assert!(!LIVE_SIGNED_HOSTS.contains(&h), "{h} leaked into the live list");
        }
    }

    // ---- authentication spellings (change live-process 2.2) ----

    #[test]
    fn exchange_auth_headers_cover_all_three_exchanges_case_insensitively() {
        for n in ["X-MBX-APIKEY", "x-mbx-apikey", "X-BAPI-API-KEY", "X-BAPI-SIGN", "x-bapi-timestamp", "X-BAPI-RECV-WINDOW", "OK-ACCESS-KEY", "ok-access-sign"] {
            assert!(is_exchange_auth_header(n), "{n}");
        }
        for n in ["Content-Type", "Accept", "x-mbx-used-weight-1m", "x-bapi", "User-Agent"] {
            assert!(!is_exchange_auth_header(n), "{n}");
        }
        assert!(is_signature_param("signature") && is_signature_param("SIGN") && is_signature_param("Sign"));
        assert!(!is_signature_param("symbol") && !is_signature_param("signed") && !is_signature_param("sig"));
    }
}
