//! Single source of every environment wording (spec live-banners): the banner, window title,
//! badge, confirmation texts and every "which world am I in" sentence come from `(ProcessIdentity,
//! ExecutionMode)` through exhaustive `match`es (no `_` arm), so a new identity or mode makes the
//! compiler point at each place to update. Pure; the screen only renders what is decided here.
//!
//! Rule enforced by `ui::env_label::tests::environment_words_live_only_here`: no other non-test
//! file under `ui/` may contain a string literal with `EXCHANGE_DEMO`, `SIMULATION` or `LIVE`
//! (field names, enum variant names, comments and doc comments do not count).

use tong_funding_core::risk::{ExecutionMode, TriggerMode};
use tong_funding_core::types::Decimal;

use super::bridge::{KillSwitchState, UiSnapshot};
use super::format;
use crate::identity::ProcessIdentity;

/// Colour role of the banner; `shell.rs` maps it to the theme (Live and a loss share the red).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerTone {
    Live,
    Demo,
    Simulation,
}

/// What a confirmation is about; picks the button wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    /// Add to the trade list / enter / submit orders.
    Enter,
    /// Close / close now.
    Close,
    /// Switch the execution mode.
    ModeSwitch,
}

/// The world of the current moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvLabel {
    pub identity: ProcessIdentity,
    pub mode: ExecutionMode,
}

/// The fixed banner (no field can hide it: there is no visibility state at all).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvBanner {
    pub text: &'static str,
    pub tone: BannerTone,
    /// Live only: the kill switch state in words.
    pub kill_switch_text: Option<&'static str>,
}

impl EnvLabel {
    pub fn of(identity: ProcessIdentity, mode: ExecutionMode) -> EnvLabel {
        EnvLabel { identity, mode }
    }

    /// The world the snapshot lives in. A stopped engine reads as simulation (nothing can be sent).
    pub fn from_snapshot(snap: &UiSnapshot) -> EnvLabel {
        let mode = snap.engine.as_ref().map_or(ExecutionMode::Simulation, |e| e.execution_mode);
        EnvLabel { identity: snap.identity, mode }
    }

    /// The world of one pair: whether the pair is simulated is a fact recorded on the pair itself.
    pub fn of_pair(identity: ProcessIdentity, simulated: bool) -> EnvLabel {
        let mode = match simulated {
            true => ExecutionMode::Simulation,
            false => ExecutionMode::ExchangeDemo,
        };
        EnvLabel { identity, mode }
    }

    /// The chip next to the app name in the header.
    pub fn header_chip(identity: ProcessIdentity) -> &'static str {
        match identity {
            ProcessIdentity::Live => "LIVE",
            ProcessIdentity::Demo => "Demo / Testnet",
        }
    }

    pub fn banner_text(self) -> &'static str {
        match (self.identity, self.mode) {
            (ProcessIdentity::Live, ExecutionMode::ExchangeDemo) => "⚠ LIVE 真錢 · Binance + Bybit 正式環境",
            (ProcessIdentity::Live, ExecutionMode::Simulation) => "⚠ LIVE 模擬（不下單）· 不會送出任何真實訂單",
            (ProcessIdentity::Demo, ExecutionMode::ExchangeDemo) => "DEMO（模擬帳戶，不涉及真錢）",
            (ProcessIdentity::Demo, ExecutionMode::Simulation) => "模擬（不下單）",
        }
    }

    pub fn tone(self) -> BannerTone {
        match (self.identity, self.mode) {
            (ProcessIdentity::Live, ExecutionMode::ExchangeDemo) => BannerTone::Live,
            (ProcessIdentity::Live, ExecutionMode::Simulation) => BannerTone::Live,
            (ProcessIdentity::Demo, ExecutionMode::ExchangeDemo) => BannerTone::Demo,
            (ProcessIdentity::Demo, ExecutionMode::Simulation) => BannerTone::Simulation,
        }
    }

    /// Window title prefix; depends on the identity alone.
    pub fn title_prefix(identity: ProcessIdentity) -> &'static str {
        match identity {
            ProcessIdentity::Live => "[LIVE]",
            ProcessIdentity::Demo => "[DEMO]",
        }
    }

    pub fn window_title(identity: ProcessIdentity) -> String {
        format!("{} Funding Monitor", Self::title_prefix(identity))
    }

    /// `LIVE` badge on pairs, rows and log lines (Live identity only).
    pub fn badge(self) -> Option<&'static str> {
        match self.identity {
            ProcessIdentity::Live => Some("LIVE"),
            ProcessIdentity::Demo => None,
        }
    }

    /// The status bar's environment in plain Chinese: 模擬 / Demo 帳戶 / 真錢 (never the internal code).
    pub fn status_name(self) -> &'static str {
        match (self.identity, self.mode) {
            (ProcessIdentity::Live, ExecutionMode::ExchangeDemo) => "真錢",
            (ProcessIdentity::Live, ExecutionMode::Simulation) => "模擬",
            (ProcessIdentity::Demo, ExecutionMode::ExchangeDemo) => "Demo 帳戶",
            (ProcessIdentity::Demo, ExecutionMode::Simulation) => "模擬",
        }
    }

    /// Name of this world for notices and pair cards.
    pub fn env_name(self) -> &'static str {
        match (self.identity, self.mode) {
            (ProcessIdentity::Live, ExecutionMode::ExchangeDemo) => "LIVE",
            (ProcessIdentity::Live, ExecutionMode::Simulation) => "LIVE SIMULATION",
            (ProcessIdentity::Demo, ExecutionMode::ExchangeDemo) => "EXCHANGE_DEMO",
            (ProcessIdentity::Demo, ExecutionMode::Simulation) => "SIMULATION",
        }
    }

    /// Name of one execution mode as the mode selector shows it.
    pub fn mode_option_name(identity: ProcessIdentity, mode: ExecutionMode) -> &'static str {
        match (identity, mode) {
            (ProcessIdentity::Live, ExecutionMode::ExchangeDemo) => "LIVE",
            (ProcessIdentity::Live, ExecutionMode::Simulation) => "SIMULATION",
            (ProcessIdentity::Demo, ExecutionMode::ExchangeDemo) => "EXCHANGE_DEMO",
            (ProcessIdentity::Demo, ExecutionMode::Simulation) => "SIMULATION",
        }
    }

    pub fn mode_description(self) -> &'static str {
        match (self.identity, self.mode) {
            (ProcessIdentity::Live, ExecutionMode::ExchangeDemo) => "對 Binance / Bybit 正式環境真實下單，會動用真錢並改變帳戶內的倉位。",
            (ProcessIdentity::Live, ExecutionMode::Simulation) => "跑完整流程與送單前檢查，訂單由模擬器成交，不送到任何交易所。",
            (ProcessIdentity::Demo, ExecutionMode::ExchangeDemo) => "對 demo / testnet 帳戶真實下單，會真的改變帳戶內的倉位；不涉及真錢。",
            (ProcessIdentity::Demo, ExecutionMode::Simulation) => "跑完整流程與送單前檢查，訂單由模擬器成交，不送到任何交易所；不涉及真錢。",
        }
    }

    /// Staged-orders page sentence ("will / will not send real orders").
    pub fn staged_env_text(self) -> &'static str {
        match (self.identity, self.mode) {
            (ProcessIdentity::Live, ExecutionMode::ExchangeDemo) => "LIVE 真錢：將對 Binance / Bybit 正式環境真實下單",
            (ProcessIdentity::Live, ExecutionMode::Simulation) => "LIVE 模擬：由模擬器成交，不會送出真實訂單",
            (ProcessIdentity::Demo, ExecutionMode::ExchangeDemo) => "EXCHANGE_DEMO：將對 demo / testnet 帳戶真實下單",
            (ProcessIdentity::Demo, ExecutionMode::Simulation) => "SIMULATION：由模擬器成交，不會送出真實訂單",
        }
    }

    /// Manual-order page sentence.
    pub fn manual_env_text(self) -> &'static str {
        match (self.identity, self.mode) {
            (ProcessIdentity::Live, ExecutionMode::ExchangeDemo) => "LIVE 真錢：將對 Binance / Bybit 正式環境真實下單",
            (ProcessIdentity::Live, ExecutionMode::Simulation) => "LIVE 模擬：由模擬器成交，不會送到交易所",
            (ProcessIdentity::Demo, ExecutionMode::ExchangeDemo) => "EXCHANGE_DEMO：將對 demo / testnet 帳戶真實下單",
            (ProcessIdentity::Demo, ExecutionMode::Simulation) => "SIMULATION：由模擬器成交，不會送到交易所",
        }
    }

    /// Where the manual-order page reads positions / orders from.
    pub fn ledger_source(self) -> &'static str {
        match (self.identity, self.mode) {
            (ProcessIdentity::Live, ExecutionMode::ExchangeDemo) => "帳戶來源：LIVE 交易所帳戶（真錢）",
            (ProcessIdentity::Live, ExecutionMode::Simulation) => "帳戶來源：模擬帳本（LIVE 程式的模擬模式，不下單）",
            (ProcessIdentity::Demo, ExecutionMode::ExchangeDemo) => "帳戶來源：demo / testnet 交易所帳戶",
            (ProcessIdentity::Demo, ExecutionMode::Simulation) => "帳戶來源：模擬帳本",
        }
    }

    /// Does this world send orders to an exchange?
    pub fn is_exchange(self) -> bool {
        match self.mode {
            ExecutionMode::ExchangeDemo => true,
            ExecutionMode::Simulation => false,
        }
    }

    pub fn confirm_env_line(self, notional: Option<Decimal>) -> String {
        let env = match (self.identity, self.mode) {
            (ProcessIdentity::Live, ExecutionMode::ExchangeDemo) => "環境：LIVE 真錢",
            (ProcessIdentity::Live, ExecutionMode::Simulation) => "環境：LIVE 模擬（不下單）",
            (ProcessIdentity::Demo, ExecutionMode::ExchangeDemo) => "環境：DEMO",
            (ProcessIdentity::Demo, ExecutionMode::Simulation) => "環境：模擬（不下單）",
        };
        match notional {
            Some(n) => format!("{env} · 名目 {} USDT", format::money(n, 2)),
            None => format!("{env} · 名目 —"),
        }
    }

    pub fn confirm_button(self, kind: ConfirmKind) -> &'static str {
        match (self.identity, self.mode, kind) {
            (ProcessIdentity::Live, ExecutionMode::ExchangeDemo, ConfirmKind::Enter) => "以真錢下單",
            (ProcessIdentity::Live, ExecutionMode::ExchangeDemo, ConfirmKind::Close) => "以真錢平倉",
            (ProcessIdentity::Live, ExecutionMode::ExchangeDemo, ConfirmKind::ModeSwitch) => "確認切換到真錢模式",
            (ProcessIdentity::Live, ExecutionMode::Simulation, ConfirmKind::Enter) => "確認送出（LIVE 模擬，不下單）",
            (ProcessIdentity::Live, ExecutionMode::Simulation, ConfirmKind::Close) => "確認平倉（LIVE 模擬，不下單）",
            (ProcessIdentity::Live, ExecutionMode::Simulation, ConfirmKind::ModeSwitch) => "確認切換",
            (ProcessIdentity::Demo, ExecutionMode::ExchangeDemo, ConfirmKind::Enter) => "確認送出",
            (ProcessIdentity::Demo, ExecutionMode::ExchangeDemo, ConfirmKind::Close) => "確認平倉",
            (ProcessIdentity::Demo, ExecutionMode::ExchangeDemo, ConfirmKind::ModeSwitch) => "確認切換",
            (ProcessIdentity::Demo, ExecutionMode::Simulation, ConfirmKind::Enter) => "確認送出",
            (ProcessIdentity::Demo, ExecutionMode::Simulation, ConfirmKind::Close) => "確認平倉",
            (ProcessIdentity::Demo, ExecutionMode::Simulation, ConfirmKind::ModeSwitch) => "確認切換",
        }
    }

    /// The question asked before switching to `self.mode`.
    pub fn mode_switch_text(self) -> String {
        let target = Self::mode_option_name(self.identity, self.mode);
        match (self.identity, self.mode) {
            (ProcessIdentity::Live, ExecutionMode::ExchangeDemo) => format!("確認切換到 {target}？將對 Binance / Bybit 正式環境以真錢下單。既有倉位與 trigger_mode 不變。"),
            (ProcessIdentity::Live, ExecutionMode::Simulation) => format!("確認切換到 {target}？訂單改由模擬器成交，不再送到交易所。既有倉位與 trigger_mode 不變。"),
            (ProcessIdentity::Demo, ExecutionMode::ExchangeDemo) => format!("確認切換到 {target}？將對 demo / testnet 帳戶真實下單（不涉及真錢）。既有倉位與 trigger_mode 不變。"),
            (ProcessIdentity::Demo, ExecutionMode::Simulation) => format!("確認切換到 {target}？訂單改由模擬器成交，不再送到交易所。既有倉位與 trigger_mode 不變。"),
        }
    }

    pub fn auto_add_consequence(self, trigger: TriggerMode) -> String {
        match trigger {
            TriggerMode::Manual => "觸發模式 MANUAL：只會自動加入交易單，進場仍需到交易單頁手動按下".into(),
            TriggerMode::Auto => match (self.identity, self.mode) {
                (ProcessIdentity::Live, ExecutionMode::ExchangeDemo) => "觸發模式 AUTO + LIVE：達標配對會自動加入，並依排程自動進場、以真錢下單到 Binance / Bybit 正式環境".into(),
                (ProcessIdentity::Live, ExecutionMode::Simulation) => "觸發模式 AUTO + LIVE 模擬：達標配對會自動加入並由模擬器自動進場".into(),
                (ProcessIdentity::Demo, ExecutionMode::ExchangeDemo) => "觸發模式 AUTO + EXCHANGE_DEMO：達標配對會自動加入，並依排程自動進場、真實下單到 demo / testnet 帳戶".into(),
                (ProcessIdentity::Demo, ExecutionMode::Simulation) => "觸發模式 AUTO + SIMULATION：達標配對會自動加入並由模擬器自動進場".into(),
            },
        }
    }

    /// "<name> is disabled while the settings are incomplete".
    pub fn incomplete_settings_text(identity: ProcessIdentity) -> String {
        format!("設定不完整時交易單與 {} 會被禁用", Self::mode_option_name(identity, ExecutionMode::ExchangeDemo))
    }

    /// Why the exchange mode cannot be chosen.
    pub fn mode_unavailable_text(identity: ProcessIdentity, why: &str) -> String {
        format!("{} 不可選：{why}", Self::mode_option_name(identity, ExecutionMode::ExchangeDemo))
    }
}

impl EnvBanner {
    pub fn of_snapshot(snap: &UiSnapshot) -> EnvBanner {
        let l = EnvLabel::from_snapshot(snap);
        EnvBanner::build(l.identity, l.mode, snap.system.kill_switch)
    }

    pub fn build(identity: ProcessIdentity, mode: ExecutionMode, kill: KillSwitchState) -> EnvBanner {
        let label = EnvLabel::of(identity, mode);
        let kill_switch_text = match identity {
            ProcessIdentity::Live => Some(match kill {
                KillSwitchState::Off => "交易已武裝",
                // Unknown fails closed, like everywhere else.
                KillSwitchState::On | KillSwitchState::Unknown => "交易已鎖定（只能平倉）",
            }),
            ProcessIdentity::Demo => None,
        };
        EnvBanner { text: label.banner_text(), tone: label.tone(), kill_switch_text }
    }
}

#[cfg(test)]
#[path = "env_label_tests.rs"]
mod tests;
