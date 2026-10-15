//! spread-position-tag (record only): one `SPREAD_TAG` event when the pair enters RECONCILED (prices of
//! the T-10 entry check) and one right before its closes are sent (the close reference prices).
//! Child of `flow_tests` (reuses its rig on the paused tokio clock).

use super::*;

fn set_price(rig: &Rig, exchange: Exchange, price: &str) {
    rig.market.prices.lock().unwrap().insert(exchange, dec(price));
}

fn tags(db: &Db) -> Vec<Value> {
    events(db).into_iter().filter(|(_, l, _)| l == SPREAD_TAG).map(|(_, _, p)| p).collect()
}

#[tokio::test(start_paused = true)]
async fn an_entry_and_a_close_each_write_one_spread_tag_from_the_prices_of_that_moment() {
    let (rig, _h) = started(Opts::default()).await;
    // Long = Binance, short = Bybit. At the entry check the long leg is cheaper: positive.
    set_price(&rig, Exchange::Binance, "100");
    set_price(&rig, Exchange::Bybit, "100.5");
    run_until(&rig.clock, T - 4_000).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED");
    let t = tags(&rig.db);
    assert_eq!(t.len(), 1, "entry only so far: {t:?}");
    assert_eq!(t[0]["phase"], json!("ENTRY"), "{}", t[0]);
    assert_eq!(t[0]["tag"], json!("positive"), "{}", t[0]);
    assert_eq!(t[0]["pct"], json!("0.5"), "{}", t[0]);
    assert_eq!(t[0]["long"]["exchange"], json!("Binance"), "{}", t[0]);
    assert_eq!(t[0]["long"]["price"], json!("100"), "{}", t[0]);
    assert_eq!(t[0]["short"]["exchange"], json!("Bybit"), "{}", t[0]);
    assert_eq!(t[0]["short"]["price"], json!("100.5"), "{}", t[0]);
    assert!(t[0]["long"]["observed_at_ms"].is_i64() && t[0]["short"]["observed_at_ms"].is_i64(), "{}", t[0]);

    // Before the scheduled close the prices crossed: the long leg is now the dearer one: negative.
    set_price(&rig, Exchange::Binance, "101");
    run_until(&rig.clock, T + 16_000).await;
    let t = tags(&rig.db);
    assert_eq!(t.len(), 2, "{t:?}");
    assert_eq!(t[1]["phase"], json!("CLOSE"), "{}", t[1]);
    assert_eq!(t[1]["tag"], json!("negative"), "{}", t[1]);
    assert!(t[1]["pct"].as_str().unwrap().starts_with("-0.4950"), "{}", t[1]);
    assert_eq!(t[1]["long"]["price"], json!("101"), "{}", t[1]);
    assert_eq!(t[1]["short"]["price"], json!("100.5"), "{}", t[1]);
    let close_ts = ts_of(&rig.db, SPREAD_TAG);
    assert_eq!(close_ts.len(), 2);
    assert!(close_ts[1] >= T + 15_000, "written when the close starts");
    assert_eq!(status(&rig.db, UUID), "FINALIZED");
}

#[tokio::test(start_paused = true)]
async fn a_close_price_that_could_not_be_fetched_still_leaves_an_unknown_record_with_the_reason() {
    let (rig, _h) = started(Opts::default()).await;
    run_until(&rig.clock, T - 4_000).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED");
    *rig.market.fail.lock().unwrap() = Some("boom".into());
    run_until(&rig.clock, T + 16_000).await;
    let t = tags(&rig.db);
    let close = t.iter().find(|p| p["phase"] == json!("CLOSE")).expect("a CLOSE record even without prices");
    assert_eq!(close["tag"], json!("unknown"), "{close}");
    assert!(close["long"]["error"].as_str().unwrap().contains("boom"), "{close}");
    assert!(close["pct"].is_null(), "{close}");
}
