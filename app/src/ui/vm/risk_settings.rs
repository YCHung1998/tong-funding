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
use crate::engine::command::Command;

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
pub const GLOBAL_FIELDS: [Field; 14] = [
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
];

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
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Field::MaxLeverage => "×",
            Field::StaleDataThresholdMs => "ms",
            Field::MaxConcurrentPairs => "pairs",
            Field::OrderTimeoutSeconds => "秒",
            Field::Min24hVolumeUsdt => "USDT",
            Field::MaxPriceDriftPct
            | Field::MaxLegImbalancePct
            | Field::MinExpectedNetPnlPct
            | Field::NetEdgeThresholdPct
            | Field::EstSlippagePct
            | Field::SafetyMarginPct
            | Field::TakerFee(_) => "%",
        }
    }

    pub fn strictness(self) -> Strictness {
        match self {
            Field::NetEdgeThresholdPct | Field::MinExpectedNetPnlPct | Field::SafetyMarginPct | Field::EstSlippagePct | Field::Min24hVolumeUsdt => Strictness::HigherStricter,
            Field::MaxLeverage | Field::MaxPriceDriftPct | Field::StaleDataThresholdMs | Field::OrderTimeoutSeconds | Field::MaxLegImbalancePct | Field::MaxConcurrentPairs => Strictness::LowerStricter,
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
        }
    }

    /// Required without a default: blank = not set (the config is then incomplete).
    pub fn optional(self) -> bool {
        matches!(self, Field::NetEdgeThresholdPct | Field::EstSlippagePct | Field::TakerFee(_))
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
}

impl RiskForm {
    pub fn from_settings(s: &Settings) -> RiskForm {
        let values = GLOBAL_FIELDS.iter().map(|f| (*f, field_text(&s.risk, *f))).collect();
        let mut overrides = BTreeMap::new();
        for (ex, o) in &s.overrides {
            for f in OVERRIDE_FIELDS {
                if let Some(v) = override_value_of(o, f) {
                    overrides.insert((*ex, f), v);
                }
            }
        }
        RiskForm { values, overrides, allowed_exchanges: s.risk.allowed_exchanges.iter().copied().collect(), allowed_coins: s.risk.allowed_coins.join(", ") }
    }

    pub fn value(&self, f: Field) -> String {
        self.values.get(&f).cloned().unwrap_or_default()
    }

    pub fn set(&mut self, f: Field, text: &str) {
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
        Field::MaxConcurrentPairs | Field::MinExpectedNetPnlPct | Field::TakerFee(_) => None,
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
    cfg.missing_fields()
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
            _ => {
                if let Some(v) = parse_dec(f, &text, &mut errors, "") {
                    match (f, v) {
                        (Field::MaxLeverage, Some(v)) => cfg.max_leverage = v,
                        (Field::MaxPriceDriftPct, Some(v)) => cfg.max_price_drift_pct = v,
                        (Field::MaxLegImbalancePct, Some(v)) => cfg.max_leg_imbalance_pct = v,
                        (Field::Min24hVolumeUsdt, Some(v)) => cfg.min_24h_volume_usdt = v,
                        (Field::MinExpectedNetPnlPct, Some(v)) => cfg.min_expected_net_pnl_pct = v,
                        (Field::SafetyMarginPct, Some(v)) => cfg.safety_margin_pct = v,
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
                    Field::StaleDataThresholdMs | Field::OrderTimeoutSeconds | Field::MaxConcurrentPairs | Field::MinExpectedNetPnlPct | Field::TakerFee(_) => {}
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

    let missing = missing_display(&cfg);
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

// ---- 3.4 execution mode ---------------------------------------------------------------------

/// The only two modes the page offers (no LIVE).
pub const MODE_OPTIONS: [ExecutionMode; 2] = [ExecutionMode::Simulation, ExecutionMode::ExchangeDemo];

pub fn mode_label(m: ExecutionMode) -> &'static str {
    match m {
        ExecutionMode::Simulation => "SIMULATION",
        ExecutionMode::ExchangeDemo => "EXCHANGE_DEMO",
    }
}

pub fn mode_description(m: ExecutionMode) -> &'static str {
    match m {
        ExecutionMode::Simulation => "跑完整流程與送單前檢查，訂單由模擬器成交，不送到任何交易所；不涉及真錢。",
        ExecutionMode::ExchangeDemo => "對 demo / testnet 帳戶真實下單，會真的改變帳戶內的倉位；不涉及真錢。",
    }
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
pub fn request_mode(current: ExecutionMode, target: ExecutionMode, settings: &Settings, demo_keys: Option<&Result<(), String>>, sink: &dyn CommandSink) -> Result<ModeStep, String> {
    if current == target {
        return Ok(ModeStep::Unchanged);
    }
    match target {
        ExecutionMode::ExchangeDemo => {
            demo_option(settings, demo_keys)?;
            Ok(ModeStep::Confirm(target))
        }
        ExecutionMode::Simulation => {
            send_mode(target, sink);
            Ok(ModeStep::Sent)
        }
    }
}

/// After the confirmation: the engine switches (and records `EXECUTION_MODE_CHANGED`).
pub fn send_mode(target: ExecutionMode, sink: &dyn CommandSink) {
    sink.send(format!("切換模式 → {}", mode_label(target)), Command::SetExecutionMode(target));
}

#[cfg(test)]
#[path = "risk_settings_tests.rs"]
mod tests;
