//! live-guardrails: the preflight that must be green before `Command::Arm` is accepted.
//!
//! The engine runs the checks itself and issues the `preflight_id` (random value + time + hash of
//! the result, kept in memory, valid for 60 s). What can be asked of the existing ports is asked
//! of them (`AccountView`, `ServerOffsets`); the rest (keys present, position mode, database
//! writable, free disk space) goes through the injectable [`PreflightProbes`] trait whose real
//! implementation the composition root provides. Every read that fails or cannot be judged is a
//! FAIL (fail closed). A report keeps only pass / fail and a FIXED reason text per item: never an
//! exchange response, never a key fragment.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};
use tong_funding_core::risk::RiskConfig;
use tong_funding_core::types::{Decimal, Exchange};

use super::live_guard::{self, LIVE_EXCHANGES, MIN_FREE_DISK_BYTES, PREFLIGHT_VALID_MS};
use super::ports::{AccountView, BoxFut, ServerOffsets};

pub const CHECK_KEYS: &str = "keys_present";
pub const CHECK_ACCOUNT_READ: &str = "account_readable";
pub const CHECK_CLOCK: &str = "clock_synced";
pub const CHECK_ONE_WAY: &str = "one_way_mode";
pub const CHECK_NO_EXPOSURE: &str = "no_existing_exposure";
pub const CHECK_MARGIN: &str = "margin_sufficient";
pub const CHECK_SETTINGS: &str = "settings_complete";
pub const CHECK_DB: &str = "db_writable";
pub const CHECK_DISK: &str = "disk_space";

/// What the engine cannot ask its existing ports. Real implementation: composition root. Every
/// `Err` is a failed check.
pub trait PreflightProbes: Send + Sync {
    /// A live key exists for `exchange` (presence only; the secret is never read here).
    fn key_present(&self, exchange: Exchange) -> BoxFut<'_, Result<bool, String>>;
    /// `exchange` is in one-way (not hedge) position mode.
    fn one_way_mode(&self, exchange: Exchange) -> BoxFut<'_, Result<bool, String>>;
    /// The database accepts a write right now.
    fn db_writable(&self) -> BoxFut<'_, Result<(), String>>;
    /// Free bytes on the filesystem that holds the database.
    fn free_disk_bytes(&self) -> BoxFut<'_, Result<u64, String>>;
}

/// The probes of an engine started without any: every check fails, so a Live engine without a real
/// implementation can never be armed.
pub struct NoProbes;

impl PreflightProbes for NoProbes {
    fn key_present(&self, _: Exchange) -> BoxFut<'_, Result<bool, String>> {
        Box::pin(std::future::ready(Err("no preflight probes configured".into())))
    }
    fn one_way_mode(&self, _: Exchange) -> BoxFut<'_, Result<bool, String>> {
        Box::pin(std::future::ready(Err("no preflight probes configured".into())))
    }
    fn db_writable(&self) -> BoxFut<'_, Result<(), String>> {
        Box::pin(std::future::ready(Err("no preflight probes configured".into())))
    }
    fn free_disk_bytes(&self) -> BoxFut<'_, Result<u64, String>> {
        Box::pin(std::future::ready(Err("no preflight probes configured".into())))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    Pass,
    /// Fixed reason text (never an exchange response).
    Fail(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightItem {
    pub name: &'static str,
    pub exchange: Option<Exchange>,
    pub outcome: CheckOutcome,
}

/// One preflight run. `id` exists only when every item passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightReport {
    pub id: Option<String>,
    pub issued_at_ms: i64,
    pub items: Vec<PreflightItem>,
    /// Hash of the caps the run saw (`live_guard::caps_hash`); `Arm` must present the same one.
    pub caps_hash: Option<String>,
}

impl PreflightReport {
    pub fn passed(&self) -> bool {
        !self.items.is_empty() && self.items.iter().all(|i| i.outcome == CheckOutcome::Pass)
    }

    /// Items that failed, as "name[exchange]: reason" for messages and events.
    pub fn failures(&self) -> Vec<String> {
        self.items
            .iter()
            .filter_map(|i| match &i.outcome {
                CheckOutcome::Pass => None,
                CheckOutcome::Fail(why) => Some(match i.exchange {
                    Some(e) => format!("{}[{}]: {why}", i.name, e.name()),
                    None => format!("{}: {why}", i.name),
                }),
            })
            .collect()
    }

    /// The report as stored in the event log: pass / fail and fixed reasons only.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "passed": self.passed(),
            "issued_at_ms": self.issued_at_ms,
            "items": self.items.iter().map(|i| serde_json::json!({
                "check": i.name,
                "exchange": i.exchange.map(|e| e.name()),
                "pass": i.outcome == CheckOutcome::Pass,
                "reason": match &i.outcome { CheckOutcome::Pass => None, CheckOutcome::Fail(w) => Some(w.clone()) },
            })).collect::<Vec<_>>(),
        })
    }

    /// Whether `id` is this report's id and the report is still fresh at `now_ms`.
    pub fn accepts(&self, id: &str, now_ms: i64) -> Result<(), String> {
        let Some(own) = &self.id else { return Err("the latest preflight did not pass".into()) };
        if own != id {
            return Err("preflight_id is not the one the engine issued".into());
        }
        let age = now_ms.saturating_sub(self.issued_at_ms);
        if !(0..=PREFLIGHT_VALID_MS).contains(&age) {
            return Err(format!("preflight is {} s old (valid for {} s): run it again", age / 1000, PREFLIGHT_VALID_MS / 1000));
        }
        Ok(())
    }
}

fn fail(name: &'static str, exchange: Option<Exchange>, why: &str) -> PreflightItem {
    PreflightItem { name, exchange, outcome: CheckOutcome::Fail(why.to_string()) }
}

fn pass(name: &'static str, exchange: Option<Exchange>) -> PreflightItem {
    PreflightItem { name, exchange, outcome: CheckOutcome::Pass }
}

fn judged(name: &'static str, exchange: Option<Exchange>, ok: bool, why: &str) -> PreflightItem {
    if ok { pass(name, exchange) } else { fail(name, exchange, why) }
}

/// Fixed text for an unreadable account: the category only, never the error text itself.
fn read_failure(error: &str) -> &'static str {
    match live_guard::classify_error(error) {
        Some(live_guard::LockCause::Auth) => "read failed (authentication error)",
        Some(live_guard::LockCause::RateLimited) => "read failed (rate limited)",
        None => "read failed",
    }
}

/// What the account of one exchange said.
struct AccountReading {
    open_positions: Result<bool, &'static str>,
    open_orders: Result<bool, &'static str>,
    margin: Result<Decimal, &'static str>,
}

async fn read_account(account: &dyn AccountView, exchange: Exchange) -> AccountReading {
    let (positions, orders, margin) =
        tokio::join!(account.positions(exchange), account.open_orders(exchange), account.available_margin(exchange));
    AccountReading {
        open_positions: match positions {
            Ok(l) if l.complete => Ok(l.items.iter().any(|p| !p.quantity.is_zero())),
            Ok(_) => Err("position list incomplete"),
            Err(e) => Err(read_failure(&e)),
        },
        open_orders: match orders {
            Ok(l) if l.complete => Ok(!l.items.is_empty()),
            Ok(_) => Err("open order list incomplete"),
            Err(e) => Err(read_failure(&e)),
        },
        margin: margin.map_err(|e| read_failure(&e)),
    }
}

/// Margin one exchange needs for the caps: `live_max_leg_notional x max_concurrent_pairs / max_leverage`
/// (one leg of each concurrent pair sits on each exchange).
pub fn margin_required(risk: &RiskConfig) -> Option<Decimal> {
    let cap = risk.live_max_leg_notional_usdt?;
    Some(cap * Decimal::from(risk.max_concurrent_pairs) / risk.max_leverage)
}

/// Runs every check. `risk` is the stored settings (`Err` = unreadable). The id is issued only when
/// everything passed.
pub async fn run_preflight(
    probes: &dyn PreflightProbes,
    account: &dyn AccountView,
    offsets: &dyn ServerOffsets,
    risk: Result<RiskConfig, String>,
    now_ms: i64,
) -> PreflightReport {
    let mut items = Vec::new();

    // settings: both live caps (and the base fields) set, inside the ceilings (loading validates them)
    let risk = match risk {
        Ok(r) => {
            let missing = r.missing_live_fields();
            if missing.is_empty() {
                items.push(pass(CHECK_SETTINGS, None));
            } else {
                items.push(fail(CHECK_SETTINGS, None, &format!("required settings not set: {}", missing.join(", "))));
            }
            Some(r)
        }
        Err(_) => {
            items.push(fail(CHECK_SETTINGS, None, "settings unreadable or invalid (a cap above its built-in ceiling is invalid)"));
            None
        }
    };
    let caps_hash = risk.as_ref().and_then(live_guard::caps_hash);
    let required = risk.as_ref().and_then(margin_required);

    for exchange in LIVE_EXCHANGES {
        let ex = Some(exchange);
        items.push(match probes.key_present(exchange).await {
            Ok(true) => pass(CHECK_KEYS, ex),
            Ok(false) => fail(CHECK_KEYS, ex, "no live key stored"),
            Err(_) => fail(CHECK_KEYS, ex, "could not check the key"),
        });
        items.push(judged(CHECK_CLOCK, ex, offsets.offset_ms(exchange).is_some(), "clock not synchronised"));
        items.push(match probes.one_way_mode(exchange).await {
            Ok(true) => pass(CHECK_ONE_WAY, ex),
            Ok(false) => fail(CHECK_ONE_WAY, ex, "not in one-way position mode"),
            Err(_) => fail(CHECK_ONE_WAY, ex, "could not read the position mode"),
        });
        let reading = read_account(account, exchange).await;
        items.push(match (&reading.open_positions, &reading.open_orders, &reading.margin) {
            (Ok(_), Ok(_), Ok(_)) => pass(CHECK_ACCOUNT_READ, ex),
            (Err(why), _, _) | (_, Err(why), _) | (_, _, Err(why)) => fail(CHECK_ACCOUNT_READ, ex, why),
        });
        items.push(match (&reading.open_positions, &reading.open_orders) {
            (Ok(false), Ok(false)) => pass(CHECK_NO_EXPOSURE, ex),
            (Ok(true), _) => fail(CHECK_NO_EXPOSURE, ex, "the account has an open position"),
            (_, Ok(true)) => fail(CHECK_NO_EXPOSURE, ex, "the account has an open order"),
            (Err(_), _) | (_, Err(_)) => fail(CHECK_NO_EXPOSURE, ex, "could not be judged (account unreadable)"),
        });
        items.push(match (&reading.margin, required) {
            (Ok(available), Some(need)) if *available >= need => pass(CHECK_MARGIN, ex),
            (Ok(_), Some(_)) => fail(CHECK_MARGIN, ex, "available margin is below what the caps need"),
            (Err(_), _) | (_, None) => fail(CHECK_MARGIN, ex, "could not be judged (margin or caps unknown)"),
        });
    }

    items.push(match probes.db_writable().await {
        Ok(()) => pass(CHECK_DB, None),
        Err(_) => fail(CHECK_DB, None, "database is not writable"),
    });
    items.push(match probes.free_disk_bytes().await {
        Ok(free) if free >= MIN_FREE_DISK_BYTES => pass(CHECK_DISK, None),
        Ok(_) => fail(CHECK_DISK, None, "less than 100 MiB of free disk space"),
        Err(_) => fail(CHECK_DISK, None, "could not read the free disk space"),
    });

    let mut report = PreflightReport { id: None, issued_at_ms: now_ms, items, caps_hash };
    if report.passed() && report.caps_hash.is_some() {
        report.id = Some(issue_id(now_ms, &report));
    }
    report
}

static ISSUED: AtomicU64 = AtomicU64::new(0);

/// Random value (per-process random hasher keys), the time and a hash of the result, joined.
fn issue_id(now_ms: i64, report: &PreflightReport) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(ISSUED.fetch_add(1, Ordering::SeqCst));
    hasher.write_i64(now_ms);
    let random = hasher.finish();
    let mut digest = Sha256::new();
    digest.update(random.to_be_bytes());
    digest.update(now_ms.to_be_bytes());
    digest.update(report.to_json().to_string().as_bytes());
    digest.update(report.caps_hash.clone().unwrap_or_default().as_bytes());
    let hash = hex::encode(digest.finalize());
    format!("pf-{now_ms:x}-{random:016x}-{}", &hash[..16])
}

#[cfg(test)]
pub(crate) mod fakes {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Scripted probes; defaults are all green.
    #[derive(Default)]
    pub struct FakeProbes {
        pub key: Mutex<HashMap<Exchange, Result<bool, String>>>,
        pub one_way: Mutex<HashMap<Exchange, Result<bool, String>>>,
        pub db: Mutex<Option<Result<(), String>>>,
        pub disk: Mutex<Option<Result<u64, String>>>,
    }

    impl FakeProbes {
        pub fn green() -> Arc<FakeProbes> {
            Arc::new(FakeProbes::default())
        }
    }

    impl PreflightProbes for FakeProbes {
        fn key_present(&self, e: Exchange) -> BoxFut<'_, Result<bool, String>> {
            let r = self.key.lock().unwrap().get(&e).cloned().unwrap_or(Ok(true));
            Box::pin(std::future::ready(r))
        }
        fn one_way_mode(&self, e: Exchange) -> BoxFut<'_, Result<bool, String>> {
            let r = self.one_way.lock().unwrap().get(&e).cloned().unwrap_or(Ok(true));
            Box::pin(std::future::ready(r))
        }
        fn db_writable(&self) -> BoxFut<'_, Result<(), String>> {
            let r = self.db.lock().unwrap().clone().unwrap_or(Ok(()));
            Box::pin(std::future::ready(r))
        }
        fn free_disk_bytes(&self) -> BoxFut<'_, Result<u64, String>> {
            let r = self.disk.lock().unwrap().clone().unwrap_or(Ok(10 * MIN_FREE_DISK_BYTES));
            Box::pin(std::future::ready(r))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fakes::FakeProbes;
    use super::*;
    use crate::engine::ports::{AccountOrder, AccountPosition, Listed};
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn complete_risk() -> RiskConfig {
        RiskConfig {
            net_edge_threshold_pct: Some(d("0.05")),
            est_slippage_pct: Some(d("0.01")),
            taker_fee_pct: Exchange::ALL.into_iter().map(|e| (e, d("0.02"))).collect(),
            live_max_leg_notional_usdt: Some(d("10")),
            live_max_pairs_per_arming: Some(2),
            max_concurrent_pairs: 1,
            ..RiskConfig::default()
        }
    }

    #[derive(Default)]
    struct Account {
        positions: Mutex<HashMap<Exchange, Result<Listed<AccountPosition>, String>>>,
        orders: Mutex<HashMap<Exchange, Result<Listed<AccountOrder>, String>>>,
        margin: Mutex<HashMap<Exchange, Result<Decimal, String>>>,
    }
    impl AccountView for Account {
        fn positions(&self, e: Exchange) -> BoxFut<'_, Result<Listed<AccountPosition>, String>> {
            let r = self.positions.lock().unwrap().get(&e).cloned().unwrap_or(Ok(Listed { items: vec![], complete: true }));
            Box::pin(std::future::ready(r))
        }
        fn open_orders(&self, e: Exchange) -> BoxFut<'_, Result<Listed<AccountOrder>, String>> {
            let r = self.orders.lock().unwrap().get(&e).cloned().unwrap_or(Ok(Listed { items: vec![], complete: true }));
            Box::pin(std::future::ready(r))
        }
        fn available_margin(&self, e: Exchange) -> BoxFut<'_, Result<Decimal, String>> {
            let r = self.margin.lock().unwrap().get(&e).cloned().unwrap_or(Ok(d("30")));
            Box::pin(std::future::ready(r))
        }
    }

    struct Offsets(Mutex<HashMap<Exchange, Option<i64>>>);
    impl Offsets {
        fn synced() -> Offsets {
            Offsets(Mutex::new(Exchange::ALL.into_iter().map(|e| (e, Some(0))).collect()))
        }
    }
    impl ServerOffsets for Offsets {
        fn offset_ms(&self, e: Exchange) -> Option<i64> {
            self.0.lock().unwrap().get(&e).copied().flatten()
        }
    }

    async fn run(probes: &FakeProbes, account: &Account, offsets: &Offsets, risk: Result<RiskConfig, String>) -> PreflightReport {
        run_preflight(probes, account, offsets, risk, 1_000).await
    }

    fn failed_names(r: &PreflightReport) -> Vec<String> {
        r.items.iter().filter(|i| i.outcome != CheckOutcome::Pass).map(|i| format!("{}/{:?}", i.name, i.exchange)).collect()
    }

    #[tokio::test]
    async fn everything_green_issues_an_id_bound_to_the_caps() {
        let r = run(&FakeProbes::default(), &Account::default(), &Offsets::synced(), Ok(complete_risk())).await;
        assert!(r.passed(), "{:?}", r.failures());
        let id = r.id.clone().expect("an id for a green run");
        assert!(id.starts_with("pf-"), "{id}");
        assert_eq!(r.caps_hash, live_guard::caps_hash(&complete_risk()));
        // 9 per-exchange-pair checks x ... : both exchanges, every check name present
        for name in [CHECK_KEYS, CHECK_ACCOUNT_READ, CHECK_CLOCK, CHECK_ONE_WAY, CHECK_NO_EXPOSURE, CHECK_MARGIN, CHECK_SETTINGS, CHECK_DB, CHECK_DISK] {
            assert!(r.items.iter().any(|i| i.name == name), "{name}");
        }
        assert_eq!(r.items.iter().filter(|i| i.name == CHECK_KEYS).count(), 2);
        // two runs never share an id
        let again = run(&FakeProbes::default(), &Account::default(), &Offsets::synced(), Ok(complete_risk())).await;
        assert_ne!(again.id, r.id);
    }

    #[tokio::test]
    async fn each_failed_item_alone_blocks_the_id_and_names_the_check() {
        type Tweak = Box<dyn Fn(&FakeProbes, &Account, &Offsets) -> Result<RiskConfig, String>>;
        let cases: Vec<(&str, Tweak)> = vec![
            ("keys_present", Box::new(|p, _, _| { p.key.lock().unwrap().insert(Exchange::Bybit, Ok(false)); Ok(complete_risk()) })),
            ("keys_present", Box::new(|p, _, _| { p.key.lock().unwrap().insert(Exchange::Binance, Err("keychain locked".into())); Ok(complete_risk()) })),
            ("clock_synced", Box::new(|_, _, o| { o.0.lock().unwrap().insert(Exchange::Binance, None); Ok(complete_risk()) })),
            ("one_way_mode", Box::new(|p, _, _| { p.one_way.lock().unwrap().insert(Exchange::Bybit, Ok(false)); Ok(complete_risk()) })),
            ("one_way_mode", Box::new(|p, _, _| { p.one_way.lock().unwrap().insert(Exchange::Bybit, Err("timeout".into())); Ok(complete_risk()) })),
            ("account_readable", Box::new(|_, a, _| { a.positions.lock().unwrap().insert(Exchange::Bybit, Err("timeout".into())); Ok(complete_risk()) })),
            ("account_readable", Box::new(|_, a, _| { a.orders.lock().unwrap().insert(Exchange::Binance, Ok(Listed { items: vec![], complete: false })); Ok(complete_risk()) })),
            ("account_readable", Box::new(|_, a, _| { a.margin.lock().unwrap().insert(Exchange::Binance, Err("denied".into())); Ok(complete_risk()) })),
            ("no_existing_exposure", Box::new(|_, a, _| {
                a.positions.lock().unwrap().insert(Exchange::Bybit, Ok(Listed { items: vec![AccountPosition { exchange: Exchange::Bybit, symbol: "BTCUSDT".into(), quantity: d("0.001") }], complete: true }));
                Ok(complete_risk())
            })),
            ("no_existing_exposure", Box::new(|_, a, _| {
                a.orders.lock().unwrap().insert(Exchange::Binance, Ok(Listed { items: vec![AccountOrder { exchange: Exchange::Binance, symbol: "BTCUSDT".into(), client_order_id: None, remaining_quantity: d("1") }], complete: true }));
                Ok(complete_risk())
            })),
            ("margin_sufficient", Box::new(|_, a, _| { a.margin.lock().unwrap().insert(Exchange::Bybit, Ok(d("1.99"))); Ok(complete_risk()) })),
            ("settings_complete", Box::new(|_, _, _| Ok(RiskConfig { live_max_leg_notional_usdt: None, ..complete_risk() }))),
            ("settings_complete", Box::new(|_, _, _| Ok(RiskConfig { live_max_pairs_per_arming: None, ..complete_risk() }))),
            ("settings_complete", Box::new(|_, _, _| Err("risk: invalid value for `live_max_leg_notional_usdt`".into()))),
            ("db_writable", Box::new(|p, _, _| { *p.db.lock().unwrap() = Some(Err("read-only".into())); Ok(complete_risk()) })),
            ("disk_space", Box::new(|p, _, _| { *p.disk.lock().unwrap() = Some(Ok(MIN_FREE_DISK_BYTES - 1)); Ok(complete_risk()) })),
            ("disk_space", Box::new(|p, _, _| { *p.disk.lock().unwrap() = Some(Err("statvfs failed".into())); Ok(complete_risk()) })),
        ];
        for (n, (expected, tweak)) in cases.iter().enumerate() {
            let (p, a, o) = (FakeProbes::default(), Account::default(), Offsets::synced());
            let risk = tweak(&p, &a, &o);
            let r = run(&p, &a, &o, risk).await;
            assert!(!r.passed() && r.id.is_none(), "case {n} ({expected}) must not issue an id");
            let names = failed_names(&r);
            assert!(names.iter().any(|f| f.starts_with(expected)), "case {n}: expected {expected} in {names:?}");
            // every failed item is accounted for by the tweak: nothing else broke
            let collateral: &[&str] = match *expected {
                "account_readable" => &["no_existing_exposure", "margin_sufficient"],
                "settings_complete" => &["margin_sufficient"],
                _ => &[],
            };
            assert!(
                names.iter().all(|f| f.starts_with(expected) || collateral.iter().any(|c| f.starts_with(c))),
                "case {n}: only {expected} (and {collateral:?}) may fail, got {names:?}"
            );
        }
    }

    #[tokio::test]
    async fn margin_boundary_is_cap_times_concurrent_pairs_over_leverage() {
        let risk = RiskConfig { live_max_leg_notional_usdt: Some(d("10")), max_concurrent_pairs: 1, max_leverage: d("5"), ..complete_risk() };
        assert_eq!(margin_required(&risk), Some(d("2")));
        let a = Account::default();
        a.margin.lock().unwrap().insert(Exchange::Binance, Ok(d("2")));
        a.margin.lock().unwrap().insert(Exchange::Bybit, Ok(d("2")));
        assert!(run(&FakeProbes::default(), &a, &Offsets::synced(), Ok(risk.clone())).await.passed(), "equal passes");
        a.margin.lock().unwrap().insert(Exchange::Bybit, Ok(d("1.9999")));
        assert!(!run(&FakeProbes::default(), &a, &Offsets::synced(), Ok(risk)).await.passed());
    }

    #[tokio::test]
    async fn the_report_never_carries_raw_responses_or_secrets() {
        let secret = "RAW-BODY-{\"apiKey\":\"AKIA-SECRET\",\"sign\":\"deadbeef\"}";
        let (p, a, o) = (FakeProbes::default(), Account::default(), Offsets::synced());
        p.key.lock().unwrap().insert(Exchange::Binance, Err(secret.into()));
        p.one_way.lock().unwrap().insert(Exchange::Bybit, Err(secret.into()));
        *p.db.lock().unwrap() = Some(Err(secret.into()));
        *p.disk.lock().unwrap() = Some(Err(secret.into()));
        for e in LIVE_EXCHANGES {
            a.positions.lock().unwrap().insert(e, Err(format!("HTTP status 401 {secret}")));
            a.orders.lock().unwrap().insert(e, Err(secret.into()));
            a.margin.lock().unwrap().insert(e, Err(secret.into()));
        }
        let r = run(&p, &a, &o, Err(format!("risk: {secret}"))).await;
        let shown = format!("{r:?} {} {:?}", r.to_json(), r.failures());
        for forbidden in ["RAW-BODY", "AKIA", "SECRET", "deadbeef", "apiKey", "sign"] {
            assert!(!shown.contains(forbidden), "{forbidden} leaked into {shown}");
        }
        // the category survives (auth error is useful to the user and is a fixed text)
        assert!(shown.contains("authentication error"), "{shown}");
    }

    #[test]
    fn an_id_is_accepted_only_when_it_is_the_issued_one_and_fresh() {
        let mut r = PreflightReport {
            id: Some("pf-1".into()),
            issued_at_ms: 10_000,
            items: vec![pass(CHECK_DB, None)],
            caps_hash: Some("h".into()),
        };
        assert_eq!(r.accepts("pf-1", 10_000), Ok(()));
        assert_eq!(r.accepts("pf-1", 70_000), Ok(()), "60 s exactly is still valid");
        assert!(r.accepts("pf-1", 70_001).unwrap_err().contains("old"));
        assert!(r.accepts("pf-1", 9_999).is_err(), "a clock that went back is not fresh");
        assert!(r.accepts("pf-2", 10_000).unwrap_err().contains("not the one"));
        assert!(r.accepts("", 10_000).is_err());
        r.id = None;
        assert!(r.accepts("pf-1", 10_000).unwrap_err().contains("did not pass"));
    }

    #[tokio::test]
    async fn a_run_without_probes_fails_closed() {
        let r = run_preflight(&NoProbes, &Account::default(), &Offsets::synced(), Ok(complete_risk()), 1).await;
        assert!(!r.passed() && r.id.is_none());
        let names = failed_names(&r);
        for n in ["keys_present", "one_way_mode", "db_writable", "disk_space"] {
            assert!(names.iter().any(|f| f.starts_with(n)), "{n} in {names:?}");
        }
    }
}
