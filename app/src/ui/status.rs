//! Status bar model: identity, mode and the exchange connection dots. What the bar says about
//! trading (allowed or stopped and why, trigger, queue, breaker, pairs to handle) comes from
//! `vm::status_bar`.

use super::env_label::EnvLabel;
use super::nav::{Page, DEBUG_WARNING};
use crate::identity::ProcessIdentity;
use tong_funding_core::risk::ExecutionMode as CoreMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    Simulation,
    ExchangeDemo,
}

impl ExecutionMode {
    pub fn core(self) -> CoreMode {
        match self {
            ExecutionMode::Simulation => CoreMode::Simulation,
            ExecutionMode::ExchangeDemo => CoreMode::ExchangeDemo,
        }
    }

    /// The badge text: names the world (identity) and the mode (live-banners).
    pub fn label(self, identity: ProcessIdentity) -> &'static str {
        EnvLabel::of(identity, self.core()).env_name()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    Disconnected,
    Connected,
}

impl Connection {
    pub fn label(self) -> &'static str {
        match self {
            Connection::Disconnected => "未連線",
            Connection::Connected => "已連線",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusModel {
    pub identity: ProcessIdentity,
    pub mode: ExecutionMode,
    pub exchanges: Vec<(&'static str, Connection)>,
}

impl Default for StatusModel {
    /// Fail-safe first-launch state: SIMULATION, nothing connected.
    fn default() -> Self {
        StatusModel {
            identity: ProcessIdentity::Demo,
            mode: ExecutionMode::Simulation,
            exchanges: vec![
                ("Binance", Connection::Disconnected),
                ("Bybit", Connection::Disconnected),
                ("OKX", Connection::Disconnected),
            ],
        }
    }
}

/// Every user-visible string the shell can show in the given world, for the "no LIVE wording" check.
pub fn all_ui_strings(identity: ProcessIdentity) -> Vec<String> {
    let mut out = vec![EnvLabel::header_chip(identity).to_string(), DEBUG_WARNING.to_string()];
    for p in Page::ALL {
        out.push(p.zh().to_string());
        out.push(p.en().to_string());
    }
    for m in [ExecutionMode::Simulation, ExecutionMode::ExchangeDemo] {
        out.push(m.label(identity).to_string());
    }
    for c in [Connection::Disconnected, Connection::Connected] {
        out.push(c.label().to_string());
    }
    for (name, _) in StatusModel::default().exchanges {
        out.push(name.to_string());
    }
    out
}
