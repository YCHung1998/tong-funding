//! Plain-language names for internal codes (change ui-plain-language). ONE table: pair states, the
//! execution world, the trigger mode, PnL statuses, known exchange errors and the pre-trade block
//! reasons, each as a Chinese name plus one sentence. The pages draw the Chinese name as the main
//! text and keep the internal code as secondary text; a code that is not in the table is returned
//! as it is (never guessed).
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

/// `(中文名稱, 一句說明)` of a pair state. Exhaustive: a new state must get its name here.
pub const fn pair_state_plain(state: PairState) -> (&'static str, &'static str) {
    match state {
        PairState::Prepared => ("已暫存", "已加入交易單，還沒進場"),
        PairState::PreTradeCheck => ("送單前檢查中", "進場前用最新資料重新檢查，還沒送出任何訂單"),
        PairState::Blocked => ("被擋下", "送單前檢查沒有通過，沒有送出任何訂單"),
        PairState::OrderSubmit => ("送單中", "正在送出兩腿的訂單"),
        PairState::FillMonitor => ("等待成交", "兩腿訂單都已送出，等待成交回報"),
        PairState::Reconciled => ("持倉中", "兩腿已成交且數量相符，等待平倉時間"),
        PairState::Imbalanced => ("兩腿不平衡", "兩腿成交數量差距超過容許值，需人工處理"),
        PairState::Closing => ("平倉中", "正在平倉，等待兩腿持倉歸零"),
        PairState::Finalized => ("已完成", "已平倉並完成損益結算"),
        PairState::Cancelled => ("已取消", "沒有留下持倉：使用者移除、兩腿都沒送出，或逾時沒有成交"),
        PairState::PartialFailure => ("單腿失敗", "只有一腿成功，另一腿失敗或平倉未完成，需人工處理"),
        PairState::Unresolved => ("狀態不明", "無法確認訂單結果（逾時或重啟後無法判定），需人工處理"),
    }
}

/// `(中文名稱, 說明)` of a stored or event state code (`FINALIZED`). An unknown code comes back as
/// it is with no explanation.
pub fn pair_state_label(code: &str) -> (String, String) {
    match PairState::from_str(code) {
        Ok(state) => {
            let (name, note) = pair_state_plain(state);
            (name.to_string(), note.to_string())
        }
        Err(_) => (code.to_string(), String::new()),
    }
}

/// `單腿失敗（PARTIAL_FAILURE）`; an unknown code as it is.
pub fn pair_state_text(code: &str) -> String {
    with_code(&pair_state_label(code).0, code)
}

// ---- execution world and trigger mode -----------------------------------------------------------

/// `(中文名稱, 說明)` of the world an order goes to: simulation, the demo account or real money.
/// Exhaustive over `(identity, mode)` like `EnvLabel`.
pub const fn execution_mode_label(identity: ProcessIdentity, mode: ExecutionMode) -> (&'static str, &'static str) {
    match (identity, mode) {
        (ProcessIdentity::Demo, ExecutionMode::Simulation) => ("模擬", "跑完整流程與送單前檢查，訂單由模擬器成交，不送到任何交易所；不涉及真錢"),
        (ProcessIdentity::Demo, ExecutionMode::ExchangeDemo) => ("Demo 帳戶", "對 demo / testnet 帳戶真實下單，會改變帳戶內的倉位；不涉及真錢"),
        (ProcessIdentity::Live, ExecutionMode::ExchangeDemo) => ("真錢", "對正式環境真實下單，會動用真錢"),
        (ProcessIdentity::Live, ExecutionMode::Simulation) => ("模擬", "正式程式的模擬模式：跑完整流程，訂單由模擬器成交，不送出任何真實訂單"),
    }
}

/// `Demo 帳戶（EXCHANGE_DEMO）`: the plain name, then the code `EnvLabel` names this world with.
pub fn execution_mode_text(identity: ProcessIdentity, mode: ExecutionMode) -> String {
    with_code(execution_mode_label(identity, mode).0, EnvLabel::of(identity, mode).env_name())
}

/// `(中文名稱, 說明)` of the trigger mode. What each mode does follows the engine's scheduler: only
/// AUTO enters by itself, the scheduled close runs in both.
pub const fn trigger_mode_label(mode: TriggerMode) -> (&'static str, &'static str) {
    match mode {
        TriggerMode::Auto => ("自動", "達標的配對到進場時間會自動進場；到平倉時間自動平倉"),
        TriggerMode::Manual => ("手動", "進場要在交易單按「一鍵送出」；到平倉時間仍會自動平倉，也可按「立即平倉」"),
    }
}

const fn trigger_mode_code(mode: TriggerMode) -> &'static str {
    match mode {
        TriggerMode::Auto => "AUTO",
        TriggerMode::Manual => "MANUAL",
    }
}

/// `自動（AUTO）`.
pub fn trigger_mode_text(mode: TriggerMode) -> String {
    with_code(trigger_mode_label(mode).0, trigger_mode_code(mode))
}

// ---- PnL status ---------------------------------------------------------------------------------

/// `(中文名稱, 說明)` of a PnL status code; an unknown code as it is.
pub fn pnl_status_label(code: &str) -> (String, String) {
    if code == PnlStatus::Complete.as_str() {
        ("完整".into(), "已取得所有預期的資金費流水與成交明細，數字為最終值".into())
    } else if code == PnlStatus::Incomplete(Vec::new()).as_str() {
        ("不完整".into(), "缺少部分流水或明細，數字只含已知的部分，原因見清單".into())
    } else {
        (code.to_string(), String::new())
    }
}

/// `不完整（INCOMPLETE）`; an unknown code as it is.
pub fn pnl_status_text(code: &str) -> String {
    with_code(&pnl_status_label(code).0, code)
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

/// The names inside `failed: [A, B]` of a `Debug`-printed `Node0Block::Checks`.
fn failed_in_debug(raw: &str) -> Vec<String> {
    let Some(start) = raw.find("failed: [") else { return Vec::new() };
    let rest = &raw[start + "failed: [".len()..];
    let Some(end) = rest.find(']') else { return Vec::new() };
    rest[..end].split(',').map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).collect()
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
/// or the `reason` of the pre-trade timeout) in plain words. Every text the engine writes is in
/// here (actor `run_node0`, `abort_stuck_pretrade`, restart reconciliation); a text that is not
/// in the table is returned as it is.
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
    if let Some(why) = raw.strip_prefix("RiskLimits: live:") {
        return format!("超過正式環境的風控上限：{}", why.trim());
    }
    if let Some(why) = raw.strip_prefix("LeverageCap:") {
        return format!("槓桿超過交易所上限：{}", why.trim());
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
    if raw.starts_with("Checks {") {
        let failed = failed_in_debug(raw);
        if !failed.is_empty() {
            return format!("送單前檢查未通過：{}", failed_checks_text(&failed));
        }
    }
    match raw {
        "restart during the pre-trade check; no order was sent" => "重啟時正在做送單前檢查，沒有送出任何訂單".to_string(),
        "pre-trade check timed out" => "送單前檢查逾時，沒有送出任何訂單".to_string(),
        other => other.to_string(),
    }
}

// ---- disabled exchanges -------------------------------------------------------------------------

/// The tone of every disabled-exchange line: muted, never the red or yellow of an error.
pub const DISABLED_TONE: Tone = Tone::Muted;

/// Added to the positions of an exchange the risk settings left unticked: its exposure is shown, not hidden.
pub const DISABLED_WITH_POSITIONS: &str = "此交易所已停用，仍有持倉";

/// `OKX 已停用（風控設定未勾選）`: the one line shown for an exchange that is not in `allowed_exchanges`.
pub fn disabled_exchange_line(ex: Exchange) -> String {
    format!("{} 已停用（風控設定未勾選）", ex.name())
}

/// The one muted line of a disabled exchange on a page, and what that page would have shown for it
/// (the error, the state, the quote row), which only the expanded line draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisabledNote {
    pub exchange: Exchange,
    pub text: String,
    pub detail: Option<String>,
}

impl DisabledNote {
    pub fn new(exchange: Exchange, detail: Option<String>) -> DisabledNote {
        DisabledNote { exchange, text: disabled_exchange_line(exchange), detail }
    }
}

#[cfg(test)]
#[path = "labels_tests.rs"]
mod tests;
