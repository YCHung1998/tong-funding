//! Risk settings view-model (ui-trading-pages tasks 3.1–3.4; spec risk-settings-page). Fields,
//! defaults, validation and the per-pair merge all come from `core::risk` (`RiskConfig::validate`,
//! `missing_fields`, `effective_for_pair`); this module only maps text inputs to them. A blank
//! required field is `None` (never 0). Saving sends one `SaveRiskSettings` command; the engine
//! stores both values with `RISK_CONFIG_UPDATED` in one transaction.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};
use tong_funding_core::risk::{effective_for_pair, EffectiveConfig, ExecutionMode, RiskConfig, RiskError, RiskOverride, RiskOverrides};
use tong_funding_core::types::{Decimal, Exchange};

use super::bridge::{CommandSink, Settings};
use super::env_label::EnvLabel;
use crate::identity::ProcessIdentity;
use crate::engine::command::Command;
use crate::engine::watchdog_text;

/// One editable number on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Field {
    MaxLeverage,
    MaxPriceDriftPct,
    StaleDataThresholdMs,
    MaxConcurrentPairs,
    OrderTimeoutSeconds,
    MaxLegImbalancePct,
    Min24hVolumeUsdt,
    MinExpectedNetPnlPct,
    NetEdgeThresholdPct,
    EstSlippagePct,
    SafetyMarginPct,
    TakerFee(Exchange),
    /// leg-early-exit: global only, 0 = disabled.
    EarlyExitMinMovePct,
    /// live-guardrails 4.2b: Live only (never shown or parsed by a Demo page); blank = not set.
    LiveMaxLegNotionalUsdt,
    LiveMaxPairsPerArming,
    /// loss-circuit-breaker: global only, with defaults (never blank).
    LossBreakerMaxLossUsdt,
    LossBreakerMaxConsecutiveLosses,
}

/// Which way a field tightens the risk gate. The source of truth is the conservative merge in
/// `core::risk::effective_for_pair` (max-merged = higher is stricter, min-merged = lower is
/// stricter); a test locks the two together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strictness {
    HigherStricter,
    LowerStricter,
    /// A fact to fill in truthfully, not a tightening knob (taker fee).
    Fact,
}

impl Strictness {
    /// Arrow text: shown next to the colour so the direction never relies on colour alone.
    pub fn badge(self) -> &'static str {
        match self {
            Strictness::HigherStricter => "▲ 越高越嚴",
            Strictness::LowerStricter => "▼ 越低越嚴",
            Strictness::Fact => "● 事實值",
        }
    }
}

/// Which judgement a field feeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Affect {
    Qualify,
    Pretrade,
    Execution,
}

impl Affect {
    pub fn label(self) -> &'static str {
        match self {
            Affect::Qualify => "達標",
            Affect::Pretrade => "送單前檢查",
            Affect::Execution => "執行",
        }
    }
}

/// Plain data shared by the help dialog and its tests.
#[derive(Debug, Clone, Copy)]
pub struct FieldHelp {
    pub meaning: &'static str,
    pub formula: Option<&'static str>,
    pub affects: &'static [Affect],
}

pub const NET_EDGE_FORMULA: &str = "net_edge_pct = 費率價差 − 2 × (taker_fee_L + taker_fee_S) − 4 × est_slippage − safety_margin";
pub const REQUIRED_SPREAD_FORMULA: &str = "所需費率價差 = 門檻 + 2 × (taker_fee_L + taker_fee_S) + 4 × est_slippage + safety_margin";
pub const QUALIFY_CONDITIONS: [&str; 4] = [
    "Net Edge ≥ net_edge_threshold_pct",
    "扣安全邊際前淨利 ≥ min_expected_net_pnl_pct",
    "兩腿 24h 成交量 ≥ min_24h_volume_usdt",
    "交易所與幣種在允許清單（allowed_exchanges / allowed_coins）",
];
pub const OVERRIDE_RULE: &str = "每腿覆寫：配對時取該配對兩腿（覆寫或全域值）中較嚴者——「越高越嚴」的欄位取較大值，「越低越嚴」的欄位取較小值；taker_fee_pct 不可覆寫。";
pub const STALE_NOTE: &str = "建議 3000 ms：送單前檢查用的是送單前剛抓的價格與費率，正常延遲約 0.1–0.5 秒、交易所慢時 1–2 秒；1000 容易因網路延遲誤擋，超過 5000 則送單時的價格可能已明顯偏離（另有 max_price_drift_pct 把關價格變動）。";

/// Global Limits, Layer 1 and Net Edge fields, in page order. Funding Threshold, Max Concurrent
/// Trades (legs), Hedge Threshold and a single Max Slippage do not exist.
pub const GLOBAL_FIELDS: [Field; 17] = [
    Field::MaxLeverage,
    Field::MaxPriceDriftPct,
    Field::StaleDataThresholdMs,
    Field::MaxConcurrentPairs,
    Field::OrderTimeoutSeconds,
    Field::MaxLegImbalancePct,
    Field::Min24hVolumeUsdt,
    Field::MinExpectedNetPnlPct,
    Field::NetEdgeThresholdPct,
    Field::EstSlippagePct,
    Field::SafetyMarginPct,
    Field::TakerFee(Exchange::Binance),
    Field::TakerFee(Exchange::Bybit),
    Field::TakerFee(Exchange::Okx),
    Field::EarlyExitMinMovePct,
    Field::LossBreakerMaxLossUsdt,
    Field::LossBreakerMaxConsecutiveLosses,
];

/// The two caps a Live process must fill in (live-guardrails 4.2b); global only, not overridable.
pub const LIVE_FIELDS: [Field; 2] = [Field::LiveMaxLegNotionalUsdt, Field::LiveMaxPairsPerArming];

/// The global fields the page of `identity` shows: Demo = `GLOBAL_FIELDS` unchanged, Live adds the caps.
pub fn global_fields(identity: ProcessIdentity) -> Vec<Field> {
    match identity {
        ProcessIdentity::Demo => GLOBAL_FIELDS.to_vec(),
        ProcessIdentity::Live => GLOBAL_FIELDS.iter().chain(LIVE_FIELDS.iter()).copied().collect(),
    }
}

/// Exactly the nine overridable fields of `risk-config` (`min_expected_net_pnl_pct` is global only).
pub const OVERRIDE_FIELDS: [Field; 9] = [
    Field::MaxLeverage,
    Field::MaxPriceDriftPct,
    Field::StaleDataThresholdMs,
    Field::OrderTimeoutSeconds,
    Field::MaxLegImbalancePct,
    Field::Min24hVolumeUsdt,
    Field::NetEdgeThresholdPct,
    Field::EstSlippagePct,
    Field::SafetyMarginPct,
];

impl Field {
    pub fn key(self) -> String {
        match self {
            Field::MaxLeverage => "max_leverage".into(),
            Field::MaxPriceDriftPct => "max_price_drift_pct".into(),
            Field::StaleDataThresholdMs => "stale_data_threshold_ms".into(),
            Field::MaxConcurrentPairs => "max_concurrent_pairs".into(),
            Field::OrderTimeoutSeconds => "order_timeout_seconds".into(),
            Field::MaxLegImbalancePct => "max_leg_imbalance_pct".into(),
            Field::Min24hVolumeUsdt => "min_24h_volume_usdt".into(),
            Field::MinExpectedNetPnlPct => "min_expected_net_pnl_pct".into(),
            Field::NetEdgeThresholdPct => "net_edge_threshold_pct".into(),
            Field::EstSlippagePct => "est_slippage_pct".into(),
            Field::SafetyMarginPct => "safety_margin_pct".into(),
            Field::TakerFee(e) => format!("{} taker_fee_pct", e.name()),
            Field::EarlyExitMinMovePct => "early_exit_min_move_pct".into(),
            Field::LiveMaxLegNotionalUsdt => "live_max_leg_notional_usdt".into(),
            Field::LiveMaxPairsPerArming => "live_max_pairs_per_arming".into(),
            Field::LossBreakerMaxLossUsdt => "loss_breaker_max_loss_usdt".into(),
            Field::LossBreakerMaxConsecutiveLosses => "loss_breaker_max_consecutive_losses".into(),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Field::MaxLeverage => "Max Leverage",
            Field::MaxPriceDriftPct => "最大價格漂移（每腿成交價相對基準價）",
            Field::StaleDataThresholdMs => "Stale Data Threshold",
            Field::MaxConcurrentPairs => "Max Concurrent Pairs",
            Field::OrderTimeoutSeconds => "Order Timeout",
            Field::MaxLegImbalancePct => "Max Leg Imbalance",
            Field::Min24hVolumeUsdt => "Min 24h Volume",
            Field::MinExpectedNetPnlPct => "Min Expected Net PnL %（扣手續費與滑價，未扣安全邊際）",
            Field::NetEdgeThresholdPct => "Net Edge 門檻（扣安全邊際後）",
            Field::EstSlippagePct => "估計滑價（每筆成交）",
            Field::SafetyMarginPct => "安全邊際",
            Field::TakerFee(_) => "Taker 費率",
            Field::EarlyExitMinMovePct => "提早平倉最小有利幅度（0 = 停用）",
            Field::LiveMaxLegNotionalUsdt => "每腿名目上限 USDT（Live，天花板 100）",
            Field::LiveMaxPairsPerArming => "每次武裝最多進場筆數（Live，天花板 5）",
            Field::LossBreakerMaxLossUsdt => "熔斷：累計虧損上限",
            Field::LossBreakerMaxConsecutiveLosses => "熔斷：連續虧損筆數上限",
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Field::MaxLeverage => "×",
            Field::StaleDataThresholdMs => "ms",
            Field::MaxConcurrentPairs => "pairs",
            Field::OrderTimeoutSeconds => "秒",
            Field::Min24hVolumeUsdt | Field::LiveMaxLegNotionalUsdt | Field::LossBreakerMaxLossUsdt => "USDT",
            Field::LiveMaxPairsPerArming | Field::LossBreakerMaxConsecutiveLosses => "筆",
            Field::MaxPriceDriftPct
            | Field::MaxLegImbalancePct
            | Field::MinExpectedNetPnlPct
            | Field::NetEdgeThresholdPct
            | Field::EstSlippagePct
            | Field::SafetyMarginPct
            | Field::EarlyExitMinMovePct
            | Field::TakerFee(_) => "%",
        }
    }

    pub fn strictness(self) -> Strictness {
        match self {
            Field::NetEdgeThresholdPct | Field::MinExpectedNetPnlPct | Field::SafetyMarginPct | Field::EstSlippagePct | Field::Min24hVolumeUsdt | Field::EarlyExitMinMovePct => Strictness::HigherStricter,
            Field::MaxLeverage | Field::MaxPriceDriftPct | Field::StaleDataThresholdMs | Field::OrderTimeoutSeconds | Field::MaxLegImbalancePct | Field::MaxConcurrentPairs | Field::LiveMaxLegNotionalUsdt | Field::LiveMaxPairsPerArming | Field::LossBreakerMaxLossUsdt | Field::LossBreakerMaxConsecutiveLosses => Strictness::LowerStricter,
            Field::TakerFee(_) => Strictness::Fact,
        }
    }

    pub fn help(self) -> FieldHelp {
        use Affect::*;
        const NE: Option<&str> = Some("net_edge_pct = 費率價差 − 2 × (taker_fee_L + taker_fee_S) − 4 × est_slippage − safety_margin；所需費率價差 = 門檻 + 2 × (taker_fee_L + taker_fee_S) + 4 × est_slippage + safety_margin");
        match self {
            Field::MaxLeverage => FieldHelp { meaning: "每腿允許的最大槓桿；合約設定的槓桿超過它，送單前檢查會擋下。", formula: None, affects: &[Pretrade, Execution] },
            Field::MaxPriceDriftPct => FieldHelp { meaning: "送單時每腿成交價相對基準價允許偏離的百分比；超過就不送單。", formula: None, affects: &[Pretrade, Execution] },
            Field::StaleDataThresholdMs => FieldHelp { meaning: "價格與費率資料的最大年齡（毫秒）；比它舊的資料視為過期，送單前檢查會擋下。", formula: None, affects: &[Pretrade] },
            Field::MaxConcurrentPairs => FieldHelp { meaning: "同時持有的配對組數上限；達上限後不再開新配對。", formula: None, affects: &[Pretrade] },
            Field::OrderTimeoutSeconds => FieldHelp { meaning: "單筆訂單等待成交的秒數；逾時視為未成交，由執行流程處理。", formula: None, affects: &[Execution] },
            Field::MaxLegImbalancePct => FieldHelp { meaning: "兩腿成交量差距允許的百分比；超過視為兩腿失衡，由執行流程處理。", formula: None, affects: &[Execution] },
            Field::Min24hVolumeUsdt => FieldHelp { meaning: "兩腿標的 24 小時成交額（USDT）的下限；流動性不足不達標。", formula: None, affects: &[Qualify, Pretrade] },
            Field::MinExpectedNetPnlPct => FieldHelp {
                meaning: "預期淨利（funding 收入 − 手續費 − 估計滑價，尚未扣安全邊際）占每腿名目本金的最低百分比；達標的第二道門檻。",
                formula: Some("預期淨利 = 費率價差 − 2 × (taker_fee_L + taker_fee_S) − 4 × est_slippage ≥ min_expected_net_pnl_pct"),
                affects: &[Qualify],
            },
            Field::NetEdgeThresholdPct => FieldHelp { meaning: "Net Edge（已扣手續費、滑價、安全邊際）至少要達到的百分比才算達標；沒有預設值，須自行填寫。", formula: NE, affects: &[Qualify, Pretrade] },
            Field::EstSlippagePct => FieldHelp {
                meaning: "每筆成交的估計滑價，四筆成交各估一次；這是估計值，估得越高，Net Edge 算得越低、越不容易達標，因此較保守。",
                formula: NE,
                affects: &[Qualify, Pretrade],
            },
            Field::SafetyMarginPct => FieldHelp { meaning: "從 Net Edge 額外扣掉的緩衝；越大越保守。", formula: NE, affects: &[Qualify, Pretrade] },
            Field::TakerFee(_) => FieldHelp { meaning: "該交易所的 taker 手續費率；請依帳戶手續費等級實填，不是調嚴調鬆的旋鈕。", formula: NE, affects: &[Qualify, Pretrade] },
            Field::EarlyExitMinMovePct => FieldHelp {
                meaning: "已對沖（RECONCILED）的配對在結算後 T+5 秒起、T+15 秒前，某一腿的價格相對自己的開倉成交均價往有利方向（多單漲、空單跌）移動達此百分比，就只平那一腿（reduce-only 市價單）、另一腿 T+15 照常平。僅 AUTO 且 kill switch 未啟用時動作；0 = 停用；等於門檻即觸發。單腿先平會暫時留下單邊曝險。",
                formula: Some("多單：(目前價 − 開倉均價) ÷ 開倉均價 × 100 ≥ 門檻；空單：(開倉均價 − 目前價) ÷ 開倉均價 × 100 ≥ 門檻"),
                affects: &[Execution],
            },
            Field::LiveMaxLegNotionalUsdt => FieldHelp {
                meaning: "Live 才有：每一腿名目（USDT）的硬上限；引擎在加入交易單、儲存合約模板、進場與每一筆非平倉單送出前都會檢查（數量 × 最差保護價，含 1% 緩衝，所以合約模板名目要設在上限的 99% 以下，例如上限 10 設 9）。內建天花板 100，空白＝未設定（未設定就無法武裝）；已武裝時只能調低。",
                formula: Some("數量 × max(基準價, 掃描價, 送單前價, 最新價) × 1.01 ≤ 上限"),
                affects: &[Pretrade, Execution],
            },
            Field::LiveMaxPairsPerArming => FieldHelp {
                meaning: "Live 才有：一次武裝最多進場的配對筆數；達到後自動鎖定，要再交易必須重新預檢與武裝。內建天花板 5，空白＝未設定（未設定就無法武裝）；已武裝時只能調低。",
                formula: None,
                affects: &[Pretrade, Execution],
            },
            Field::LossBreakerMaxLossUsdt => FieldHelp {
                meaning: "熔斷：自最近一次開放交易（kill switch 關閉，Live 為武裝）起，已平倉配對的累計虧損達此 USDT 數，就自動打開 kill switch，只停止開新倉、平倉與進行中的配對照常；Live 同時解除武裝。每筆金額用 PnL 淨額，淨額未知時用價格損益扣兩邊手續費（暫計，不含資金費）；金額算不出來也會觸發。預設 15（新的 Live 資料庫預設 5），越低越嚴。",
                formula: Some("區間內各筆金額加總 ≤ −上限 → 觸發；重新開放交易後重新計算"),
                affects: &[Execution],
            },
            Field::LossBreakerMaxConsecutiveLosses => FieldHelp {
                meaning: "熔斷：自最近一次開放交易起，結尾連續虧損（金額小於 0）的已平倉配對達此筆數，就自動打開 kill switch，只停止開新倉；獲利或持平的一筆會讓連虧歸零。預設 3（新的 Live 資料庫預設 2），越低越嚴。",
                formula: Some("結尾連續虧損筆數 ≥ 上限 → 觸發"),
                affects: &[Execution],
            },
        }
    }

    /// Required without a default: blank = not set (the config is then incomplete).
    pub fn optional(self) -> bool {
        matches!(self, Field::NetEdgeThresholdPct | Field::EstSlippagePct | Field::TakerFee(_) | Field::LiveMaxLegNotionalUsdt | Field::LiveMaxPairsPerArming)
    }
}

fn dtext(d: Decimal) -> String {
    d.normalize().to_string()
}

/// The text a field shows for `cfg` (blank when a required value is unset).
pub fn field_text(cfg: &RiskConfig, f: Field) -> String {
    match f {
        Field::MaxLeverage => dtext(cfg.max_leverage),
        Field::MaxPriceDriftPct => dtext(cfg.max_price_drift_pct),
        Field::StaleDataThresholdMs => cfg.stale_data_threshold_ms.to_string(),
        Field::MaxConcurrentPairs => cfg.max_concurrent_pairs.to_string(),
        Field::OrderTimeoutSeconds => cfg.order_timeout_seconds.to_string(),
        Field::MaxLegImbalancePct => dtext(cfg.max_leg_imbalance_pct),
        Field::Min24hVolumeUsdt => dtext(cfg.min_24h_volume_usdt),
        Field::MinExpectedNetPnlPct => dtext(cfg.min_expected_net_pnl_pct),
        Field::NetEdgeThresholdPct => cfg.net_edge_threshold_pct.map(dtext).unwrap_or_default(),
        Field::EstSlippagePct => cfg.est_slippage_pct.map(dtext).unwrap_or_default(),
        Field::SafetyMarginPct => dtext(cfg.safety_margin_pct),
        Field::TakerFee(e) => cfg.taker_fee_pct.get(&e).copied().map(dtext).unwrap_or_default(),
        Field::EarlyExitMinMovePct => dtext(cfg.early_exit_min_move_pct),
        Field::LiveMaxLegNotionalUsdt => cfg.live_max_leg_notional_usdt.map(dtext).unwrap_or_default(),
        Field::LiveMaxPairsPerArming => cfg.live_max_pairs_per_arming.map(|n| n.to_string()).unwrap_or_default(),
        Field::LossBreakerMaxLossUsdt => dtext(cfg.loss_breaker_max_loss_usdt),
        Field::LossBreakerMaxConsecutiveLosses => cfg.loss_breaker_max_consecutive_losses.to_string(),
    }
}

/// The page's inputs.
#[derive(Debug, Clone, PartialEq)]
pub struct RiskForm {
    values: BTreeMap<Field, String>,
    /// `Some(text)` = override on for that exchange and field.
    overrides: BTreeMap<(Exchange, Field), String>,
    pub allowed_exchanges: BTreeSet<Exchange>,
    /// Comma separated coins; empty = no restriction.
    pub allowed_coins: String,
    /// A Live page: the two caps are inputs (and parsed on evaluate). A Demo form ignores them, so a
    /// Demo save carries the stored caps through untouched.
    live: bool,
}

impl RiskForm {
    /// The Demo form (`GLOBAL_FIELDS` only).
    pub fn from_settings(s: &Settings) -> RiskForm {
        RiskForm::from_settings_for(s, ProcessIdentity::Demo)
    }

    pub fn from_settings_for(s: &Settings, identity: ProcessIdentity) -> RiskForm {
        let values = global_fields(identity).iter().map(|f| (*f, field_text(&s.risk, *f))).collect();
        let mut overrides = BTreeMap::new();
        for (ex, o) in &s.overrides {
            for f in OVERRIDE_FIELDS {
                if let Some(v) = override_value_of(o, f) {
                    overrides.insert((*ex, f), v);
                }
            }
        }
        RiskForm { values, overrides, allowed_exchanges: s.risk.allowed_exchanges.iter().copied().collect(), allowed_coins: s.risk.allowed_coins.join(", "), live: identity.is_live() }
    }

    pub fn value(&self, f: Field) -> String {
        self.values.get(&f).cloned().unwrap_or_default()
    }

    pub fn set(&mut self, f: Field, text: &str) {
        if LIVE_FIELDS.contains(&f) && !self.live {
            return;
        }
        self.values.insert(f, text.to_string());
    }

    /// Switch an override on (starting from the current global value) or off (removing it).
    pub fn set_override(&mut self, ex: Exchange, f: Field, on: bool, base: &Settings) {
        if !OVERRIDE_FIELDS.contains(&f) {
            return;
        }
        if on {
            let current = self.values.get(&f).cloned().unwrap_or_else(|| field_text(&base.risk, f));
            self.overrides.entry((ex, f)).or_insert(current);
        } else {
            self.overrides.remove(&(ex, f));
        }
    }

    pub fn set_override_value(&mut self, ex: Exchange, f: Field, text: &str) {
        if let Some(v) = self.overrides.get_mut(&(ex, f)) {
            *v = text.to_string();
        }
    }

    pub fn override_value(&self, ex: Exchange, f: Field) -> Option<String> {
        self.overrides.get(&(ex, f)).cloned()
    }
}

fn override_value_of(o: &RiskOverride, f: Field) -> Option<String> {
    match f {
        Field::MaxLeverage => o.max_leverage.map(dtext),
        Field::MaxPriceDriftPct => o.max_price_drift_pct.map(dtext),
        Field::StaleDataThresholdMs => o.stale_data_threshold_ms.map(|v| v.to_string()),
        Field::OrderTimeoutSeconds => o.order_timeout_seconds.map(|v| v.to_string()),
        Field::MaxLegImbalancePct => o.max_leg_imbalance_pct.map(dtext),
        Field::Min24hVolumeUsdt => o.min_24h_volume_usdt.map(dtext),
        Field::NetEdgeThresholdPct => o.net_edge_threshold_pct.map(dtext),
        Field::EstSlippagePct => o.est_slippage_pct.map(dtext),
        Field::SafetyMarginPct => o.safety_margin_pct.map(dtext),
        Field::MaxConcurrentPairs | Field::MinExpectedNetPnlPct | Field::EarlyExitMinMovePct | Field::TakerFee(_) | Field::LiveMaxLegNotionalUsdt | Field::LiveMaxPairsPerArming | Field::LossBreakerMaxLossUsdt | Field::LossBreakerMaxConsecutiveLosses => None,
    }
}

/// The generated effective settings of one exchange pair (same function as Node 0).
#[derive(Debug, Clone, PartialEq)]
pub struct PairPreview {
    pub long: Exchange,
    pub short: Exchange,
    pub effective: EffectiveConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RiskVm {
    pub result: Result<(RiskConfig, RiskOverrides), String>,
    /// Field key → message with the field name (e.g. `max_leverage 必須大於 0`).
    pub field_errors: BTreeMap<String, String>,
    /// Missing required fields, display names (`Bybit taker_fee_pct`).
    pub missing: Vec<String>,
    pub incomplete_text: Option<String>,
    pub can_save: bool,
    pub preview: Vec<PairPreview>,
    /// The Net Edge formula with the current values (`未設定` where missing, never 0).
    pub formula: String,
}

/// `taker_fee_pct.Bybit` → `Bybit taker_fee_pct` (the one completeness source: `missing_fields`).
pub fn missing_display(cfg: &RiskConfig) -> Vec<String> {
    display_missing(cfg.missing_fields())
}

/// The same for a Live page: also names the two caps while they are unset.
pub fn missing_display_live(cfg: &RiskConfig) -> Vec<String> {
    display_missing(cfg.missing_live_fields())
}

fn display_missing(fields: Vec<String>) -> Vec<String> {
    fields
        .into_iter()
        .map(|f| match f.strip_prefix("taker_fee_pct.") {
            Some(ex) => {
                let name = Exchange::ALL.into_iter().find(|e| format!("{e:?}") == ex).map(|e| e.name().to_string()).unwrap_or_else(|| ex.to_string());
                format!("{name} taker_fee_pct")
            }
            None => f,
        })
        .collect()
}

fn translate(reason: &str) -> String {
    match reason {
        "must be > 0" => "必須大於 0".into(),
        "must be >= 0" => "必須 ≥ 0".into(),
        "must be an integer >= 1" => "必須為 ≥ 1 的整數".into(),
        other => other.to_string(),
    }
}

fn error_text(e: &RiskError) -> (String, String) {
    match e {
        RiskError::InvalidValue { field, reason } => (field.clone(), format!("{field} {}", translate(reason))),
        RiskError::GlobalOnlyField { field } => (field.clone(), format!("{field} 只能存在於全域")),
        RiskError::UnknownField { field } => (field.clone(), format!("未知欄位 {field}")),
        RiskError::Malformed(m) => ("risk".into(), m.clone()),
    }
}

fn parse_dec(f: Field, text: &str, errors: &mut BTreeMap<String, String>, scope: &str) -> Option<Option<Decimal>> {
    let t = text.trim();
    if t.is_empty() {
        if f.optional() {
            return Some(None);
        }
        errors.insert(format!("{scope}{}", f.key()), format!("{scope}{} 必填", f.key()));
        return None;
    }
    match t.replace(',', "").parse::<Decimal>() {
        Ok(v) => Some(Some(v)),
        Err(_) => {
            errors.insert(format!("{scope}{}", f.key()), format!("{scope}{} 不是有效的數字", f.key()));
            None
        }
    }
}

fn parse_int<T: TryFrom<u64>>(f: Field, text: &str, errors: &mut BTreeMap<String, String>, scope: &str) -> Option<T> {
    match text.trim().parse::<u64>().ok().and_then(|v| T::try_from(v).ok()) {
        Some(v) => Some(v),
        None => {
            errors.insert(format!("{scope}{}", f.key()), format!("{scope}{} 必須為非負整數", f.key()));
            None
        }
    }
}

/// Evaluates the form against the stored settings (modes are kept as stored: they change only
/// through their own commands). Pure.
pub fn evaluate(form: &RiskForm, base: &Settings) -> RiskVm {
    let mut errors = BTreeMap::new();
    let mut cfg = base.risk.clone();
    for f in GLOBAL_FIELDS {
        let text = form.value(f);
        match f {
            Field::StaleDataThresholdMs => {
                if let Some(v) = parse_int::<u64>(f, &text, &mut errors, "") {
                    cfg.stale_data_threshold_ms = v;
                }
            }
            Field::MaxConcurrentPairs => {
                if let Some(v) = parse_int::<u32>(f, &text, &mut errors, "") {
                    cfg.max_concurrent_pairs = v;
                }
            }
            Field::OrderTimeoutSeconds => {
                if let Some(v) = parse_int::<u32>(f, &text, &mut errors, "") {
                    cfg.order_timeout_seconds = v;
                }
            }
            Field::LossBreakerMaxConsecutiveLosses => {
                if let Some(v) = parse_int::<u32>(f, &text, &mut errors, "") {
                    cfg.loss_breaker_max_consecutive_losses = v;
                }
            }
            _ => {
                if let Some(v) = parse_dec(f, &text, &mut errors, "") {
                    match (f, v) {
                        (Field::MaxLeverage, Some(v)) => cfg.max_leverage = v,
                        (Field::MaxPriceDriftPct, Some(v)) => cfg.max_price_drift_pct = v,
                        (Field::MaxLegImbalancePct, Some(v)) => cfg.max_leg_imbalance_pct = v,
                        (Field::Min24hVolumeUsdt, Some(v)) => cfg.min_24h_volume_usdt = v,
                        (Field::MinExpectedNetPnlPct, Some(v)) => cfg.min_expected_net_pnl_pct = v,
                        (Field::SafetyMarginPct, Some(v)) => cfg.safety_margin_pct = v,
                        (Field::EarlyExitMinMovePct, Some(v)) => cfg.early_exit_min_move_pct = v,
                        (Field::LossBreakerMaxLossUsdt, Some(v)) => cfg.loss_breaker_max_loss_usdt = v,
                        (Field::NetEdgeThresholdPct, v) => cfg.net_edge_threshold_pct = v,
                        (Field::EstSlippagePct, v) => cfg.est_slippage_pct = v,
                        (Field::TakerFee(e), Some(v)) => {
                            cfg.taker_fee_pct.insert(e, v);
                        }
                        (Field::TakerFee(e), None) => {
                            cfg.taker_fee_pct.remove(&e);
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    if form.live {
        for f in LIVE_FIELDS {
            let text = form.value(f);
            let t = text.trim();
            match f {
                Field::LiveMaxLegNotionalUsdt => {
                    if let Some(v) = parse_dec(f, &text, &mut errors, "") {
                        cfg.live_max_leg_notional_usdt = v;
                    }
                }
                Field::LiveMaxPairsPerArming if t.is_empty() => cfg.live_max_pairs_per_arming = None,
                Field::LiveMaxPairsPerArming => {
                    if let Some(v) = parse_int::<u32>(f, &text, &mut errors, "") {
                        cfg.live_max_pairs_per_arming = Some(v);
                    }
                }
                _ => {}
            }
        }
    }
    cfg.allowed_exchanges = Exchange::ALL.into_iter().filter(|e| form.allowed_exchanges.contains(e)).collect();
    cfg.allowed_coins = form.allowed_coins.split(',').map(|c| c.trim().to_ascii_uppercase()).filter(|c| !c.is_empty()).collect();
    if errors.is_empty()
        && let Err(e) = cfg.validate()
    {
        let (k, m) = error_text(&e);
        errors.insert(k, m);
    }

    let mut overrides = RiskOverrides::new();
    for ((ex, f), text) in &form.overrides {
        let scope = format!("overrides.{ex:?}.");
        let o = overrides.entry(*ex).or_default();
        match f {
            Field::StaleDataThresholdMs => o.stale_data_threshold_ms = parse_int::<u64>(*f, text, &mut errors, &scope),
            Field::OrderTimeoutSeconds => o.order_timeout_seconds = parse_int::<u32>(*f, text, &mut errors, &scope),
            _ => {
                // Overrides are never "unset": blank is an error, not a missing value.
                let v = if text.trim().is_empty() {
                    errors.insert(format!("{scope}{}", f.key()), format!("{scope}{} 必填", f.key()));
                    None
                } else {
                    parse_dec(*f, text, &mut errors, &scope).flatten()
                };
                match f {
                    Field::MaxLeverage => o.max_leverage = v,
                    Field::MaxPriceDriftPct => o.max_price_drift_pct = v,
                    Field::MaxLegImbalancePct => o.max_leg_imbalance_pct = v,
                    Field::Min24hVolumeUsdt => o.min_24h_volume_usdt = v,
                    Field::NetEdgeThresholdPct => o.net_edge_threshold_pct = v,
                    Field::EstSlippagePct => o.est_slippage_pct = v,
                    Field::SafetyMarginPct => o.safety_margin_pct = v,
                    Field::StaleDataThresholdMs | Field::OrderTimeoutSeconds | Field::MaxConcurrentPairs | Field::MinExpectedNetPnlPct | Field::EarlyExitMinMovePct | Field::TakerFee(_) | Field::LiveMaxLegNotionalUsdt | Field::LiveMaxPairsPerArming | Field::LossBreakerMaxLossUsdt | Field::LossBreakerMaxConsecutiveLosses => {}
                }
            }
        }
    }
    for (ex, o) in &overrides {
        if let Err(e) = o.validate(*ex) {
            let (k, m) = error_text(&e);
            errors.insert(k, m);
        }
    }

    let missing = if form.live { missing_display_live(&cfg) } else { missing_display(&cfg) };
    let incomplete_text = (!missing.is_empty()).then(|| format!("設定不完整：{}", missing.join("、")));
    let preview = [(Exchange::Binance, Exchange::Bybit), (Exchange::Binance, Exchange::Okx), (Exchange::Bybit, Exchange::Okx)]
        .into_iter()
        .map(|(l, s)| PairPreview { long: l, short: s, effective: effective_for_pair(&cfg, &overrides, l, s) })
        .collect();
    let formula = formula_text(&cfg);
    let ok = errors.is_empty();
    RiskVm {
        result: if ok { Ok((cfg, overrides)) } else { Err(errors.values().cloned().collect::<Vec<_>>().join("；")) },
        field_errors: errors,
        missing,
        incomplete_text,
        can_save: ok,
        preview,
        formula,
    }
}

fn formula_text(cfg: &RiskConfig) -> String {
    let opt = |v: Option<Decimal>| v.map(|d| format!("{}%", dtext(d))).unwrap_or_else(|| "未設定".into());
    let fees: Vec<String> = Exchange::ALL
        .into_iter()
        .map(|e| match cfg.taker_fee_pct.get(&e) {
            Some(v) => format!("{} {}", e.name(), dtext(*v)),
            None => format!("{} 未設定", e.name()),
        })
        .collect();
    format!(
        "預期 funding 收入 − 4 × 手續費（{}）− 4 × 估計滑價 {} − 安全邊際 {}% ≥ Net Edge 門檻 {}；另需預期淨收益（未扣安全邊際）≥ {}%。估計滑價以四筆成交各估一次。",
        fees.join(" / "),
        opt(cfg.est_slippage_pct),
        dtext(cfg.safety_margin_pct),
        opt(cfg.net_edge_threshold_pct),
        dtext(cfg.min_expected_net_pnl_pct)
    )
}

fn overrides_json(ov: &RiskOverrides) -> Value {
    let mut out = Map::new();
    for (ex, o) in ov {
        let mut m = Map::new();
        for f in OVERRIDE_FIELDS {
            if let Some(v) = override_value_of(o, f) {
                m.insert(f.key(), Value::String(v));
            }
        }
        if !m.is_empty() {
            out.insert(format!("{ex:?}"), Value::Object(m));
        }
    }
    Value::Object(out)
}

/// Sends `SaveRiskSettings` when everything validates; `false` (nothing sent) otherwise.
pub fn save(vm: &RiskVm, sink: &dyn CommandSink) -> bool {
    let Ok((cfg, ov)) = &vm.result else { return false };
    let Ok(risk) = serde_json::to_value(cfg) else { return false };
    sink.send("儲存風控設定".into(), Command::SaveRiskSettings { risk, overrides: overrides_json(ov) });
    true
}

// ---- loss-circuit-breaker 3.3: groups and unsaved changes -----------------------------------------

/// The page's blocks of fields (titles in Chinese). The "自動處理規則（授權 B）" block is not a group:
/// it holds no field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Net Edge threshold, minimum net profit, minimum 24h volume (and the allowed exchanges and coins, which are not `Field`s).
    Entry,
    /// Taker fees, estimated slippage, safety margin.
    Cost,
    /// Leverage, price drift, stale data, order timeout, leg imbalance, early exit.
    Protection,
    /// Concurrent pairs, the breaker, the Live caps.
    Position,
}

impl Group {
    pub const ALL: [Group; 4] = [Group::Entry, Group::Cost, Group::Protection, Group::Position];

    pub fn title(self) -> &'static str {
        match self {
            Group::Entry => "進場條件",
            Group::Cost => "成本估計",
            Group::Protection => "執行保護",
            Group::Position => "部位與熔斷",
        }
    }

    /// The fields of the block on the page of `identity`, in page order.
    pub fn fields(self, identity: ProcessIdentity) -> Vec<Field> {
        let mut out = match self {
            Group::Entry => vec![Field::NetEdgeThresholdPct, Field::MinExpectedNetPnlPct, Field::Min24hVolumeUsdt],
            Group::Cost => vec![Field::TakerFee(Exchange::Binance), Field::TakerFee(Exchange::Bybit), Field::TakerFee(Exchange::Okx), Field::EstSlippagePct, Field::SafetyMarginPct],
            Group::Protection => vec![
                Field::MaxLeverage,
                Field::MaxPriceDriftPct,
                Field::StaleDataThresholdMs,
                Field::OrderTimeoutSeconds,
                Field::MaxLegImbalancePct,
                Field::EarlyExitMinMovePct,
            ],
            Group::Position => vec![Field::MaxConcurrentPairs, Field::LossBreakerMaxLossUsdt, Field::LossBreakerMaxConsecutiveLosses],
        };
        if self == Group::Position && identity.is_live() {
            out.extend(LIVE_FIELDS);
        }
        out
    }
}

/// The same number written differently (`15` and ` 15.0 `) is the same value.
fn same_text(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    a == b || matches!((a.replace(',', "").parse::<Decimal>(), b.replace(',', "").parse::<Decimal>()), (Ok(x), Ok(y)) if x == y)
}

fn coin_set(text: &str) -> BTreeSet<String> {
    text.split(',').map(|c| c.trim().to_ascii_uppercase()).filter(|c| !c.is_empty()).collect()
}

/// What the form holds that differs from the STORED settings, one entry per setting: field keys in page
/// order, then `allowed_exchanges`, `allowed_coins`, then overrides (`overrides.<Exchange>.<field>`).
pub fn changed_items(form: &RiskForm, base: &Settings) -> Vec<String> {
    let identity = if form.live { ProcessIdentity::Live } else { ProcessIdentity::Demo };
    let mut out: Vec<String> = global_fields(identity).into_iter().filter(|f| !same_text(&form.value(*f), &field_text(&base.risk, *f))).map(|f| f.key()).collect();
    let stored_ex: BTreeSet<Exchange> = base.risk.allowed_exchanges.iter().copied().collect();
    if form.allowed_exchanges != stored_ex {
        out.push("allowed_exchanges".into());
    }
    if coin_set(&form.allowed_coins) != base.risk.allowed_coins.iter().map(|c| c.to_ascii_uppercase()).collect() {
        out.push("allowed_coins".into());
    }
    let mut keys: BTreeSet<(Exchange, Field)> = form.overrides.keys().copied().collect();
    for (ex, o) in &base.overrides {
        keys.extend(OVERRIDE_FIELDS.iter().filter(|f| override_value_of(o, **f).is_some()).map(|f| (*ex, *f)));
    }
    for (ex, f) in keys {
        let stored = base.overrides.get(&ex).and_then(|o| override_value_of(o, f));
        let changed = match (form.overrides.get(&(ex, f)), stored) {
            (None, None) => false,
            (Some(a), Some(b)) => !same_text(a, &b),
            _ => true,
        };
        if changed {
            out.push(format!("overrides.{ex:?}.{}", f.key()));
        }
    }
    out
}

/// The bar fixed at the bottom of the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveBar {
    /// 「有 N 項未儲存的變更」 or 「沒有未儲存的變更」.
    pub text: String,
    pub enabled: bool,
    /// Why the save button is off (shown next to it); `None` when it can be pressed.
    pub disabled_reason: Option<String>,
}

/// `changes` = `changed_items(..).len()`.
pub fn save_bar(vm: &RiskVm, changes: usize) -> SaveBar {
    let text = if changes == 0 { "沒有未儲存的變更".to_string() } else { format!("有 {changes} 項未儲存的變更") };
    let errors: Vec<&String> = vm.field_errors.values().collect();
    let disabled_reason = match (errors.as_slice(), changes) {
        ([], 0) => Some("沒有變更可儲存".to_string()),
        ([], _) => None,
        ([one], _) => Some(format!("{one}，修正後才能儲存")),
        (many, _) => Some(format!("有 {} 個欄位有錯誤，修正後才能儲存（{}）", many.len(), many[0])),
    };
    SaveBar { text, enabled: disabled_reason.is_none(), disabled_reason }
}

// ---- 3.4 execution mode ---------------------------------------------------------------------

/// The only two modes the page offers (no LIVE).
pub const MODE_OPTIONS: [ExecutionMode; 2] = [ExecutionMode::Simulation, ExecutionMode::ExchangeDemo];

pub fn mode_label(identity: ProcessIdentity, m: ExecutionMode) -> &'static str {
    EnvLabel::mode_option_name(identity, m)
}

pub fn mode_description(identity: ProcessIdentity, m: ExecutionMode) -> &'static str {
    EnvLabel::of(identity, m).mode_description()
}

/// Why `EXCHANGE_DEMO` cannot be chosen now (`Ok` = it can).
pub fn demo_option(settings: &Settings, demo_keys: Option<&Result<(), String>>) -> Result<(), String> {
    if let Some(e) = &settings.error {
        return Err(format!("設定不完整：風控設定讀取失敗（{e}）"));
    }
    let missing = missing_display(&settings.risk);
    if !missing.is_empty() {
        return Err(format!("設定不完整：{}", missing.join("、")));
    }
    match demo_keys {
        Some(Ok(())) => Ok(()),
        Some(Err(why)) => Err(format!("demo 金鑰不可用：{why}")),
        None => Err("demo 金鑰尚未確認".into()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeStep {
    Unchanged,
    /// Show the confirmation first (SIMULATION → EXCHANGE_DEMO); nothing sent yet.
    Confirm(ExecutionMode),
    /// Sent right away (back to SIMULATION).
    Sent,
}

/// The user picked `target`. `Err` = refused with the reason (nothing sent).
pub fn request_mode(identity: ProcessIdentity, current: ExecutionMode, target: ExecutionMode, settings: &Settings, demo_keys: Option<&Result<(), String>>, sink: &dyn CommandSink) -> Result<ModeStep, String> {
    if current == target {
        return Ok(ModeStep::Unchanged);
    }
    match target {
        ExecutionMode::ExchangeDemo => {
            demo_option(settings, demo_keys)?;
            Ok(ModeStep::Confirm(target))
        }
        ExecutionMode::Simulation => {
            send_mode(identity, target, sink);
            Ok(ModeStep::Sent)
        }
    }
}

/// After the confirmation: the engine switches (and records `EXECUTION_MODE_CHANGED`).
pub fn send_mode(identity: ProcessIdentity, target: ExecutionMode, sink: &dyn CommandSink) {
    sink.send(format!("切換模式 → {}", mode_label(identity, target)), Command::SetExecutionMode(target));
}

/// The fixed "自動處理規則（授權 B）" block of the page (naked-leg-watchdog): the rules the system acts
/// by on its own, always shown, with no way to collapse or switch it. Every word comes from
/// `engine::watchdog_text`, the one source shared with the manual-handling card and the notifications.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoRulesBlock {
    pub title: &'static str,
    /// The rule paragraph; bold runs are emphasised on the page.
    pub runs: Vec<watchdog_text::Run>,
    /// Lines under the paragraph: an exchange overrides the order timeout, or the stored settings
    /// could not be read (then the number above is the default, not a fact).
    pub notes: Vec<String>,
}

impl AutoRulesBlock {
    /// The paragraph without emphasis.
    pub fn plain(&self) -> String {
        watchdog_text::plain(&self.runs)
    }
}

/// The block for the STORED settings (what the engine acts by), not for unsaved edits in the form.
pub fn auto_rules_block(settings: &Settings) -> AutoRulesBlock {
    let mut notes = Vec::new();
    if let Some(e) = &settings.error {
        notes.push(format!("風控設定讀取失敗（{e}）：上面的訂單逾時秒數是預設值，實際值未知。"));
    }
    let overridden: Vec<(Exchange, u32)> = settings.overrides.iter().filter_map(|(ex, o)| o.order_timeout_seconds.map(|s| (*ex, s))).collect();
    notes.extend(watchdog_text::rules_override_note(&overridden));
    AutoRulesBlock { title: watchdog_text::RULES_TITLE, runs: watchdog_text::rules_runs(settings.risk.order_timeout_seconds), notes }
}

#[cfg(test)]
#[path = "risk_settings_tests.rs"]
mod tests;
