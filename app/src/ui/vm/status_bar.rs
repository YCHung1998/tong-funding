//! The status bar at the bottom of every page (loss-circuit-breaker 3.1; spec app-shell): the
//! environment in plain Chinese, whether new entries are allowed (and why not), the trigger mode,
//! the queue with its countdown, the breaker numbers, the pairs that need a person, and the
//! exchange connection dots. Pure: it reads the snapshot and the time it is given; the shell only
//! draws the cells. Colours are roles (`Tone`), the shell maps them to theme tokens.

use super::alerts::{self, FreshStatus};
use super::bridge::{SourceId, UiSnapshot};
use super::env_label::EnvLabel;
use crate::ui::clock::read_clock;
use crate::ui::status::Connection;
use crate::ui::theme::Tone;
use tong_funding_core::risk::TriggerMode;
use tong_funding_core::types::Exchange;

/// One piece of text; `tone: None` = the bar's normal text colour.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub text: String,
    pub tone: Option<Tone>,
}

impl Cell {
    fn plain(text: impl Into<String>) -> Cell {
        Cell { text: text.into(), tone: None }
    }
    fn toned(text: impl Into<String>, tone: Tone) -> Cell {
        Cell { text: text.into(), tone: Some(tone) }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatusBarVm {
    /// 模擬 / Demo 帳戶 / 真錢.
    pub env: &'static str,
    /// 「交易：允許」 or 「交易：已停止（原因）」.
    pub trading: Cell,
    /// 「觸發：自動」 (reminder colour) or 「觸發：手動」.
    pub trigger: Cell,
    pub queue: Cell,
    pub breaker: Cell,
    /// 「需處理：N」, warning colour above 0.
    pub handling: Cell,
    pub exchanges: Vec<(&'static str, Connection)>,
}

/// `mm:ss` of a non-negative number of seconds (minutes may pass 59).
fn mmss(secs: i64) -> String {
    let s = secs.max(0);
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// "15:59:50 UTC" of an epoch millisecond.
fn utc_clock(ms: i64) -> String {
    format!("{} UTC", read_clock(ms.div_euclid(1000)).utc_time)
}

pub fn build(snap: &UiSnapshot, now_ms: i64) -> StatusBarVm {
    let exchanges = Exchange::ALL
        .into_iter()
        .map(|ex| {
            let online = snap.health_of(SourceId::MarketPoll(ex)).is_some_and(|h| alerts::source_status(h, now_ms) == FreshStatus::Online);
            (ex.name(), if online { Connection::Connected } else { Connection::Disconnected })
        })
        .collect();
    let env = EnvLabel::from_snapshot(snap).status_name();
    let (Some(engine), Some(status)) = (snap.engine.as_ref(), snap.engine_status.as_ref()) else {
        // Not running, or nothing published yet: nothing can trade, and nothing is guessed.
        return StatusBarVm {
            env,
            trading: Cell::plain("交易：已停止（引擎未運行）"),
            trigger: Cell::plain("觸發：—"),
            queue: Cell::plain("無排隊配對"),
            breaker: Cell::plain("熔斷：—"),
            handling: Cell::plain("需處理：—"),
            exchanges,
        };
    };
    let trading = match status.stop_reason {
        None if status.trading_allowed => Cell::plain("交易：允許"),
        Some(r) => Cell::plain(format!("交易：已停止（{}）", r.label())),
        // not allowed without a reason cannot happen; say it rather than show "允許"
        None => Cell::plain("交易：已停止"),
    };
    let trigger = match engine.trigger_mode {
        TriggerMode::Auto => Cell::toned("觸發：自動", Tone::Warning),
        TriggerMode::Manual => Cell::plain("觸發：手動"),
    };
    let queue = match (status.queued_pairs, status.next_entry_ms) {
        (0, _) => Cell::plain("無排隊配對"),
        (n, Some(at)) => Cell::plain(format!("排隊 {n} 對 · {} 進場（倒數 {}）", utc_clock(at), mmss((at - now_ms).div_euclid(1000)))),
        (n, None) => Cell::plain(format!("排隊 {n} 對")),
    };
    let breaker = match &status.breaker {
        Some(b) if b.tripped => Cell::toned("熔斷已觸發", Tone::Negative),
        Some(b) => Cell::plain(format!("熔斷：已虧 {:.2}／{} USDT · 連虧 {}／{}", b.loss_used, b.max_loss.normalize(), b.consecutive, b.max_consecutive)),
        None => Cell::plain("熔斷：—"),
    };
    let handling = if status.needs_handling > 0 { Cell::toned(format!("需處理：{}", status.needs_handling), Tone::Negative) } else { Cell::plain("需處理：0") };
    StatusBarVm { env, trading, trigger, queue, breaker, handling, exchanges }
}

#[cfg(test)]
#[path = "status_bar_tests.rs"]
mod tests;
