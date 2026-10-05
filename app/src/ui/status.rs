//! Status bar model. In this change connections are always "disconnected" and the
//! kill switch slot is always "not enabled"; later changes feed real values in.

use super::nav::{Page, DEBUG_WARNING};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    Simulation,
    ExchangeDemo,
}

impl ExecutionMode {
    pub fn label(self) -> &'static str {
        match self {
            ExecutionMode::Simulation => "SIMULATION",
            ExecutionMode::ExchangeDemo => "EXCHANGE_DEMO",
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillSwitch {
    Disabled,
    Halted,
}

impl KillSwitch {
    pub fn label(self) -> &'static str {
        match self {
            KillSwitch::Disabled => "未啟用",
            KillSwitch::Halted => "已停機",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusModel {
    pub mode: ExecutionMode,
    pub exchanges: Vec<(&'static str, Connection)>,
    pub kill_switch: KillSwitch,
}

impl Default for StatusModel {
    /// Fail-safe first-launch state: SIMULATION, nothing connected, kill switch slot idle.
    fn default() -> Self {
        StatusModel {
            mode: ExecutionMode::Simulation,
            exchanges: vec![
                ("Binance", Connection::Disconnected),
                ("Bybit", Connection::Disconnected),
                ("OKX", Connection::Disconnected),
            ],
            kill_switch: KillSwitch::Disabled,
        }
    }
}

pub const ENVIRONMENT_LABEL: &str = "Demo / Testnet";

/// Every user-visible string the shell can show, for the "no LIVE wording" check.
pub fn all_ui_strings() -> Vec<String> {
    let mut out = vec![ENVIRONMENT_LABEL.to_string(), DEBUG_WARNING.to_string()];
    for p in Page::ALL {
        out.push(p.zh().to_string());
        out.push(p.en().to_string());
    }
    for m in [ExecutionMode::Simulation, ExecutionMode::ExchangeDemo] {
        out.push(m.label().to_string());
    }
    for c in [Connection::Disconnected, Connection::Connected] {
        out.push(c.label().to_string());
    }
    for k in [KillSwitch::Disabled, KillSwitch::Halted] {
        out.push(k.label().to_string());
    }
    for (name, _) in StatusModel::default().exchanges {
        out.push(name.to_string());
    }
    out
}
