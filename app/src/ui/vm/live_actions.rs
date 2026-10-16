//! Live-only actions on pairs (live-guardrails 4.1 screen part, and single-leg flatten): the
//! "前往交易所" link buttons and the "平掉裸露的一腿" buttons with their confirmation. Pure: the
//! page only draws what is decided here and forwards the click. In a Demo process every function
//! here returns nothing, so Demo screens do not change.
//!
//! URLs come from `engine::exchange_links` (public domain + the pair only). Opening one is the
//! page's job (`cx.open_url`).

use tong_funding_core::pair::PairState;
use tong_funding_core::types::{Decimal, Exchange};

use super::bridge::{CommandSink, PairInfo, UiSnapshot};
use super::env_label::{ConfirmKind, EnvLabel};
use crate::engine::command::PairView;
use crate::engine::exchange_links::futures_url;
use crate::engine::ports::Leg;
use crate::identity::ProcessIdentity;

/// One "go to the exchange" button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkButton {
    pub label: String,
    pub url: String,
}

/// The buttons that open `symbol`'s futures page on each of the pair's exchanges (long first, no
/// duplicates). Empty outside Live and for an exchange without a live link (OKX).
pub fn exchange_links(identity: ProcessIdentity, symbol: &str, long: Exchange, short: Exchange) -> Vec<LinkButton> {
    if !identity.is_live() {
        return Vec::new();
    }
    let mut out: Vec<LinkButton> = Vec::new();
    for ex in [long, short] {
        if let Some(url) = futures_url(ex, symbol) {
            if !out.iter().any(|b| b.url == url) {
                out.push(LinkButton { label: format!("前往 {} 合約 {symbol}", ex.name()), url });
            }
        }
    }
    out
}

/// Links of a pair the snapshot knows.
pub fn pair_links(snap: &UiSnapshot, p: &PairInfo) -> Vec<LinkButton> {
    exchange_links(snap.identity, &p.symbol, p.long_exchange, p.short_exchange)
}

/// Links for a banner item about a pair (`source` = the pair id of a manual-attention alert).
pub fn banner_links(snap: &UiSnapshot, pair_id: &str) -> Vec<LinkButton> {
    snap.pairs.iter().find(|p| p.pair_id == pair_id).map(|p| pair_links(snap, p)).unwrap_or_default()
}

/// One "平掉 ..." button: a leg of a locked pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlattenButton {
    /// The engine's pair id (`FlattenLeg.pair`).
    pub pair: String,
    pub leg: Leg,
    pub exchange: Exchange,
    pub symbol: String,
    pub label: String,
}

fn side_word(leg: Leg) -> &'static str {
    match leg {
        Leg::Long => "多單（LONG）",
        Leg::Short => "空單（SHORT）",
    }
}

fn exchange_of(p: &PairView, leg: Leg) -> Exchange {
    match leg {
        Leg::Long => p.long_exchange,
        Leg::Short => p.short_exchange,
    }
}

fn locked(state: PairState) -> bool {
    matches!(state, PairState::PartialFailure | PairState::Imbalanced | PairState::Unresolved)
}

/// The flatten buttons of the engine pair with `pair_id` (the display id): one per leg, only in a
/// Live process and only while the pair is PARTIAL_FAILURE / IMBALANCED / UNRESOLVED.
pub fn flatten_buttons(snap: &UiSnapshot, pair_id: &str) -> Vec<FlattenButton> {
    let Some(p) = snap.engine.as_ref().and_then(|e| e.pairs.iter().find(|p| p.pair_id == pair_id)) else { return Vec::new() };
    flatten_buttons_of(snap.identity, p)
}

/// Same, found by the engine's own uuid (the manual-handling card knows only that).
pub fn flatten_buttons_by_uuid(snap: &UiSnapshot, uuid: &str) -> Vec<FlattenButton> {
    snap.engine.as_ref().and_then(|e| e.pairs.iter().find(|p| p.internal_uuid == uuid)).map(|p| flatten_buttons_of(snap.identity, p)).unwrap_or_default()
}

/// Same, for an engine pair already in hand.
pub fn flatten_buttons_of(identity: ProcessIdentity, p: &PairView) -> Vec<FlattenButton> {
    if !identity.is_live() || !locked(p.state) {
        return Vec::new();
    }
    Leg::BOTH
        .into_iter()
        .map(|leg| {
            let ex = exchange_of(p, leg);
            FlattenButton { pair: p.internal_uuid.clone(), leg, exchange: ex, symbol: p.symbol.clone(), label: format!("平掉 {} {} · {}", ex.name(), p.symbol, side_word(leg)) }
        })
        .collect()
}

/// Title above the flatten buttons.
pub const FLATTEN_TITLE: &str = "平掉裸露的一腿";
/// What the action does, stated before the click (reduce-only; the engine sizes it from the real position).
pub const FLATTEN_NOTE: &str = "只送一張 reduce-only 市價單，數量取交易所帳戶的實際持倉；不會平另一腿，也不會補單。平掉其中一腿後另一腿仍是單邊曝險，請到交易所頁面確認。";

/// The confirmation shown after the click, before anything is sent.
#[derive(Debug, Clone, PartialEq)]
pub struct FlattenConfirm {
    pub pair: String,
    pub leg: Leg,
    pub symbol: String,
    pub exchange: Exchange,
    pub env: EnvLabel,
    pub question: String,
    pub notional: Option<Decimal>,
}

impl FlattenConfirm {
    pub fn env_line(&self) -> String {
        self.env.confirm_env_line(self.notional)
    }

    /// "以真錢平倉" in a Live exchange pair.
    pub fn button(&self) -> &'static str {
        self.env.confirm_button(ConfirmKind::Close)
    }
}

/// Opens the confirmation for `b`; `None` when the pair is no longer locked (nothing is sent either way).
pub fn request_flatten(snap: &UiSnapshot, b: &FlattenButton) -> Option<FlattenConfirm> {
    let p = snap.engine.as_ref()?.pairs.iter().find(|p| p.internal_uuid == b.pair)?;
    if !snap.identity.is_live() || !locked(p.state) {
        return None;
    }
    let env = EnvLabel::of_pair(snap.identity, p.simulated);
    Some(FlattenConfirm {
        pair: b.pair.clone(),
        leg: b.leg,
        symbol: p.symbol.clone(),
        exchange: exchange_of(p, b.leg),
        env,
        question: format!("確認平掉 {} 的 {} {}？{FLATTEN_NOTE}", p.symbol, exchange_of(p, b.leg).name(), side_word(b.leg)),
        notional: None,
    })
}

/// After the confirmation: re-checks that the pair is still locked, then sends `FlattenLeg`.
pub fn send_flatten(snap: &UiSnapshot, c: &FlattenConfirm, sink: &dyn CommandSink) -> Result<(), String> {
    let still = snap.identity.is_live() && snap.engine.as_ref().and_then(|e| e.pairs.iter().find(|p| p.internal_uuid == c.pair)).is_some_and(|p| locked(p.state));
    if !still {
        return Err("此配對已不在 PARTIAL_FAILURE / IMBALANCED / UNRESOLVED，未送出".into());
    }
    sink.flatten_leg(format!("平掉 {} {}（{}）", c.exchange.name(), c.symbol, c.leg.as_str()), c.pair.clone(), c.leg);
    Ok(())
}

#[cfg(test)]
#[path = "live_actions_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "live_scope_tests.rs"]
mod scope_tests;
