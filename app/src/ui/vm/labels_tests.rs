//! Task 1.1 (spec ui-plain-language, "內部代碼以白話呈現"): one table of plain names.
use std::collections::BTreeSet;

use tong_funding_core::pair::PairState;
use tong_funding_core::risk::{ExecutionMode, TriggerMode};
use tong_funding_core::types::Exchange;

use super::*;
use crate::identity::ProcessIdentity::{Demo, Live};
use crate::ui::env_label::EnvLabel;
use crate::ui::theme::Tone;

// ---- pair states ---------------------------------------------------------------------------

#[test]
fn the_three_states_the_spec_names_read_as_finalized_blocked_and_partial_failure() {
    assert_eq!(pair_state_label("FINALIZED"), "已完成");
    assert_eq!(pair_state_label("BLOCKED"), "被擋下");
    assert_eq!(pair_state_label("PARTIAL_FAILURE"), "單腿失敗");
}

#[test]
fn every_pair_state_has_its_own_plain_name() {
    let mut names = BTreeSet::new();
    for s in PairState::ALL {
        let name = pair_state_label(s.as_str());
        assert!(!name.is_empty() && name != s.as_str(), "{s}: the main text is Chinese, not the code: {name:?}");
        assert!(names.insert(name.clone()), "{s}: {name:?} is used by another state");
    }
    assert_eq!(names.len(), PairState::ALL.len());
}

#[test]
fn the_wording_of_all_twelve_states_is_pinned() {
    let expected = [
        (PairState::Prepared, "已暫存"),
        (PairState::PreTradeCheck, "送單前檢查中"),
        (PairState::Blocked, "被擋下"),
        (PairState::OrderSubmit, "送單中"),
        (PairState::FillMonitor, "等待成交"),
        (PairState::Reconciled, "持倉中"),
        (PairState::Imbalanced, "兩腿不平衡"),
        (PairState::Closing, "平倉中"),
        (PairState::Finalized, "已完成"),
        (PairState::Cancelled, "已取消"),
        (PairState::PartialFailure, "單腿失敗"),
        (PairState::Unresolved, "狀態不明"),
    ];
    assert_eq!(expected.len(), PairState::ALL.len(), "a new state needs a pinned name here too");
    for (state, name) in expected {
        assert_eq!(pair_state_plain(state), name, "{state}");
    }
}

#[test]
fn the_typed_and_the_stored_code_lookups_agree() {
    for s in PairState::ALL {
        assert_eq!(pair_state_label(s.as_str()), pair_state_plain(s), "{s}");
    }
}

#[test]
fn an_unknown_state_code_is_shown_as_it_is_and_never_guessed() {
    assert_eq!(pair_state_label("WAITING_FOR_GODOT"), "WAITING_FOR_GODOT");
    // the stored codes are exact: a different spelling is not "close enough"
    assert_eq!(pair_state_label("finalized"), "finalized");
    assert_eq!(pair_state_label("未知"), "未知");
    assert_eq!(pair_state_label(""), "");
}

#[test]
fn the_code_stays_as_small_print_after_the_plain_name() {
    assert_eq!(with_code("已完成", "FINALIZED"), "已完成（FINALIZED）");
    assert_eq!(with_code("WAITING", "WAITING"), "WAITING", "an unknown code is not printed twice");
    assert_eq!(pair_state_text("PARTIAL_FAILURE"), "單腿失敗（PARTIAL_FAILURE）");
    assert_eq!(pair_state_text("WAITING_FOR_GODOT"), "WAITING_FOR_GODOT");
}

// ---- execution world and trigger mode ----------------------------------------------------------

#[test]
fn execution_worlds_read_as_simulation_demo_account_and_real_money() {
    assert_eq!(execution_mode_label(Demo, ExecutionMode::Simulation), "模擬");
    assert_eq!(execution_mode_label(Demo, ExecutionMode::ExchangeDemo), "Demo 帳戶");
    assert_eq!(execution_mode_label(Live, ExecutionMode::ExchangeDemo), "真錢");
    assert_eq!(execution_mode_label(Live, ExecutionMode::Simulation), "模擬");
}

#[test]
fn the_environment_code_comes_from_env_label_and_follows_the_plain_name() {
    for identity in [Demo, Live] {
        for mode in [ExecutionMode::Simulation, ExecutionMode::ExchangeDemo] {
            let code = EnvLabel::of(identity, mode).env_name();
            let name = execution_mode_label(identity, mode);
            assert_eq!(execution_mode_text(identity, mode), format!("{name}（{code}）"), "{identity:?} {mode:?}");
        }
    }
}

#[test]
fn trigger_modes_read_as_automatic_and_manual() {
    assert_eq!(trigger_mode_label(TriggerMode::Auto), "自動");
    assert_eq!(trigger_mode_label(TriggerMode::Manual), "手動");
    assert_eq!(trigger_mode_text(TriggerMode::Auto), "自動（AUTO）");
    assert_eq!(trigger_mode_text(TriggerMode::Manual), "手動（MANUAL）");
}

// ---- PnL status ---------------------------------------------------------------------------------

#[test]
fn pnl_statuses_read_as_complete_and_incomplete_and_unknown_ones_stay_as_they_are() {
    assert_eq!(pnl_status_label("COMPLETE"), "完整");
    assert_eq!(pnl_status_label("INCOMPLETE"), "不完整");
    assert_eq!(pnl_status_label("PARTIAL"), "PARTIAL");
    assert_eq!(pnl_status_text("INCOMPLETE"), "不完整（INCOMPLETE）");
    assert_eq!(pnl_status_text("PARTIAL"), "PARTIAL");
}

// ---- known exchange errors ----------------------------------------------------------------------

const OKX_SPOT_MODE: &str = "exchange error acct_mode_unsupported: 帳戶模式不支援 (acctLv 1, posMode net_mode)";

#[test]
fn the_okx_account_mode_error_reads_in_plain_words_with_the_fix() {
    let (name, advice) = exchange_error_label(OKX_SPOT_MODE).expect("known code");
    assert_eq!(name, "OKX 帳戶模式不支援");
    assert_eq!(advice, "在 OKX 改為單幣種或跨幣種保證金模式");
    // the same code arrives with the exchange's name in front on the margin row
    assert!(exchange_error_label(&format!("OKX: {OKX_SPOT_MODE}")).is_some());
    assert_eq!(plain_error(OKX_SPOT_MODE), "OKX 帳戶模式不支援（在 OKX 改為單幣種或跨幣種保證金模式）");
}

#[test]
fn an_error_that_is_not_in_the_table_is_shown_as_it_is() {
    for raw in ["request timed out", "exchange error 50101: API key does not match", "HTTP status 500", ""] {
        assert_eq!(exchange_error_label(raw), None, "{raw}");
        assert_eq!(plain_error(raw), raw);
    }
}

// ---- pre-trade block reasons --------------------------------------------------------------------

/// Every `block` string the engine writes (actor `run_node0`, `abort_stuck_pretrade`, recovery).
#[test]
fn every_block_reason_the_engine_writes_has_a_plain_text() {
    let cases = [
        ("pre-trade fetch failed: request timed out", "送單前重抓價格逾時"),
        ("pre-trade fetch failed: request timed out; request timed out", "送單前重抓價格逾時"),
        ("pre-trade fetch failed: network error: dns lookup failed", "送單前重抓價格失敗：network error: dns lookup failed"),
        ("pre-trade fetch failed: request timed out; HTTP status 500", "送單前重抓價格失敗：request timed out; HTTP status 500"),
        ("settings unreadable: database is locked", "風控設定讀取失敗：database is locked"),
        ("InvalidEntry: entry snapshot: missing field `leverage`", "進場快照無法使用：entry snapshot: missing field `leverage`"),
        (r#"InvalidEntry { reason: "leverage must be positive" }"#, "進場快照無法使用：leverage must be positive"),
        (r#"DataMismatch { reason: "quote is not of Bybit" }"#, "報價資料與配對不符：quote is not of Bybit"),
        (r#"ConfigIncomplete { missing: ["est_slippage_pct", "taker_fee_pct.Bybit"] }"#, "設定不完整：est_slippage_pct、taker_fee_pct.Bybit"),
        ("restart during the pre-trade check; no order was sent", "重啟時正在做送單前檢查，沒有送出任何訂單"),
        ("pre-trade check timed out", "送單前檢查逾時，沒有送出任何訂單"),
    ];
    for (raw, plain) in cases {
        assert_eq!(block_reason_label(raw), plain, "{raw}");
    }
}

#[test]
fn a_block_reason_that_is_not_in_the_table_is_shown_as_it_is() {
    assert_eq!(block_reason_label("something the engine has not said before"), "something the engine has not said before");
    assert_eq!(block_reason_label(""), "");
}

#[test]
fn the_ten_pretrade_checks_and_the_leverage_cap_have_plain_names_with_the_code_kept() {
    let all = [
        ("DataFresh", "資料不夠新"),
        ("CoinListed", "幣種未上架"),
        ("ExchangeAllowed", "交易所不在允許清單"),
        ("NetEdgeQualified", "Net Edge 未達標"),
        ("PriceDrift", "價格漂移過大"),
        ("Liquidity", "流動性不足"),
        ("Margin", "可用保證金不足"),
        ("Leverage", "槓桿超過上限"),
        ("ExistingExposure", "標的已有其他持倉或委託"),
        ("RiskLimits", "超過風控上限"),
        ("LeverageCap", "槓桿超過交易所上限"),
    ];
    for (code, plain) in all {
        assert_eq!(check_label(code), format!("{plain}（{code}）"), "{code}");
    }
    assert_eq!(check_label("SomethingNew"), "SomethingNew");
    assert_eq!(failed_checks_text(&["PriceDrift".to_string(), "Margin".to_string()]), "價格漂移過大（PriceDrift）、可用保證金不足（Margin）");
}

// ---- disabled exchanges -------------------------------------------------------------------------

#[test]
fn the_disabled_exchange_line_names_the_exchange_and_the_setting() {
    assert_eq!(disabled_exchange_line(Exchange::Okx), "OKX 已停用（風控設定未勾選）");
    assert_eq!(disabled_exchange_line(Exchange::Bybit), "Bybit 已停用（風控設定未勾選）");
    assert_eq!(DISABLED_WITH_EXPOSURE, "此交易所已停用，但仍有持倉或持倉狀態未確認");
}

#[test]
fn a_disabled_note_carries_the_line_and_keeps_the_detail_for_the_expanded_view() {
    let n = DisabledNote::new(Exchange::Okx, Some("載入失敗：OKX 帳戶模式不支援".into()));
    assert_eq!(n.exchange, Exchange::Okx);
    assert_eq!(n.text, "OKX 已停用（風控設定未勾選）");
    assert_eq!(n.detail.as_deref(), Some("載入失敗：OKX 帳戶模式不支援"));
    assert_eq!(DisabledNote::new(Exchange::Okx, None).detail, None);
    assert_eq!(n.tone, Tone::Muted);
}

#[test]
fn an_unconfirmed_note_names_the_reason_in_the_warning_tone_and_never_says_no_positions() {
    let n = DisabledNote::unconfirmed(Exchange::Okx, "OKX 帳戶模式不支援", Some("原始訊息".into()));
    assert_eq!(n.text, "OKX 已停用 · 無法確認持倉（OKX 帳戶模式不支援）");
    assert_eq!(n.tone, Tone::Warning);
    assert!(!n.text.contains("沒有持倉") && !n.text.contains("風控設定未勾選"), "{}", n.text);
    assert_eq!(n.detail.as_deref(), Some("原始訊息"));
}

