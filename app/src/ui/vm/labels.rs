//! Plain-language names for internal codes (change ui-plain-language). ONE table: pair states, the
//! execution world, the trigger mode, PnL statuses, known exchange errors and the pre-trade block
//! reasons, each as a Chinese name. The pages draw the Chinese name as the main text and keep the
//! internal code as secondary text; a code that is not in the table is returned as it is (never
//! guessed).
//!
//! `pair_state_plain` and `execution_mode_label` are exhaustive `match`es without a wildcard arm,
//! so a new `PairState`, identity or mode fails to compile here until it has a name. The
//! environment words are never written as literals in this file (the rule of
//! `env_label::tests::environment_words_live_only_here`): the code part comes from `EnvLabel`.
//! Pure: no clock, no GPUI.

use std::str::FromStr;

use tong_funding_core::pair::PairState;
use tong_funding_core::pnl::PnlStatus;
use tong_funding_core::pretrade::Check;
use tong_funding_core::risk::{ExecutionMode, TriggerMode};
use tong_funding_core::types::Exchange;

use super::env_label::EnvLabel;
use crate::exchange::error::AdapterError;
use crate::exchange::signed::okx::ACCOUNT_MODE_CODE;
use crate::identity::ProcessIdentity;
use crate::ui::theme::Tone;

/// `已完成（FINALIZED）`: the plain name first, the internal code as the small print after it.
/// A name that is the code itself (an unknown code) is not printed twice.
pub fn with_code(name: &str, code: &str) -> String {
    if code.is_empty() || name == code { name.to_string() } else { format!("{name}（{code}）") }
}

// ---- pair states --------------------------------------------------------------------------------

/// The Chinese name of a pair state. Exhaustive: a new state must get its name here.
pub const fn pair_state_plain(state: PairState) -> &'static str {
    match state {
        PairState::Prepared => "已暫存",
        PairState::PreTradeCheck => "送單前檢查中",
        PairState::Blocked => "被擋下",
        PairState::OrderSubmit => "送單中",
        PairState::FillMonitor => "等待成交",
        PairState::Reconciled => "持倉中",
        PairState::Imbalanced => "兩腿不平衡",
        PairState::Closing => "平倉中",
        PairState::Finalized => "已完成",
        PairState::Cancelled => "已取消",
        PairState::PartialFailure => "單腿失敗",
        PairState::Unresolved => "狀態不明",
    }
}

/// The Chinese name of a stored or event state code (`FINALIZED`). An unknown code comes back as it is.
pub fn pair_state_label(code: &str) -> String {
    PairState::from_str(code).map_or_else(|_| code.to_string(), |state| pair_state_plain(state).to_string())
}

/// `單腿失敗（PARTIAL_FAILURE）`; an unknown code as it is.
pub fn pair_state_text(code: &str) -> String {
    with_code(&pair_state_label(code), code)
}

// ---- execution world and trigger mode -----------------------------------------------------------

/// The Chinese name of the world an order goes to: simulation, the demo account or real money.
/// Exhaustive over `(identity, mode)` like `EnvLabel`.
pub const fn execution_mode_label(identity: ProcessIdentity, mode: ExecutionMode) -> &'static str {
    match (identity, mode) {
        (ProcessIdentity::Demo, ExecutionMode::Simulation) | (ProcessIdentity::Live, ExecutionMode::Simulation) => "模擬",
        (ProcessIdentity::Demo, ExecutionMode::ExchangeDemo) => "Demo 帳戶",
        (ProcessIdentity::Live, ExecutionMode::ExchangeDemo) => "真錢",
    }
}

/// `Demo 帳戶（EXCHANGE_DEMO）`: the plain name, then the code `EnvLabel` names this world with.
pub fn execution_mode_text(identity: ProcessIdentity, mode: ExecutionMode) -> String {
    with_code(execution_mode_label(identity, mode), EnvLabel::of(identity, mode).env_name())
}

/// The Chinese name of the trigger mode.
pub const fn trigger_mode_label(mode: TriggerMode) -> &'static str {
    match mode {
        TriggerMode::Auto => "自動",
        TriggerMode::Manual => "手動",
    }
}

/// `自動（AUTO）`.
pub fn trigger_mode_text(mode: TriggerMode) -> String {
    with_code(trigger_mode_label(mode), crate::engine::gate::trigger_mode_str(mode))
}

// ---- PnL status ---------------------------------------------------------------------------------

/// The Chinese name of a PnL status code; an unknown code as it is.
pub fn pnl_status_label(code: &str) -> String {
    if code == PnlStatus::Complete.as_str() {
        "完整".into()
    } else if code == PnlStatus::Incomplete(Vec::new()).as_str() {
        "不完整".into()
    } else {
        code.to_string()
    }
}

/// `不完整（INCOMPLETE）`; an unknown code as it is.
pub fn pnl_status_text(code: &str) -> String {
    with_code(&pnl_status_label(code), code)
}

// ---- known exchange errors ----------------------------------------------------------------------

/// Exchange error codes the table knows: `(code, 中文名稱, 建議動作)`.
const KNOWN_ERRORS: [(&str, &str, &str); 1] = [(ACCOUNT_MODE_CODE, "OKX 帳戶模式不支援", "在 OKX 改為單幣種或跨幣種保證金模式")];

/// `(中文名稱, 建議動作)` when the message contains a known exchange error code; `None` otherwise.
pub fn exchange_error_label(raw: &str) -> Option<(&'static str, &'static str)> {
    KNOWN_ERRORS.iter().find(|(code, ..)| raw.contains(code)).map(|(_, name, advice)| (*name, *advice))
}

/// The message in plain words with the suggested fix (`OKX 帳戶模式不支援（在 OKX 改為…）`), or the
/// message as it is when its code is not in the table.
pub fn plain_error(raw: &str) -> String {
    match exchange_error_label(raw) {
        Some((name, advice)) => format!("{name}（{advice}）"),
        None => raw.to_string(),
    }
}

// ---- pre-trade block reasons --------------------------------------------------------------------

/// The ten core checks in plain words. Exhaustive: a new check must get its name here.
const fn check_plain(check: Check) -> &'static str {
    match check {
        Check::DataFresh => "資料不夠新",
        Check::CoinListed => "幣種未上架",
        Check::ExchangeAllowed => "交易所不在允許清單",
        Check::NetEdgeQualified => "Net Edge 未達標",
        Check::PriceDrift => "價格漂移過大",
        Check::Liquidity => "流動性不足",
        Check::Margin => "可用保證金不足",
        Check::Leverage => "槓桿超過上限",
        Check::ExistingExposure => "標的已有其他持倉或委託",
        Check::RiskLimits => "超過風控上限",
    }
}

const CHECKS: [Check; 10] = [
    Check::DataFresh,
    Check::CoinListed,
    Check::ExchangeAllowed,
    Check::NetEdgeQualified,
    Check::PriceDrift,
    Check::Liquidity,
    Check::Margin,
    Check::Leverage,
    Check::ExistingExposure,
    Check::RiskLimits,
];

/// The engine's own gate (symbol-leverage-cap) names itself in `failed_checks` like a core check.
const LEVERAGE_CAP_CHECK: &str = "LeverageCap";

/// `價格漂移過大（PriceDrift）` for a failed check named as the engine writes it (the `Debug` name
/// of the core check); an unknown name as it is.
pub fn check_label(name: &str) -> String {
    if let Some(check) = CHECKS.iter().find(|c| format!("{c:?}") == name) {
        return with_code(check_plain(*check), name);
    }
    if name == LEVERAGE_CAP_CHECK {
        return with_code("槓桿超過交易所上限", name);
    }
    name.to_string()
}

/// Every failed check by plain name, in the order given: `價格漂移過大（PriceDrift）、可用保證金不足（Margin）`.
pub fn failed_checks_text(checks: &[String]) -> String {
    checks.iter().map(|c| check_label(c)).collect::<Vec<_>>().join("、")
}

/// The text between double quotes, each quoted piece in order (`Debug` output of strings).
fn quoted(raw: &str) -> Vec<&str> {
    raw.split('"').skip(1).step_by(2).collect()
}

/// `reason: "…"` of a `Debug`-printed struct variant, with its escaped quotes restored.
fn debug_reason(raw: &str) -> Option<String> {
    let (first, last) = (raw.find('"')?, raw.rfind('"')?);
    (first < last).then(|| raw[first + 1..last].replace("\\\"", "\""))
}

fn fetch_failed_text(why: &str) -> String {
    let timeout = AdapterError::Timeout.to_string();
    if !why.is_empty() && why.split(';').all(|part| part.trim() == timeout) {
        "送單前重抓價格逾時".to_string()
    } else {
        format!("送單前重抓價格失敗：{why}")
    }
}

/// A pre-trade block reason as the engine writes it (`detail.block` of the transition to BLOCKED,
/// or the `reason` of the pre-trade timeout) in plain words. Every text the engine writes
/// without `failed_checks` is in here (actor `run_node0`, `abort_stuck_pretrade`, restart
/// reconciliation); `Checks`, `RiskLimits` and `LeverageCap` always carry `failed_checks`, which the
/// caller names with `failed_checks_text` instead. A text that is not in the table is returned as it is.
pub fn block_reason_label(raw: &str) -> String {
    let raw = raw.trim();
    if let Some(why) = raw.strip_prefix("pre-trade fetch failed:") {
        return fetch_failed_text(why.trim());
    }
    if let Some(why) = raw.strip_prefix("settings unreadable:") {
        return format!("風控設定讀取失敗：{}", why.trim());
    }
    if let Some(why) = raw.strip_prefix("InvalidEntry:") {
        return format!("進場快照無法使用：{}", why.trim());
    }
    // `Debug` of `Node0Block`, when the structured `failed_checks` list is not at hand.
    if raw.starts_with("ConfigIncomplete {") {
        let names = quoted(raw);
        return if names.is_empty() { "設定不完整".to_string() } else { format!("設定不完整：{}", names.join("、")) };
    }
    if raw.starts_with("InvalidEntry {")
        && let Some(reason) = debug_reason(raw)
    {
        return format!("進場快照無法使用：{reason}");
    }
    if raw.starts_with("DataMismatch {")
        && let Some(reason) = debug_reason(raw)
    {
        return format!("報價資料與配對不符：{reason}");
    }
    match raw {
        "restart during the pre-trade check; no order was sent" => "重啟時正在做送單前檢查，沒有送出任何訂單".to_string(),
        "pre-trade check timed out" => "送單前檢查逾時，沒有送出任何訂單".to_string(),
        other => other.to_string(),
    }
}

// ---- disabled exchanges -------------------------------------------------------------------------

/// Added to the card, the positions and the margin row of an exchange the risk settings left unticked
/// while it still holds positions, has a position list that is incomplete, or is on one leg of an
/// unfinished pair: its exposure is shown (or its state is not confirmed), never hidden.
pub const DISABLED_WITH_EXPOSURE: &str = "此交易所已停用，但仍有持倉或持倉狀態未確認";

/// `OKX 已停用（風控設定未勾選）`: the one line shown for an exchange that is not in `allowed_exchanges`.
pub fn disabled_exchange_line(ex: Exchange) -> String {
    format!("{} 已停用（風控設定未勾選）", ex.name())
}

/// `OKX 已停用`: the short line in the scanner's source status bar.
pub fn disabled_source_text(ex: Exchange) -> String {
    format!("{} 已停用", ex.name())
}

/// The one line of a disabled exchange on a page, and what that page would have shown for it
/// (the error, the state, the quote row), which only the expanded line draws. Muted when the latest
/// read succeeded and found nothing; warning-toned when the positions cannot be confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisabledNote {
    pub exchange: Exchange,
    pub text: String,
    pub detail: Option<String>,
    pub tone: Tone,
}

impl DisabledNote {
    /// `OKX 已停用（風控設定未勾選）`, muted.
    pub fn new(exchange: Exchange, detail: Option<String>) -> DisabledNote {
        DisabledNote { exchange, text: disabled_exchange_line(exchange), detail, tone: Tone::Muted }
    }

    /// `OKX 已停用 · 無法確認持倉（<reason>）` in the warning tone: the read failed, so "no positions"
    /// must not be said.
    pub fn unconfirmed(exchange: Exchange, reason: &str, detail: Option<String>) -> DisabledNote {
        DisabledNote { exchange, text: format!("{} 已停用 · 無法確認持倉（{reason}）", exchange.name()), detail, tone: Tone::Warning }
    }
}

#[cfg(test)]
#[path = "labels_tests.rs"]
mod tests;
