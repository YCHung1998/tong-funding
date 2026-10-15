//! leg-early-exit: a RECONCILED leg is closed alone in T+5..T+15 when its price moved in its favour
//! by `early_exit_min_move_pct` (versus its own open fill average). Child of `flow_tests` (reuses its
//! rig on the paused tokio clock; the simulator fills at 100, the fake market quotes per exchange).

use super::*;

fn early_on(rig: &Rig, min_move_pct: &str) {
    let mut cfg = risk();
    cfg.early_exit_min_move_pct = dec(min_move_pct);
    store_risk(&rig.db, &cfg);
}

fn set_price(rig: &Rig, exchange: Exchange, price: &str) {
    rig.market.prices.lock().unwrap().insert(exchange, dec(price));
}

/// Reduce-only orders that reached the simulator (the pair's closes, early or scheduled).
fn closes_sent(rig: &Rig) -> Vec<OrderRequest> {
    rig.sim.submitted().into_iter().filter(|r| r.reduce_only).collect()
}

fn of(db: &Db, label: &str) -> Vec<Value> {
    events(db).into_iter().filter(|(_, l, _)| l == label).map(|(_, _, p)| p).collect()
}

/// Early exit on at `min_move_pct`, and the pair entered at 100 (prices are scripted only after
/// this, so both legs open at an average of 100) and RECONCILED.
async fn reconciled(min_move_pct: &str) -> (Rig, EngineHandle) {
    let (rig, h) = started(Opts::default()).await;
    early_on(&rig, min_move_pct);
    run_until(&rig.clock, T - 4_000).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED");
    (rig, h)
}

#[tokio::test(start_paused = true)]
async fn a_leg_that_moved_in_favour_by_the_threshold_is_closed_alone_and_the_pair_stays_reconciled() {
    let (rig, _h) = reconciled("0.3").await;
    set_price(&rig, Exchange::Binance, "100.3"); // long: +0.3 % = the threshold (equal passes)
    set_price(&rig, Exchange::Bybit, "100.3"); // short: 0.3 % against it
    run_until(&rig.clock, T + 9_000).await;

    assert_eq!(status(&rig.db, UUID), "RECONCILED", "no new state, not a single-leg failure");
    assert_eq!(count(&rig.db, "CLOSING"), 0);
    let closes = closes_sent(&rig);
    assert_eq!(closes.len(), 1, "only the long leg: {closes:?}");
    assert_eq!(closes[0].client_order_id, sim_id(Leg::Long, OrderAction::Close));
    assert_eq!((closes[0].quantity, closes[0].side), (dec("10"), OrderSide::Sell));
    assert_eq!(rig.sim.position(Exchange::Binance, SYM), Decimal::ZERO);
    assert_eq!(rig.sim.position(Exchange::Bybit, SYM), dec("-10"), "the short leg is untouched");

    let ev: Vec<_> = events(&rig.db).into_iter().filter(|(_, l, _)| l == LEG_EARLY_EXIT).collect();
    assert_eq!(ev.len(), 1, "{ev:?}");
    let (ts, _, p) = &ev[0];
    assert!(*ts >= T + 5_000 && *ts < T + 6_000, "T{:+}", ts - T);
    assert_eq!(p["leg"], json!("long"), "{p}");
    assert_eq!(p["exchange"], json!("Binance"), "{p}");
    assert_eq!(p["open_avg_price"], json!("100"), "{p}");
    assert_eq!(p["trigger_price"], json!("100.3"), "{p}");
    assert!(p["price_observed_at_ms"].is_i64(), "{p}");
    assert_eq!(p["move_pct"], json!("0.3"), "{p}");
    assert_eq!(p["threshold_pct"], json!("0.3"), "{p}");
    assert_eq!(p["t_plus_ms"], json!(*ts - T), "{p}");
    assert_eq!(p["simulated"], json!(true), "{p}");
    assert!(of(&rig.db, LEG_EARLY_EXIT_ABORTED).is_empty());
    // the early close carries its reference price like a scheduled one
    let sub = events(&rig.db).into_iter().find(|(_, l, p)| l == ORDER_SUBMITTED && p["action"] == json!("close")).unwrap().2;
    assert_eq!(sub["reference_price"], json!("100.3"), "{sub}");

    // T+15: only the other leg is sent; the early leg is not closed twice.
    run_until(&rig.clock, T + 16_000).await;
    let closes = closes_sent(&rig);
    let ids: Vec<&str> = closes.iter().map(|r| r.client_order_id.as_str()).collect();
    assert_eq!(ids, vec![sim_id(Leg::Long, OrderAction::Close).as_str(), sim_id(Leg::Short, OrderAction::Close).as_str()]);
    assert_eq!(count(&rig.db, LEG_EARLY_EXIT), 1);
    assert_eq!(ts_of(&rig.db, "CLOSING"), vec![T + 15_000]);
    assert_eq!(status(&rig.db, UUID), "FINALIZED");
    assert_eq!(rig.sim.position(Exchange::Bybit, SYM), Decimal::ZERO);
}

#[tokio::test(start_paused = true)]
async fn a_short_leg_that_fell_enough_is_closed_alone() {
    let (rig, _h) = reconciled("0.05").await;
    set_price(&rig, Exchange::Binance, "99.94"); // long 0.06 % against it
    set_price(&rig, Exchange::Bybit, "99.94"); // short +0.06 %
    run_until(&rig.clock, T + 9_000).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED");
    let closes = closes_sent(&rig);
    assert_eq!(closes.len(), 1, "{closes:?}");
    assert_eq!((closes[0].client_order_id.clone(), closes[0].side), (sim_id(Leg::Short, OrderAction::Close), OrderSide::Buy));
    assert_eq!(rig.sim.position(Exchange::Binance, SYM), dec("10"));
    assert_eq!(rig.sim.position(Exchange::Bybit, SYM), Decimal::ZERO);
    assert_eq!(of(&rig.db, LEG_EARLY_EXIT)[0]["leg"], json!("short"));
}

#[tokio::test(start_paused = true)]
async fn both_legs_closed_early_go_straight_to_the_closed_confirmation_and_t_plus_15_sends_nothing() {
    let (rig, _h) = reconciled("0.05").await;
    set_price(&rig, Exchange::Binance, "100.06"); // long hits at T+5
    set_price(&rig, Exchange::Bybit, "100.06"); // short still against it
    run_until(&rig.clock, T + 7_500).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED");
    assert_eq!(closes_sent(&rig).len(), 1);
    set_price(&rig, Exchange::Bybit, "99.94"); // short hits at the next tick
    run_until(&rig.clock, T + 12_000).await;

    assert_eq!(closes_sent(&rig).len(), 2, "one early close per leg");
    assert_eq!(count(&rig.db, LEG_EARLY_EXIT), 2);
    let closing = ts_of(&rig.db, "CLOSING");
    assert_eq!(closing.len(), 1);
    assert!(closing[0] < T + 15_000, "confirmation starts as soon as both legs are flat, not at T+15: T{:+}", closing[0] - T);
    assert_eq!(count(&rig.db, CLOSE_CONFIRMED), 1);
    assert_eq!(status(&rig.db, UUID), "FINALIZED");
    run_until(&rig.clock, T + 17_000).await;
    assert_eq!(closes_sent(&rig).len(), 2, "T+15 sends no close order");
    assert_eq!(rig.sim.position(Exchange::Binance, SYM), Decimal::ZERO);
    assert_eq!(rig.sim.position(Exchange::Bybit, SYM), Decimal::ZERO);
}

#[tokio::test(start_paused = true)]
async fn a_move_just_below_the_threshold_does_nothing_until_the_scheduled_exit() {
    let (rig, _h) = reconciled("0.3").await;
    set_price(&rig, Exchange::Binance, "100.299");
    set_price(&rig, Exchange::Bybit, "99.701");
    run_until(&rig.clock, T + 14_000).await;
    assert!(closes_sent(&rig).is_empty());
    assert_eq!(count(&rig.db, LEG_EARLY_EXIT), 0);
    run_until(&rig.clock, T + 16_000).await;
    assert_eq!(closes_sent(&rig).len(), 2, "both legs at T+15 as before");
    assert_eq!(ts_of(&rig.db, "CLOSING"), vec![T + 15_000]);
}

#[tokio::test(start_paused = true)]
async fn nothing_is_fetched_or_closed_before_t_plus_5() {
    let (rig, _h) = reconciled("0.3").await;
    set_price(&rig, Exchange::Binance, "101");
    set_price(&rig, Exchange::Bybit, "99");
    run_until(&rig.clock, T + 4_000).await;
    assert!(closes_sent(&rig).is_empty());
    assert!(rig.market.calls().iter().all(|(_, _, t)| *t < T), "no price fetch inside the window yet: {:?}", rig.market.calls());
    run_until(&rig.clock, T + 5_000).await;
    assert_eq!(closes_sent(&rig).len(), 2, "both legs hit exactly at T+5");
}

#[tokio::test(start_paused = true)]
async fn a_zero_threshold_never_fetches_or_closes_early() {
    let (rig, _h) = reconciled("0").await;
    set_price(&rig, Exchange::Binance, "110");
    set_price(&rig, Exchange::Bybit, "90");
    run_until(&rig.clock, T + 14_000).await;
    assert!(closes_sent(&rig).is_empty());
    let times: Vec<i64> = rig.market.calls().iter().map(|(_, _, t)| t - T).collect();
    assert_eq!(times, vec![-15_000, -15_000, -10_000, -10_000], "{times:?}");
}

#[tokio::test(start_paused = true)]
async fn manual_trigger_mode_never_closes_early() {
    let (rig, h) = started(Opts { trigger: "MANUAL", ..Opts::default() }).await;
    early_on(&rig, "0.3");
    run_until(&rig.clock, T - 5_000).await;
    assert_eq!(ask(&h, Command::ManualEnter { pair: UUID.into() }).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 4_000).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED");
    set_price(&rig, Exchange::Binance, "101");
    set_price(&rig, Exchange::Bybit, "99");
    run_until(&rig.clock, T + 14_000).await;
    assert!(closes_sent(&rig).is_empty());
    assert_eq!(count(&rig.db, LEG_EARLY_EXIT), 0);
    run_until(&rig.clock, T + 16_000).await;
    assert_eq!(closes_sent(&rig).len(), 2, "the scheduled exit is unchanged");
}

#[tokio::test(start_paused = true)]
async fn an_active_kill_switch_stops_early_exits_but_not_the_scheduled_exit() {
    let (rig, h) = reconciled("0.3").await;
    run_until(&rig.clock, T - 4_000).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED");
    assert_eq!(ask(&h, Command::SetKillSwitch { on: true }).await, CommandReply::Accepted);
    set_price(&rig, Exchange::Binance, "101");
    set_price(&rig, Exchange::Bybit, "99");
    run_until(&rig.clock, T + 14_000).await;
    assert!(closes_sent(&rig).is_empty());
    assert_eq!(count(&rig.db, LEG_EARLY_EXIT), 0);
    assert!(rig.market.calls().iter().all(|(_, _, t)| *t < T), "not even a price fetch");
    run_until(&rig.clock, T + 16_000).await;
    assert_eq!(closes_sent(&rig).len(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_missing_clock_offset_blocks_early_exit_but_the_exit_still_runs_on_local_time() {
    let (rig, _h) = reconciled("0.3").await;
    run_until(&rig.clock, T - 4_000).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED");
    rig.offsets.0.lock().unwrap().insert(Exchange::Bybit, None);
    set_price(&rig, Exchange::Binance, "101");
    set_price(&rig, Exchange::Bybit, "99");
    run_until(&rig.clock, T + 14_000).await;
    assert!(closes_sent(&rig).is_empty());
    assert!(rig.market.calls().iter().all(|(_, _, t)| *t < T));
    run_until(&rig.clock, T + 16_000).await;
    assert_eq!(closes_sent(&rig).len(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_price_that_is_older_than_the_stale_threshold_is_not_used() {
    let (rig, _h) = reconciled("0.3").await;
    run_until(&rig.clock, T - 4_000).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED");
    // a source that stopped updating: every refetch returns this 10-seconds-old, favourable quote
    for ex in [Exchange::Binance, Exchange::Bybit] {
        let mut q = FakeMarket::quote(ex, SYM, T - 10_000);
        q.price = if ex == Exchange::Binance { dec("101") } else { dec("99") };
        rig.market.first.lock().unwrap().insert(ex, q);
    }
    *rig.market.frozen.lock().unwrap() = true;
    run_until(&rig.clock, T + 14_000).await;
    assert!(closes_sent(&rig).is_empty(), "stale (> 3000 ms) price must not trigger an order");
    assert_eq!(count(&rig.db, LEG_EARLY_EXIT), 0);
}

#[tokio::test(start_paused = true)]
async fn a_failing_price_fetch_means_no_early_exit_and_one_event_per_leg() {
    let (rig, _h) = reconciled("0.3").await;
    run_until(&rig.clock, T - 4_000).await;
    *rig.market.fail.lock().unwrap() = Some("price source down (scripted)".into());
    run_until(&rig.clock, T + 14_000).await;
    assert!(closes_sent(&rig).is_empty());
    let a = of(&rig.db, LEG_EARLY_EXIT_ABORTED);
    assert_eq!(a.len(), 2, "written once per leg, not every second: {a:?}");
    assert!(a.iter().all(|p| p["reason"].as_str().unwrap().contains("price source down")), "{a:?}");
    *rig.market.fail.lock().unwrap() = None;
    run_until(&rig.clock, T + 16_000).await;
    assert_eq!(closes_sent(&rig).len(), 2, "the scheduled exit still closes both");
    assert_eq!(status(&rig.db, UUID), "FINALIZED");
}

#[tokio::test(start_paused = true)]
async fn a_rejected_early_close_leaves_the_leg_open_the_pair_reconciled_and_t_plus_15_closes_it() {
    let (rig, _h) = reconciled("0.3").await;
    rig.sim.script_prefix(&sim_id(Leg::Long, OrderAction::Close), SimBehavior::Reject("reduce-only refused (scripted)".into()));
    set_price(&rig, Exchange::Binance, "100.5");
    run_until(&rig.clock, T + 12_000).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED", "a rejected single leg does not change the pair state");
    assert_eq!(rig.sim.position(Exchange::Binance, SYM), dec("10"), "the leg is still open");
    assert_eq!(count(&rig.db, LEG_EARLY_EXIT), 1);
    let a = of(&rig.db, LEG_EARLY_EXIT_ABORTED);
    assert_eq!(a.len(), 1, "{a:?}");
    assert_eq!(a[0]["leg"], json!("long"));
    assert!(a[0]["reason"].as_str().unwrap().contains("reduce-only refused"), "{a:?}");
    assert_eq!(count(&rig.db, "PARTIAL_FAILURE"), 0);
    run_until(&rig.clock, T + 17_000).await;
    assert_eq!(status(&rig.db, UUID), "FINALIZED");
    assert_eq!(rig.sim.position(Exchange::Binance, SYM), Decimal::ZERO);
    assert_eq!(rig.sim.position(Exchange::Bybit, SYM), Decimal::ZERO);
    assert_eq!(count(&rig.db, LEG_EARLY_EXIT), 1, "the rejected leg is not retried within the window");
}

#[tokio::test(start_paused = true)]
async fn a_position_far_from_the_recorded_fill_blocks_the_early_close_and_sends_nothing() {
    let (rig, _h) = reconciled("0.3").await;
    run_until(&rig.clock, T - 4_000).await;
    user_order(&rig, OrderSide::Buy, "2", false).await; // 12 long vs 10 recorded: 16.7 % > 1 %
    set_price(&rig, Exchange::Binance, "101");
    run_until(&rig.clock, T + 12_000).await;
    assert!(closes_sent(&rig).is_empty(), "fail closed: {:?}", closes_sent(&rig));
    assert_eq!(status(&rig.db, UUID), "RECONCILED");
    assert_eq!(count(&rig.db, LEG_EARLY_EXIT), 0, "the event is written only when an order is sent");
    let a = of(&rig.db, LEG_EARLY_EXIT_ABORTED);
    assert_eq!(a.len(), 1, "{a:?}");
    assert!(a[0]["reason"].as_str().unwrap().contains("differs"), "{a:?}");
    assert_eq!(rig.sim.position(Exchange::Binance, SYM), dec("12"), "nothing touched");
}
