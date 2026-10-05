//! Risk configuration (capability risk-config): global fields, validation, per-exchange
//! overrides, and the pure "most conservative wins" merge for a pair.
//!
//! Unit convention: every `_pct` field is a percentage number (0.01 = 0.01%).

use std::collections::BTreeMap;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::types::{Decimal, Exchange, Pct};

/// Errors from validating or parsing risk configuration; always names the offending field.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RiskError {
    #[error("invalid value for `{field}`: {reason}")]
    InvalidValue { field: String, reason: String },
    #[error("`{field}` can only exist in the global config, not in a per-exchange override")]
    GlobalOnlyField { field: String },
    #[error("unknown risk field `{field}`")]
    UnknownField { field: String },
    #[error("malformed risk config: {0}")]
    Malformed(String),
}

impl RiskError {
    /// The field name this error is about, if any.
    pub fn field(&self) -> Option<&str> {
        match self {
            RiskError::InvalidValue { field, .. }
            | RiskError::GlobalOnlyField { field }
            | RiskError::UnknownField { field } => Some(field),
            RiskError::Malformed(_) => None,
        }
    }
}

/// Execution mode. There is deliberately no real-money `LIVE` mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionMode {
    #[serde(rename = "SIMULATION")]
    Simulation,
    #[serde(rename = "EXCHANGE_DEMO")]
    ExchangeDemo,
}

impl FromStr for ExecutionMode {
    type Err = RiskError;
    fn from_str(s: &str) -> Result<Self, RiskError> {
        match s {
            "SIMULATION" => Ok(ExecutionMode::Simulation),
            "EXCHANGE_DEMO" => Ok(ExecutionMode::ExchangeDemo),
            other => Err(invalid("execution_mode", format!("`{other}` is not SIMULATION or EXCHANGE_DEMO"))),
        }
    }
}

/// Whether qualifying pairs are opened automatically or only on user confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerMode {
    #[serde(rename = "AUTO")]
    Auto,
    #[serde(rename = "MANUAL")]
    Manual,
}

impl FromStr for TriggerMode {
    type Err = RiskError;
    fn from_str(s: &str) -> Result<Self, RiskError> {
        match s {
            "AUTO" => Ok(TriggerMode::Auto),
            "MANUAL" => Ok(TriggerMode::Manual),
            other => Err(invalid("trigger_mode", format!("`{other}` is not AUTO or MANUAL"))),
        }
    }
}

/// Global risk settings. Required-without-default fields are `Option` (`None` = not set yet).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RiskConfig {
    pub max_leverage: Decimal,
    pub max_concurrent_pairs: u32,
    pub max_price_drift_pct: Pct,
    pub stale_data_threshold_ms: u64,
    pub safety_margin_pct: Pct,
    pub order_timeout_seconds: u32,
    pub max_leg_imbalance_pct: Pct,
    pub min_24h_volume_usdt: Decimal,
    pub allowed_exchanges: Vec<Exchange>,
    /// Empty means "no restriction".
    pub allowed_coins: Vec<String>,
    pub execution_mode: ExecutionMode,
    pub trigger_mode: TriggerMode,
    /// Required, no default.
    pub net_edge_threshold_pct: Option<Pct>,
    /// Required, no default.
    pub est_slippage_pct: Option<Pct>,
    /// Required, no default; a missing exchange entry means "not set".
    pub taker_fee_pct: BTreeMap<Exchange, Pct>,
}

impl Default for RiskConfig {
    fn default() -> Self {
        RiskConfig {
            max_leverage: Decimal::from(5),
            max_concurrent_pairs: 3,
            max_price_drift_pct: Decimal::new(5, 2),
            stale_data_threshold_ms: 1000,
            safety_margin_pct: Decimal::new(1, 2),
            order_timeout_seconds: 15,
            max_leg_imbalance_pct: Decimal::new(10, 1),
            min_24h_volume_usdt: Decimal::from(50_000),
            allowed_exchanges: Exchange::ALL.to_vec(),
            allowed_coins: vec![],
            execution_mode: ExecutionMode::Simulation,
            trigger_mode: TriggerMode::Auto,
            net_edge_threshold_pct: None,
            est_slippage_pct: None,
            taker_fee_pct: BTreeMap::new(),
        }
    }
}

impl RiskConfig {
    /// Checks every field rule; the error names the first offending field.
    pub fn validate(&self) -> Result<(), RiskError> {
        positive("max_leverage", self.max_leverage)?;
        if self.max_concurrent_pairs < 1 {
            return Err(invalid("max_concurrent_pairs", "must be an integer >= 1"));
        }
        positive("max_price_drift_pct", self.max_price_drift_pct)?;
        if self.stale_data_threshold_ms == 0 {
            return Err(invalid("stale_data_threshold_ms", "must be > 0"));
        }
        non_negative("safety_margin_pct", self.safety_margin_pct)?;
        if self.order_timeout_seconds < 1 {
            return Err(invalid("order_timeout_seconds", "must be an integer >= 1"));
        }
        non_negative("max_leg_imbalance_pct", self.max_leg_imbalance_pct)?;
        non_negative("min_24h_volume_usdt", self.min_24h_volume_usdt)?;
        if self.allowed_exchanges.is_empty() {
            return Err(invalid("allowed_exchanges", "must contain at least one exchange"));
        }
        if let Some(v) = self.net_edge_threshold_pct {
            non_negative("net_edge_threshold_pct", v)?;
        }
        if let Some(v) = self.est_slippage_pct {
            non_negative("est_slippage_pct", v)?;
        }
        for v in self.taker_fee_pct.values() {
            non_negative("taker_fee_pct", *v)?;
        }
        Ok(())
    }

    /// Applies `f` to a copy and commits only if the result validates; otherwise `self` is untouched.
    pub fn try_update(&mut self, f: impl FnOnce(&mut RiskConfig)) -> Result<(), RiskError> {
        let mut candidate = self.clone();
        f(&mut candidate);
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Names of required fields still unset (e.g. `taker_fee_pct.Okx`).
    pub fn missing_fields(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.net_edge_threshold_pct.is_none() {
            out.push("net_edge_threshold_pct".to_string());
        }
        if self.est_slippage_pct.is_none() {
            out.push("est_slippage_pct".to_string());
        }
        for e in Exchange::ALL {
            if !self.taker_fee_pct.contains_key(&e) {
                out.push(format!("taker_fee_pct.{e:?}"));
            }
        }
        out
    }

    /// True when no required field is missing; Net Edge must not be used otherwise.
    pub fn is_complete(&self) -> bool {
        self.missing_fields().is_empty()
    }
}

/// Per-exchange overrides: only the nine overridable fields exist here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RiskOverride {
    pub max_leverage: Option<Decimal>,
    pub max_price_drift_pct: Option<Pct>,
    pub stale_data_threshold_ms: Option<u64>,
    pub order_timeout_seconds: Option<u32>,
    pub max_leg_imbalance_pct: Option<Pct>,
    pub min_24h_volume_usdt: Option<Decimal>,
    pub net_edge_threshold_pct: Option<Pct>,
    pub est_slippage_pct: Option<Pct>,
    pub safety_margin_pct: Option<Pct>,
}

const GLOBAL_ONLY_FIELDS: [&str; 5] =
    ["max_concurrent_pairs", "allowed_exchanges", "allowed_coins", "execution_mode", "trigger_mode"];
const OVERRIDABLE_FIELDS: [&str; 9] = [
    "max_leverage",
    "max_price_drift_pct",
    "stale_data_threshold_ms",
    "order_timeout_seconds",
    "max_leg_imbalance_pct",
    "min_24h_volume_usdt",
    "net_edge_threshold_pct",
    "est_slippage_pct",
    "safety_margin_pct",
];

impl RiskOverride {
    /// Parses one exchange's override object, rejecting global-only and unknown keys by name.
    pub fn from_json_value(value: &serde_json::Value) -> Result<RiskOverride, RiskError> {
        let obj = value
            .as_object()
            .ok_or_else(|| RiskError::Malformed("an override must be a JSON object".into()))?;
        for key in obj.keys() {
            if GLOBAL_ONLY_FIELDS.contains(&key.as_str()) {
                return Err(RiskError::GlobalOnlyField { field: key.clone() });
            }
            if !OVERRIDABLE_FIELDS.contains(&key.as_str()) {
                return Err(RiskError::UnknownField { field: key.clone() });
            }
        }
        serde_json::from_value(value.clone()).map_err(|e| RiskError::Malformed(e.to_string()))
    }

    /// Applies the same field rules as the global config; errors name `overrides.<Exchange>.<field>`.
    pub fn validate(&self, exchange: Exchange) -> Result<(), RiskError> {
        let name = |f: &str| format!("overrides.{exchange:?}.{f}");
        if let Some(v) = self.max_leverage {
            positive(&name("max_leverage"), v)?;
        }
        if let Some(v) = self.max_price_drift_pct {
            positive(&name("max_price_drift_pct"), v)?;
        }
        if self.stale_data_threshold_ms == Some(0) {
            return Err(invalid(&name("stale_data_threshold_ms"), "must be > 0"));
        }
        if self.order_timeout_seconds == Some(0) {
            return Err(invalid(&name("order_timeout_seconds"), "must be an integer >= 1"));
        }
        let non_neg = [
            ("max_leg_imbalance_pct", self.max_leg_imbalance_pct),
            ("min_24h_volume_usdt", self.min_24h_volume_usdt),
            ("net_edge_threshold_pct", self.net_edge_threshold_pct),
            ("est_slippage_pct", self.est_slippage_pct),
            ("safety_margin_pct", self.safety_margin_pct),
        ];
        for (f, v) in non_neg {
            if let Some(v) = v {
                non_negative(&name(f), v)?;
            }
        }
        Ok(())
    }
}

/// Overrides keyed by exchange; absent exchange = no override.
pub type RiskOverrides = BTreeMap<Exchange, RiskOverride>;

/// Parses and validates `{"Bybit": {...}, ...}`.
pub fn parse_overrides(value: &serde_json::Value) -> Result<RiskOverrides, RiskError> {
    let obj = value
        .as_object()
        .ok_or_else(|| RiskError::Malformed("overrides must be a JSON object keyed by exchange".into()))?;
    let mut out = RiskOverrides::new();
    for (key, v) in obj {
        let exchange: Exchange = serde_json::from_value(serde_json::Value::String(key.clone()))
            .map_err(|_| RiskError::Malformed(format!("unknown exchange `{key}`")))?;
        let o = RiskOverride::from_json_value(v)?;
        o.validate(exchange)?;
        out.insert(exchange, o);
    }
    Ok(out)
}

/// The settings in force for one pair (long leg + short leg), after conservative merging.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectiveConfig {
    pub max_leverage: Decimal,
    pub max_price_drift_pct: Pct,
    pub stale_data_threshold_ms: u64,
    pub order_timeout_seconds: u32,
    pub max_leg_imbalance_pct: Pct,
    pub min_24h_volume_usdt: Decimal,
    pub safety_margin_pct: Pct,
    /// `None` if unset for either leg.
    pub net_edge_threshold_pct: Option<Pct>,
    /// `None` if unset for either leg.
    pub est_slippage_pct: Option<Pct>,
    pub long_taker_fee_pct: Option<Pct>,
    pub short_taker_fee_pct: Option<Pct>,
}

impl EffectiveConfig {
    /// True when thresholds, slippage and both legs' fees are all set.
    pub fn is_complete(&self) -> bool {
        self.net_edge_threshold_pct.is_some()
            && self.est_slippage_pct.is_some()
            && self.long_taker_fee_pct.is_some()
            && self.short_taker_fee_pct.is_some()
    }
}

/// Pure merge: per field apply each leg's override (else global), then take the more conservative.
pub fn effective_for_pair(
    global: &RiskConfig,
    overrides: &RiskOverrides,
    long_exchange: Exchange,
    short_exchange: Exchange,
) -> EffectiveConfig {
    let legs = [long_exchange, short_exchange];
    let ov = |e: Exchange| overrides.get(&e);
    let min_of = |[a, b]: [Decimal; 2]| a.min(b);
    let max_of = |[a, b]: [Decimal; 2]| a.max(b);
    // Required fields: unset on either leg means unset for the pair (never silently 0).
    let leg_opt = |pick: fn(&RiskOverride) -> Option<Pct>, g: Option<Pct>| {
        match legs.map(|e| ov(e).and_then(pick).or(g)) {
            [Some(a), Some(b)] => Some(a.max(b)),
            _ => None,
        }
    };
    let dec = |pick: fn(&RiskOverride) -> Option<Decimal>, g: Decimal| legs.map(|e| ov(e).and_then(pick).unwrap_or(g));
    let leg_u64 = |pick: fn(&RiskOverride) -> Option<u64>, g: u64| legs.map(|e| ov(e).and_then(pick).unwrap_or(g));
    let leg_u32 = |pick: fn(&RiskOverride) -> Option<u32>, g: u32| legs.map(|e| ov(e).and_then(pick).unwrap_or(g));
    EffectiveConfig {
        max_leverage: min_of(dec(|o| o.max_leverage, global.max_leverage)),
        max_price_drift_pct: min_of(dec(|o| o.max_price_drift_pct, global.max_price_drift_pct)),
        stale_data_threshold_ms: leg_u64(|o| o.stale_data_threshold_ms, global.stale_data_threshold_ms)
            .into_iter()
            .min()
            .unwrap_or(global.stale_data_threshold_ms),
        order_timeout_seconds: leg_u32(|o| o.order_timeout_seconds, global.order_timeout_seconds)
            .into_iter()
            .min()
            .unwrap_or(global.order_timeout_seconds),
        max_leg_imbalance_pct: min_of(dec(|o| o.max_leg_imbalance_pct, global.max_leg_imbalance_pct)),
        min_24h_volume_usdt: max_of(dec(|o| o.min_24h_volume_usdt, global.min_24h_volume_usdt)),
        safety_margin_pct: max_of(dec(|o| o.safety_margin_pct, global.safety_margin_pct)),
        net_edge_threshold_pct: leg_opt(|o| o.net_edge_threshold_pct, global.net_edge_threshold_pct),
        est_slippage_pct: leg_opt(|o| o.est_slippage_pct, global.est_slippage_pct),
        long_taker_fee_pct: global.taker_fee_pct.get(&long_exchange).copied(),
        short_taker_fee_pct: global.taker_fee_pct.get(&short_exchange).copied(),
    }
}

fn invalid(field: &str, reason: impl Into<String>) -> RiskError {
    RiskError::InvalidValue { field: field.to_string(), reason: reason.into() }
}

fn positive(field: &str, v: Decimal) -> Result<(), RiskError> {
    if v > Decimal::ZERO {
        Ok(())
    } else {
        Err(invalid(field, "must be > 0"))
    }
}

fn non_negative(field: &str, v: Decimal) -> Result<(), RiskError> {
    if v >= Decimal::ZERO {
        Ok(())
    } else {
        Err(invalid(field, "must be >= 0"))
    }
}
