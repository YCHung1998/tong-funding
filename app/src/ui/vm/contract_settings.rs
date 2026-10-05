//! Contract settings view-model (ui-trading-pages tasks 2.1, 2.2; spec contract-settings-page).
//! Pure, Decimal only. The stored values are the per-leg Target Notional and Leverage; margin is
//! derived. The quantity quote floors with `core::quantity` (OKX in contracts) and never shows a
//! quantity from a stale price or from missing rules.

use tong_funding_core::quantity::{Quantity, QuantityError};
use tong_funding_core::risk::ExecutionMode;
use tong_funding_core::types::{Decimal, Exchange};

use super::bridge::{CommandSink, ContractTemplate, Settings, UiSnapshot};
use super::format;
use crate::engine::command::Command;
use crate::engine::ports::OrderRules;

/// Which value the user types and which one is derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalcMode {
    /// Margin = Notional ÷ Leverage.
    LeverageToMargin,
    /// Leverage = Notional ÷ Margin.
    MarginToLeverage,
}

/// The raw inputs of the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractForm {
    pub notional: String,
    pub leverage: String,
    pub margin: String,
    pub mode: CalcMode,
}

impl ContractForm {
    pub fn from_template(t: &ContractTemplate) -> ContractForm {
        ContractForm {
            notional: t.notional_usdt.normalize().to_string(),
            leverage: t.leverage.normalize().to_string(),
            margin: t.margin().normalize().to_string(),
            mode: CalcMode::LeverageToMargin,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContractVm {
    pub notional: Option<Decimal>,
    pub leverage: Option<Decimal>,
    pub margin: Option<Decimal>,
    /// e.g. `1,200 ÷ 3 = 400 / leg`.
    pub formula: Option<String>,
    /// One LONG + SHORT pair: twice the per-leg values.
    pub pair_notional: Option<Decimal>,
    pub pair_margin: Option<Decimal>,
    pub errors: Vec<String>,
    /// Above the global or an exchange's `max_leverage`: shown, never blocks saving (Node 0 decides).
    pub leverage_warning: Option<String>,
    pub can_save: bool,
    /// The current `execution_mode` for the boundary table (`未知` while the engine is not running).
    pub mode_label: &'static str,
}

fn parse_positive(label: &str, raw: &str, errors: &mut Vec<String>) -> Option<Decimal> {
    match raw.trim().replace(',', "").parse::<Decimal>() {
        Err(_) => {
            errors.push(format!("{label} 不是有效的數字"));
            None
        }
        Ok(v) if v <= Decimal::ZERO => {
            errors.push(format!("{label} 必須大於 0"));
            None
        }
        Ok(v) => Some(v),
    }
}

fn mode_text(mode: Option<ExecutionMode>) -> &'static str {
    match mode {
        Some(ExecutionMode::Simulation) => "SIMULATION",
        Some(ExecutionMode::ExchangeDemo) => "EXCHANGE_DEMO",
        None => "未知",
    }
}

/// Evaluates the form. Pure.
pub fn evaluate(form: &ContractForm, settings: &Settings, mode: Option<ExecutionMode>) -> ContractVm {
    let mut errors = Vec::new();
    let notional = parse_positive("Target Notional", &form.notional, &mut errors);
    let (leverage, margin, formula) = match form.mode {
        CalcMode::LeverageToMargin => {
            let lev = parse_positive("Leverage", &form.leverage, &mut errors);
            let margin = notional.zip(lev).map(|(n, l)| n / l);
            let formula = notional.zip(lev).zip(margin).map(|((n, l), m)| format!("{} ÷ {} = {} / leg", format::compact(n), format::compact(l), format::compact(m)));
            (lev, margin, formula)
        }
        CalcMode::MarginToLeverage => {
            let margin = parse_positive("Margin", &form.margin, &mut errors);
            let lev = notional.zip(margin).map(|(n, m)| n / m);
            let formula = notional.zip(margin).zip(lev).map(|((n, m), l)| format!("{} ÷ {} = {}×", format::compact(n), format::compact(m), format::compact(l)));
            (lev, margin, formula)
        }
    };
    let leverage_warning = leverage.and_then(|l| {
        let mut over = Vec::new();
        if l > settings.risk.max_leverage {
            over.push(format!("全域 max_leverage {}", settings.risk.max_leverage.normalize()));
        }
        for (ex, o) in &settings.overrides {
            if let Some(m) = o.max_leverage.filter(|m| l > *m) {
                over.push(format!("{} max_leverage {}", ex.name(), m.normalize()));
            }
        }
        (!over.is_empty()).then(|| format!("槓桿 {} 超過 {}（送單前檢查會擋下）", l.normalize(), over.join("、")))
    });
    let two = Decimal::TWO;
    ContractVm {
        notional,
        leverage,
        margin,
        formula,
        pair_notional: notional.map(|n| n * two),
        pair_margin: margin.map(|m| m * two),
        can_save: errors.is_empty() && notional.is_some() && leverage.is_some(),
        errors,
        leverage_warning,
        mode_label: mode_text(mode),
    }
}

/// Sends `SaveContractTemplate` when the form is valid; `false` (nothing sent) otherwise.
pub fn save(vm: &ContractVm, sink: &dyn CommandSink) -> bool {
    match (vm.can_save, vm.notional, vm.leverage) {
        (true, Some(notional_usdt), Some(leverage)) => {
            sink.send("儲存合約模板".into(), Command::SaveContractTemplate { notional_usdt, leverage });
            true
        }
        _ => false,
    }
}

// ---- 2.2 quantity quote -----------------------------------------------------------------------

/// What one exchange's quantity cell shows.
#[derive(Debug, Clone, PartialEq)]
pub enum QuoteCell {
    /// Base-coin quantity floored to the step (Binance, Bybit).
    Qty { qty: Decimal, text: String },
    /// OKX: contracts floored to `lotSz`, with the base amount they stand for.
    Contracts { contracts: Decimal, base: Decimal, text: String },
    BelowMinimum,
    Stale { age_ms: i64 },
    NoPrice,
    NotListed,
    /// Rules could not be read (the reason is shown).
    NoRules(String),
    /// Rules not fetched yet.
    RulesPending,
}

impl QuoteCell {
    pub fn text(&self) -> String {
        match self {
            QuoteCell::Qty { text, .. } | QuoteCell::Contracts { text, .. } => text.clone(),
            QuoteCell::BelowMinimum => "低於最小下單量".into(),
            QuoteCell::Stale { .. } => "價格已過期".into(),
            QuoteCell::NoPrice => "無價格".into(),
            QuoteCell::NotListed => "未上架".into(),
            QuoteCell::NoRules(why) => format!("無法取得合約規格（{why}）"),
            QuoteCell::RulesPending => "合約規格載入中".into(),
        }
    }

    /// A sendable quantity (in the exchange's order unit).
    pub fn order_qty(&self) -> Option<Decimal> {
        match self {
            QuoteCell::Qty { qty, .. } => Some(*qty),
            QuoteCell::Contracts { contracts, .. } => Some(*contracts),
            QuoteCell::BelowMinimum
            | QuoteCell::Stale { .. }
            | QuoteCell::NoPrice
            | QuoteCell::NotListed
            | QuoteCell::NoRules(_)
            | QuoteCell::RulesPending => None,
        }
    }
}

/// The floored quantity of one leg at `price` (no freshness check: callers decide). Pure.
pub fn leg_quantity(exchange: Exchange, symbol: &str, notional: Decimal, price: Decimal, rules: Option<&Result<OrderRules, String>>) -> QuoteCell {
    let rules = match rules {
        None => return QuoteCell::RulesPending,
        Some(Err(why)) => return QuoteCell::NoRules(why.clone()),
        Some(Ok(r)) => r,
    };
    if price <= Decimal::ZERO {
        return QuoteCell::NoPrice;
    }
    let coin = format::base_coin(symbol);
    let below = |e: QuantityError| match e {
        QuantityError::BelowMinimum { .. } => QuoteCell::BelowMinimum,
        other => QuoteCell::NoRules(other.to_string()),
    };
    match exchange {
        Exchange::Okx => {
            let Some(ct_val) = rules.okx_ct_val else { return QuoteCell::NoRules("ctVal missing".into()) };
            match Quantity::okx_contracts(notional / price, ct_val, &rules.lot) {
                Ok(q) => {
                    let contracts = q.value();
                    let base = (contracts * ct_val).normalize();
                    QuoteCell::Contracts { contracts, base, text: format!("{} 張（≈ {} {coin}）", contracts.normalize(), base) }
                }
                Err(e) => below(e),
            }
        }
        Exchange::Binance | Exchange::Bybit => match Quantity::from_notional(notional, price, &rules.lot) {
            Ok(q) => QuoteCell::Qty { qty: q.value(), text: format!("{} {coin}", q.to_order_string(&rules.lot)) },
            Err(e) => below(e),
        },
    }
}

/// One exchange's quote row.
#[derive(Debug, Clone, PartialEq)]
pub struct ExchangeQuote {
    pub exchange: Exchange,
    pub price: Option<Decimal>,
    pub observed_at: Option<i64>,
    pub cell: QuoteCell,
}

/// The quantity quote of `symbol` for a per-leg `notional`, on every exchange. A price older than
/// `stale_data_threshold_ms` or missing rules give no quantity. Pure.
pub fn quote(symbol: &str, notional: Decimal, snap: &UiSnapshot, now_ms: i64) -> Vec<ExchangeQuote> {
    let symbol = symbol.trim().to_ascii_uppercase();
    let stale_ms = i64::try_from(snap.settings.risk.stale_data_threshold_ms).unwrap_or(i64::MAX);
    Exchange::ALL
        .into_iter()
        .map(|ex| {
            let o = snap.market.get(&ex).and_then(|f| f.observations.iter().find(|o| o.symbol == symbol));
            let Some(o) = o else { return ExchangeQuote { exchange: ex, price: None, observed_at: None, cell: QuoteCell::NoPrice } };
            let age = now_ms - o.observed_at;
            let cell = if !tong_funding_core::funding::is_consistent_listed(o) && o.data_status != tong_funding_core::funding::DataStatus::Listed {
                QuoteCell::NotListed
            } else if age > stale_ms {
                QuoteCell::Stale { age_ms: age }
            } else {
                leg_quantity(ex, &symbol, notional, o.mark_price, snap.rules.get(&(ex, symbol.clone())))
            };
            ExchangeQuote { exchange: ex, price: Some(o.mark_price), observed_at: Some(o.observed_at), cell }
        })
        .collect()
}

/// The page footnote (spec: the quote excludes slippage and fees).
pub const QUOTE_NOTE: &str = "試算未包含成交滑價與手續費；實際下單數量依各所 lot size 取整，並由送單前檢查以最新資料重新驗證。";

#[cfg(test)]
#[path = "contract_settings_tests.rs"]
mod tests;
