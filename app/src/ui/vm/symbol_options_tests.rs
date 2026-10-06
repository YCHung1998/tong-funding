use super::*;
use crate::engine::ports::{AccountOrder, Listed};
use crate::ui::bridge::{LegAccount, MarketFeed};
use crate::ui::testkit::{d, obs};

const NOW: i64 = 1_800_000_000_000;

fn feed(ex: Exchange, symbols: &[&str]) -> MarketFeed {
    MarketFeed { observations: symbols.iter().map(|s| obs(ex, s, "0.0001", 28_800, NOW + 3_600_000, NOW - 100)).collect(), last_success_at: Some(NOW), last_error: None }
}

fn snap() -> UiSnapshot {
    let mut s = UiSnapshot::default();
    s.market.insert(Exchange::Binance, feed(Exchange::Binance, &["ETHUSDT", "1000PEPEUSDT", "BTCUSDT"]));
    s.market.insert(Exchange::Bybit, feed(Exchange::Bybit, &["SOLUSDT", "BTCUSDT"]));
    s
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn symbols_are_per_exchange_sorted() {
    let s = snap();
    assert_eq!(symbol_options(&s, Exchange::Binance), strs(&["1000PEPEUSDT", "BTCUSDT", "ETHUSDT"]));
    assert_eq!(symbol_options(&s, Exchange::Bybit), strs(&["BTCUSDT", "SOLUSDT"]));
}

#[test]
fn all_symbols_are_the_deduplicated_union() {
    assert_eq!(all_symbol_options(&snap()), strs(&["1000PEPEUSDT", "BTCUSDT", "ETHUSDT", "SOLUSDT"]));
}

#[test]
fn coins_are_deduplicated_base_coins() {
    assert_eq!(coin_options(&snap()), strs(&["1000PEPE", "BTC", "ETH", "SOL"]));
}

#[test]
fn market_not_loaded_gives_empty_lists() {
    let s = UiSnapshot::default();
    assert!(symbol_options(&s, Exchange::Binance).is_empty());
    assert!(all_symbol_options(&s).is_empty());
    assert!(coin_options(&s).is_empty());
    assert!(open_order_symbols(&s, Exchange::Binance).is_empty());
}

#[test]
fn open_order_symbols_come_from_the_exchange_open_orders() {
    let mut s = snap();
    let ord = |sym: &str| AccountOrder { exchange: Exchange::Binance, symbol: sym.into(), client_order_id: None, remaining_quantity: d("1") };
    // no execution engine => simulated ledger account
    s.leg_accounts.insert(
        (true, Exchange::Binance),
        LegAccount { positions: Ok(Listed { items: vec![], complete: true }), open_orders: Ok(Listed { items: vec![ord("ETHUSDT"), ord("BTCUSDT"), ord("ETHUSDT")], complete: true }), available_margin: Ok(d("1")), fetched_at: NOW },
    );
    assert_eq!(open_order_symbols(&s, Exchange::Binance), strs(&["BTCUSDT", "ETHUSDT"]));
    assert!(open_order_symbols(&s, Exchange::Bybit).is_empty());
}

#[test]
fn filter_is_case_insensitive_substring() {
    let o = strs(&["1000PEPEUSDT", "BTCUSDT", "ETHUSDT"]);
    let r = options_with_query("pep", &o);
    assert_eq!(r[1], SymbolOption { value: "1000PEPEUSDT".into(), free: false });
    assert_eq!(r.iter().filter(|x| !x.free).count(), 1);
}

#[test]
fn empty_query_lists_everything_without_a_free_row() {
    let o = strs(&["BTCUSDT", "ETHUSDT"]);
    let r = options_with_query("  ", &o);
    assert_eq!(r.len(), 2);
    assert!(r.iter().all(|x| !x.free));
}

#[test]
fn unknown_query_is_offered_first_uppercased_and_trimmed() {
    let o = strs(&["BTCUSDT"]);
    let r = options_with_query(" newusdt ", &o);
    assert_eq!(r, vec![SymbolOption { value: "NEWUSDT".into(), free: true }]);
    let r = options_with_query("btc", &o);
    assert_eq!(r[0], SymbolOption { value: "BTC".into(), free: true });
    assert_eq!(r[1], SymbolOption { value: "BTCUSDT".into(), free: false });
}

#[test]
fn exact_match_has_no_free_row() {
    let o = strs(&["BTCUSDT", "BTCUSDTX"]);
    let r = options_with_query("btcusdt", &o);
    assert!(r.iter().all(|x| !x.free));
    assert_eq!(r.len(), 2);
}

#[test]
fn is_known_ignores_case_and_whitespace() {
    let o = strs(&["BTCUSDT"]);
    assert!(is_known(" btcusdt ", &o));
    assert!(!is_known("NEWUSDT", &o));
}

#[test]
fn coins_text_round_trips() {
    assert_eq!(parse_coins("btc, ETH ,,eth"), strs(&["BTC", "ETH"]));
    assert_eq!(join_coins(&strs(&["BTC", "ETH"])), "BTC, ETH");
    assert_eq!(parse_coins(""), Vec::<String>::new());
}
